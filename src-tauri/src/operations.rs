use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::archive::Release;
use crate::backend::Backend;
use crate::files::SafeRoot;
use crate::process::MutationGuard;
use crate::types::{Operation, OperationProgress, Settings, VerificationSummary};
use crate::work::{Control, WorkError};

const BUFFER_BYTES: usize = 128 * 1024;
const INSTALL_WORKERS: usize = 4;

pub struct Outcome {
    pub summary: VerificationSummary,
    pub version: Option<String>,
}

struct Scan {
    summary: VerificationSummary,
    changed: Vec<usize>,
    root: Option<SafeRoot>,
}

pub fn run(
    backend: &Backend,
    app: &AppHandle,
    operation: Operation,
    settings: &Settings,
    control: Arc<Control>,
) -> Result<Outcome, WorkError> {
    let release = backend.release(control.clone())?;
    let progress = Reporter::new(app, operation, "verifying", release.summary.installed_bytes, release.entries.len());
    let scan = verify(&release, settings, &control, &progress)?;
    control.check()?;
    if operation == Operation::Verify {
        let complete = scan.changed.is_empty();
        if let Some(root) = &scan.root {
            root.check_selected(Path::new(&settings.game_dir))?;
        }
        let version = complete.then(|| release.summary.version.clone());
        backend.set_version(settings, version.as_deref())?;
        return Ok(Outcome { summary: scan.summary, version });
    }
    // Verification is read-only, but installation must recheck after that
    // potentially long phase before creating directories or temporary files.
    let guard = MutationGuard::new(settings)?;
    if !scan.changed.is_empty() {
        backend.set_version(settings, None)?;
    }
    control.check()?;
    guard.check()?;
    let root = match scan.root {
        Some(root) => root,
        None => SafeRoot::open(Path::new(&settings.game_dir), true)?.ok_or("Cannot open the created game directory".to_string())?,
    };
    root.check_selected(Path::new(&settings.game_dir))?;
    for directory in &release.directories {
        control.check()?;
        guard.check()?;
        root.ensure_directory(directory)?;
    }
    if !scan.changed.is_empty() {
        let bytes = scan.changed.iter().map(|index| release.entries[*index].size).sum();
        let progress = Reporter::new(app, operation, "installing", bytes, scan.changed.len());
        install_changed(&release, &scan.changed, &root, &guard, &control, &progress)?;
    }
    control.check()?;
    root.check_selected(Path::new(&settings.game_dir))?;
    backend.set_version(settings, Some(&release.summary.version))?;
    Ok(Outcome {
        summary: VerificationSummary {
            version: release.summary.version.clone(),
            files_checked: release.entries.len(),
            missing_files: 0,
            changed_files: 0,
            verified_bytes: release.summary.installed_bytes,
        },
        version: Some(release.summary.version.clone()),
    })
}

fn verify(release: &Release, settings: &Settings, control: &Control, progress: &Reporter<'_>) -> Result<Scan, WorkError> {
    let root = SafeRoot::open(Path::new(&settings.game_dir), false)?;
    let mut summary = VerificationSummary {
        version: release.summary.version.clone(),
        files_checked: 0,
        missing_files: 0,
        changed_files: 0,
        verified_bytes: 0,
    };
    let mut changed = Vec::new();
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    for (index, entry) in release.entries.iter().enumerate() {
        control.check()?;
        progress.current(&entry.path);
        let file = match &root {
            Some(root) => root.open_file(&entry.path)?,
            None => None,
        };
        match file {
            None => {
                summary.missing_files += 1;
                changed.push(index);
            }
            Some(mut file) => {
                if file.metadata()?.len() != entry.size {
                    summary.changed_files += 1;
                    changed.push(index);
                } else {
                    let mut hash = crc32fast::Hasher::new();
                    let mut read = 0_u64;
                    loop {
                        control.check()?;
                        let count = file.read(&mut buffer)
                            .map_err(|error| format!("Cannot verify {}: {error}", entry.path.display()))?;
                        if count == 0 { break; }
                        read += count as u64;
                        hash.update(&buffer[..count]);
                        progress.bytes(count as u64);
                        if read > entry.size { break; }
                    }
                    if read == entry.size && hash.finalize() == entry.crc32 {
                        summary.verified_bytes += entry.size;
                    } else {
                        summary.changed_files += 1;
                        changed.push(index);
                    }
                }
            }
        }
        summary.files_checked += 1;
        progress.file();
    }
    progress.finish();
    Ok(Scan { summary, changed, root })
}

