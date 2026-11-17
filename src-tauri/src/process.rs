use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config;
use crate::files::SafeRoot;
use crate::types::Settings;
use crate::work::WorkError;

pub fn running(settings: &Settings) -> Result<bool, String> {
    let processes = fs::read_dir("/proc").map_err(|error| format!("Cannot inspect running games: {error}"))?;
    let mut buffer = [0_u8; 8192];
    for process in processes {
        let process = process.map_err(|error| format!("Cannot inspect /proc: {error}"))?;
        if process.file_name().as_bytes().iter().all(u8::is_ascii_digit)
            && matches_process(&process.path(), settings, &mut buffer)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn matches_process(process: &Path, settings: &Settings, buffer: &mut [u8]) -> Result<bool, String> {
    let mut command = match File::open(process.join("cmdline")) {
        Ok(file) => file,
        Err(error) if vanished_or_private(&error) => return Ok(false),
        Err(error) => return Err(format!("Cannot inspect a process command: {error}")),
    };
    let count = match command.read(buffer) {
        Ok(count) => count,
        Err(error) if vanished_or_private(&error) => return Ok(false),
        Err(error) => return Err(format!("Cannot read a process command: {error}")),
    };
    let first = buffer[..count].split(|byte| *byte == 0).next().unwrap_or_default();
    let name = first.rsplit(|byte| matches!(*byte, b'/' | b'\\')).next().unwrap_or_default();
    // Later arguments can name the game in lingering proton/python or
    // steam.exe wrappers. Only the actual process's argv[0] counts.
    if !name.eq_ignore_ascii_case(b"Arknights.exe") {
        return Ok(false);
    }
    let environment = match fs::read(process.join("environ")) {
        Ok(environment) => environment,
        Err(error) if vanished(&error) => return Ok(false),
        Err(error) => return Err(format!("Cannot determine the prefix of Arknights process {}: {error}", process.display())),
    };
    let compat_data = Path::new(&settings.compat_data);
    let prefix = compat_data.join("pfx");
    for variable in environment.split(|byte| *byte == 0) {
        if let Some(value) = variable.strip_prefix(b"WINEPREFIX=") {
            if same_path(Path::new(std::ffi::OsStr::from_bytes(value)), &prefix) {
                return Ok(true);
            }
        } else if let Some(value) = variable.strip_prefix(b"STEAM_COMPAT_DATA_PATH=") {
            if same_path(Path::new(std::ffi::OsStr::from_bytes(value)), compat_data) {
                return Ok(true);
            }
        }
    }
    uses_game_directory(process, Path::new(&settings.game_dir))
}

fn uses_game_directory(process: &Path, game_dir: &Path) -> Result<bool, String> {
    let target = match fs::metadata(game_dir) {
        Ok(metadata) => metadata,
        Err(error) if vanished(&error) => return Ok(false),
        Err(error) => return Err(format!("Cannot inspect the game directory: {error}")),
    };
    if fs::metadata(process.join("cwd")).is_ok_and(|metadata| same_identity(&metadata, &target)) {
        return Ok(true);
    }
    let maps = match File::open(process.join("maps")) {
        Ok(file) => file,
        Err(error) if vanished(&error) => return Ok(false),
        Err(error) => return Err(format!("Cannot inspect mapped game files: {error}")),
    };
    let root = process.join("root");
    let mut maps = BufReader::new(maps);
    let mut line = Vec::new();
    while maps.read_until(b'\n', &mut line).map_err(|error| format!("Cannot read mapped game files: {error}"))? != 0 {
        // Wine can unmap the PE executable after loading it. Its game DLLs
        // still identify the installation even if the working directory differs.
        let pathname = line.splitn(6, |byte| *byte == b' ').nth(5).unwrap_or_default();
        let start = pathname.iter().position(|byte| !byte.is_ascii_whitespace()).unwrap_or(pathname.len());
        let pathname = &pathname[start..];
        if pathname.starts_with(b"/") {
            if let Some(directory) = Path::new(std::ffi::OsStr::from_bytes(pathname)).parent() {
                let directory = directory.as_os_str().as_bytes();
                if mapped_directory_matches(&root, directory, &target)? {
                    return Ok(true);
                }
                // proc_pid_maps escapes newlines, but not literal backslashes.
                // Check both spellings so either valid directory name is safe.
                if directory.windows(4).any(|bytes| bytes == b"\\012") {
                    let mut decoded = Vec::with_capacity(directory.len());
                    let mut index = 0;
                    while index < directory.len() {
                        if directory[index..].starts_with(b"\\012") {
                            decoded.push(b'\n');
                            index += 4;
                        } else {
                            decoded.push(directory[index]);
                            index += 1;
                        }
                    }
                    if mapped_directory_matches(&root, &decoded, &target)? {
                        return Ok(true);
                    }
                }
            }
        }
        line.clear();
    }
    Ok(false)
}

fn mapped_directory_matches(root: &Path, directory: &[u8], target: &fs::Metadata) -> Result<bool, String> {
    let relative = Path::new(std::ffi::OsStr::from_bytes(directory)).strip_prefix("/").map_err(|error| error.to_string())?;
    match fs::metadata(root.join(relative)) {
        Ok(metadata) => Ok(same_identity(&metadata, target)),
        Err(error) if vanished(&error) => Ok(false),
        Err(error) => Err(format!("Cannot inspect a mapped game directory: {error}")),
    }
}

fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

pub fn spawn(settings: &Settings) -> Result<Child, String> {
    if !config::is_executable(Path::new(&settings.proton_path)) {
        return Err("Select an existing executable DWProton proton file in Settings".into());
    }
    let root = SafeRoot::open(Path::new(&settings.game_dir), false)
        .map_err(|error| error.to_string())?
        .ok_or("The selected game directory does not exist")?;
    root.open_file(Path::new("Arknights.exe"))
        .map_err(|error| error.to_string())?
        .ok_or("Arknights.exe is missing from the selected game directory")?;
    let compat = Path::new(&settings.compat_data);
    let steam = config::data_directory()?.join("steam");
    fs::create_dir_all(compat).map_err(|error| format!("Cannot create compatibility data: {error}"))?;
    fs::create_dir_all(&steam).map_err(|error| format!("Cannot create private Steam compatibility directory: {error}"))?;
    Command::new(&settings.proton_path)
        .arg("run")
        .arg(Path::new(&settings.game_dir).join("Arknights.exe"))
        .args(["-launcher_lang=zh-cn", "-launcher_sub_channel=1"])
        .current_dir(&settings.game_dir)
        .env("STEAM_COMPAT_DATA_PATH", compat)
        .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam)
        .env("WINEPREFIX", compat.join("pfx"))
        .env("SteamAppId", "0")
        .env("SteamGameId", "0")
        .stdin(Stdio::null())
        .spawn()
        .map_err(|error| format!("Cannot start the selected Proton runtime: {error}"))
}

// No idle process polling. During an active mutation, check at most once every
// two seconds as well as at its start, so an externally started game stops
// subsequent file commits without scanning all of /proc for every small file.
pub struct MutationGuard {
    settings: Settings,
    checked: Mutex<Instant>,
}

impl MutationGuard {
    pub fn new(settings: &Settings) -> Result<Self, WorkError> {
        if running(settings)? {
            return Err("Arknights is running from this installation or prefix; close it before modifying game files".to_string().into());
        }
        Ok(Self { settings: settings.clone(), checked: Mutex::new(Instant::now()) })
    }

    pub fn check(&self) -> Result<(), WorkError> {
        let mut checked = self.checked.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if checked.elapsed() >= Duration::from_secs(2) {
            if running(&self.settings)? {
                return Err("Arknights started during installation; further file changes were stopped".to_string().into());
            }
            *checked = Instant::now();
        }
        Ok(())
    }
}

fn vanished(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
        || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error())
}

