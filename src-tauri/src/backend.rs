use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::archive::{self, Release};
use crate::config::{self, Config, VerifiedInstallation};
use crate::files::SafeRoot;
use crate::operations;
use crate::process;
use crate::types::{GameExited, GameStatus, LaunchResult, Operation, OperationFinished, OperationProgress, ReleaseSummary, Settings};
use crate::work::{Control, WorkError};

struct LaunchReservation {
    id: u64,
    until: Instant,
}

#[derive(Default)]
struct Inner {
    config: Option<Config>,
    operation: Option<Arc<Control>>,
    launch: Option<LaunchReservation>,
    next_launch: u64,
}

pub struct Backend {
    directory: Result<PathBuf, String>,
    inner: Mutex<Inner>,
    release: Mutex<Option<Arc<Release>>>,
}

impl Backend {
    pub fn new() -> Self {
        Self { directory: config::directory(), inner: Mutex::new(Inner::default()), release: Mutex::new(None) }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn config<'a>(&self, inner: &'a mut Inner) -> Result<&'a mut Config, String> {
        if inner.config.is_none() {
            inner.config = Some(config::load(self.directory.as_ref().map_err(Clone::clone)?)?);
        }
        inner.config.as_mut().ok_or_else(|| "Settings did not load".into())
    }

    fn idle(inner: &mut Inner) -> Result<(), String> {
        if inner.operation.is_some() {
            return Err("A verification or installation operation is already in progress".into());
        }
        if inner.launch.as_ref().is_some_and(|launch| Instant::now() < launch.until) {
            return Err("The game launch is still starting; wait a moment and refresh its status".into());
        }
        inner.launch = None;
        Ok(())
    }

    pub fn get_settings(&self) -> Result<Settings, String> {
        Ok(self.config(&mut self.inner())?.settings.clone())
    }

    pub fn save_settings(&self, settings: Settings) -> Result<Settings, String> {
        let mut inner = self.inner();
        Self::idle(&mut inner)?;
        let settings = config::validate(settings)?;
        let mut next = self.config(&mut inner)?.clone();
        if next.settings.game_dir != settings.game_dir {
            next.verified_installation = None;
        }
        next.settings = settings.clone();
        config::save(self.directory.as_ref().map_err(Clone::clone)?, &next)?;
        inner.config = Some(next);
        Ok(settings)
    }

    pub fn get_status(&self) -> Result<GameStatus, String> {
        let config = self.config(&mut self.inner())?.clone();
        let running = process::running(&config.settings)?;
        let installed = match SafeRoot::open(Path::new(&config.settings.game_dir), false).map_err(|error| error.to_string())? {
            Some(root) => root.open_file(Path::new("Arknights.exe")).map_err(|error| error.to_string())?.is_some(),
            None => false,
        };
        let verified_version = config.verified_installation
            .filter(|verified| verified.matches(&config.settings))
            .map(|verified| verified.version);
        let mut inner = self.inner();
        if running || inner.launch.as_ref().is_some_and(|launch| Instant::now() >= launch.until) {
            inner.launch = None;
        }
        Ok(GameStatus { installed, running, verified_version, settings: config.settings })
    }

    pub fn check_release(&self) -> Result<ReleaseSummary, String> {
        if self.inner().operation.is_some() {
            return Err("Wait for the current operation before checking another release".into());
        }
        let release = Arc::new(archive::fetch(Arc::new(Control::default())).map_err(|error| error.to_string())?);
        let summary = release.summary.clone();
        *self.release.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(release);
        Ok(summary)
    }

    pub fn release(&self, control: Arc<Control>) -> Result<Arc<Release>, WorkError> {
        control.check()?;
        if let Some(release) = self.release.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone() {
            return Ok(release);
        }
        let release = Arc::new(archive::fetch(control)?);
        *self.release.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(release.clone());
        Ok(release)
    }

    pub fn set_version(&self, settings: &Settings, version: Option<&str>) -> Result<(), WorkError> {
        let mut inner = self.inner();
        let mut config = self.config(&mut inner)?.clone();
        if config.settings != *settings {
            return Err("The selected installation changed during the operation".to_string().into());
        }
        config.verified_installation = version.map(|version| VerifiedInstallation::record(settings, version)).transpose()?;
        config::save(self.directory.as_ref().map_err(Clone::clone)?, &config)?;
        inner.config = Some(config);
        Ok(())
    }

    pub fn start(self: &Arc<Self>, app: AppHandle, operation: Operation) -> Result<(), String> {
        let mut inner = self.inner();
        Self::idle(&mut inner)?;
        let settings = self.config(&mut inner)?.settings.clone();
        if operation == Operation::Install && process::running(&settings)? {
            return Err("Arknights is running from this installation or prefix; close it before installing or repairing".into());
        }
        let control = Arc::new(Control::default());
        inner.operation = Some(control.clone());
        drop(inner);
        let backend = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = app.emit("operation-progress", OperationProgress {
                    operation,
                    phase: "metadata".into(),
                    completed_bytes: 0,
                    total_bytes: 0,
                    completed_files: 0,
                    total_files: 0,
                    current_file: String::new(),
                });
                operations::run(&backend, &app, operation, &settings, control.clone())
            })).unwrap_or_else(|panic| {
                let message = panic.downcast_ref::<&str>().copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown worker panic");
                Err(WorkError::Failed(format!("The operation worker failed: {message}")))
            });
            let finished = match result {
                Ok(result) => OperationFinished { operation, cancelled: false, summary: Some(result.summary), version: result.version, error: None },
                Err(WorkError::Cancelled) => OperationFinished { operation, cancelled: true, summary: None, version: None, error: None },
                Err(error) => OperationFinished { operation, cancelled: false, summary: None, version: None, error: Some(error.to_string()) },
            };
            let mut inner = backend.inner();
            if inner.operation.as_ref().is_some_and(|active| Arc::ptr_eq(active, &control)) {
                inner.operation = None;
            }
            drop(inner);
            if let Err(error) = app.emit("operation-finished", finished) {
                eprintln!("Cannot deliver operation completion: {error}");
            }
        });
        Ok(())
    }

    pub fn cancel(&self) {
        if let Some(control) = &self.inner().operation {
            control.cancelled.store(true, Ordering::Release);
        }
    }

    pub fn launch(self: &Arc<Self>, app: AppHandle) -> Result<LaunchResult, String> {
        let mut inner = self.inner();
        if inner.operation.is_some() {
            return Err("Wait for verification or installation before launching".into());
        }
        let settings = self.config(&mut inner)?.settings.clone();
        if process::running(&settings)? {
            inner.launch = None;
            return Ok(LaunchResult { already_running: true });
        }
        Self::idle(&mut inner)?;
        let mut child = process::spawn(&settings)?;
        inner.next_launch = inner.next_launch.wrapping_add(1);
        let id = inner.next_launch;
        // Reserve only the short startup gap, never the lifetime of proton or
        // steam.exe wrappers. Actual running status always comes from /proc.
        inner.launch = Some(LaunchReservation { id, until: Instant::now() + Duration::from_secs(15) });
        drop(inner);
        let backend = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let exit_code = match child.wait() {
                Ok(status) => status.code(),
                Err(error) => {
                    eprintln!("Cannot wait for the Proton process: {error}");
                    None
                }
            };
            let mut inner = backend.inner();
            if inner.launch.as_ref().is_some_and(|launch| launch.id == id) {
                inner.launch = None;
            }
            drop(inner);
            if let Err(error) = app.emit("game-exited", GameExited { exit_code }) {
                eprintln!("Cannot deliver game exit: {error}");
            }
        });
        Ok(LaunchResult { already_running: false })
    }
}