fn install_changed(
    release: &Release,
    changed: &[usize],
    root: &SafeRoot,
    guard: &MutationGuard,
    control: &Arc<Control>,
    progress: &Reporter<'_>,
) -> Result<(), WorkError> {
    let next = AtomicUsize::new(0);
    let first_error = Mutex::new(None);
    std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(INSTALL_WORKERS.min(changed.len()));
        for _ in 0..INSTALL_WORKERS.min(changed.len()) {
            let next = &next;
            let first_error = &first_error;
            workers.push(scope.spawn(move || {
                let result = (|| {
                    let (mut archive, limit) = release.archive(control.clone())?;
                    let mut buffer = vec![0_u8; BUFFER_BYTES];
                    loop {
                        if control.stopped.load(Ordering::Acquire) { return Ok(()); }
                        control.check()?;
                        let job = next.fetch_add(1, Ordering::Relaxed);
                        let Some(index) = changed.get(job) else { return Ok(()) };
                        let entry = &release.entries[*index];
                        guard.check()?;
                        progress.current(&entry.path);
                        // Read exactly the fixed local header first. Once zip
                        // has resolved its variable fields, permit only this
                        // changed entry's compressed payload, not its neighbors.
                        limit.store(entry.header_start + 30, Ordering::Release);
                        let mut file = archive.by_index(entry.index)
                            .map_err(|error| control.error(format!("Cannot open {} in the ZIP: {error}", entry.path.display())))?;
                        if file.size() != entry.size || file.compressed_size() != entry.compressed_size || file.crc32() != entry.crc32 {
                            return Err(WorkError::Failed("ZIP local and central metadata disagree".into()));
                        }
                        let end = file.data_start().checked_add(entry.compressed_size)
                            .filter(|end| *end <= release.metadata.start)
                            .ok_or("ZIP file payload overlaps its central directory".to_string())?;
                        limit.store(end, Ordering::Release);
                        root.replace(entry, &mut file, &mut buffer, control, |bytes| progress.bytes(bytes), || guard.check())?;
                        progress.file();
                    }
                })();
                if let Err(error) = result {
                    let mut first = first_error.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if first.is_none() { *first = Some(error); }
                    control.stopped.store(true, Ordering::Release);
                }
            }));
        }
        for worker in workers {
            if worker.join().is_err() {
                let mut first = first_error.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if first.is_none() { *first = Some(WorkError::Failed("An extraction worker panicked".into())); }
                control.stopped.store(true, Ordering::Release);
            }
        }
    });
    if let Some(error) = first_error.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner()) {
        return Err(error);
    }
    control.check()?;
    progress.finish();
    Ok(())
}

struct Progress {
    payload: OperationProgress,
    emitted: Instant,
}

struct Reporter<'a> {
    app: &'a AppHandle,
    progress: Mutex<Progress>,
}

impl<'a> Reporter<'a> {
    fn new(app: &'a AppHandle, operation: Operation, phase: &str, total_bytes: u64, total_files: usize) -> Self {
        let payload = OperationProgress {
            operation,
            phase: phase.into(),
            completed_bytes: 0,
            total_bytes,
            completed_files: 0,
            total_files,
            current_file: String::new(),
        };
        let _ = app.emit("operation-progress", &payload);
        Self { app, progress: Mutex::new(Progress { payload, emitted: Instant::now() }) }
    }

    fn current(&self, path: &Path) {
        let mut progress = self.progress.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        progress.payload.current_file.clear();
        progress.payload.current_file.push_str(&path.to_string_lossy());
    }

    fn bytes(&self, bytes: u64) {
        let mut progress = self.progress.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        progress.payload.completed_bytes += bytes;
        self.emit(&mut progress, false);
    }

    fn file(&self) {
        let mut progress = self.progress.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        progress.payload.completed_files += 1;
        self.emit(&mut progress, false);
    }

    fn finish(&self) {
        self.emit(&mut self.progress.lock().unwrap_or_else(|poisoned| poisoned.into_inner()), true);
    }

    fn emit(&self, progress: &mut Progress, force: bool) {
        if force || progress.emitted.elapsed() >= Duration::from_millis(100) {
            let _ = self.app.emit("operation-progress", &progress.payload);
            progress.emitted = Instant::now();
        }
    }
}