fn vanished_or_private(error: &std::io::Error) -> bool {
    vanished(error) || error.kind() == std::io::ErrorKind::PermissionDenied
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right { return true; }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::symlink, process::CommandExt};

    struct TestProcess(Child);

    impl Drop for TestProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn shared_installation_is_detected_across_prefixes() {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("shared game\nvolume");
        let alias = temporary.path().join("game alias");
        let other_game = temporary.path().join("unrelated game");
        let selected_compat = temporary.path().join("selected prefix");
        let running_compat = temporary.path().join("running prefix");
        fs::create_dir(&game).unwrap();
        fs::create_dir(&other_game).unwrap();
        fs::create_dir_all(selected_compat.join("pfx")).unwrap();
        fs::create_dir_all(running_compat.join("pfx")).unwrap();
        symlink(&game, &alias).unwrap();
        let executable = game.join("Arknights.exe");
        fs::copy("/usr/bin/sh", &executable).unwrap();
        let mut child = TestProcess(
            Command::new(&executable)
                .arg0(r"C:\Games\Arknights.exe")
                .args(["-c", "printf '\\n'; read -r input"])
                .current_dir(temporary.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .env_clear()
                .env("WINEPREFIX", running_compat.join("pfx"))
                .env("STEAM_COMPAT_DATA_PATH", &running_compat)
                .spawn()
                .unwrap(),
        );
        // Wait until the program is running before inspecting its /proc files.
        child.0.stdout.as_mut().unwrap().read_exact(&mut [0_u8]).unwrap();
        let process = Path::new("/proc").join(child.0.id().to_string());
        let mut settings = Settings {
            game_dir: alias.to_str().unwrap().to_string(),
            compat_data: selected_compat.to_str().unwrap().to_string(),
            proton_path: String::new(),
        };
        let mut buffer = [0_u8; 8192];
        assert!(matches_process(&process, &settings, &mut buffer).unwrap());

        settings.game_dir = other_game.to_str().unwrap().to_string();
        assert!(!matches_process(&process, &settings, &mut buffer).unwrap());

        settings.compat_data = running_compat.to_str().unwrap().to_string();
        assert!(matches_process(&process, &settings, &mut buffer).unwrap());
    }
}
