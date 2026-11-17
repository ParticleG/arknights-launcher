use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::types::Settings;

#[derive(Clone, Deserialize, Serialize)]
pub struct VerifiedInstallation {
    game_dir: String,
    pub version: String,
    device: u64,
    inode: u64,
}

impl VerifiedInstallation {
    pub fn record(settings: &Settings, version: &str) -> Result<Self, String> {
        let metadata = fs::metadata(&settings.game_dir)
            .map_err(|error| format!("Cannot identify the verified game directory: {error}"))?;
        if !metadata.is_dir() {
            return Err("The game directory is not a directory".into());
        }
        Ok(Self {
            game_dir: settings.game_dir.clone(),
            version: version.into(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub fn matches(&self, settings: &Settings) -> bool {
        self.game_dir == settings.game_dir
            && fs::metadata(&settings.game_dir).is_ok_and(|metadata| {
                metadata.is_dir()
                    && metadata.dev() == self.device
                    && metadata.ino() == self.inode
            })
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Config {
    pub settings: Settings,
    pub verified_installation: Option<VerifiedInstallation>,
}

pub fn directory() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|path| path.join("arknights-launcher"))
        .ok_or_else(|| "Cannot determine the XDG configuration directory".into())
}

pub fn data_directory() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|path| path.join("arknights-launcher"))
        .ok_or_else(|| "Cannot determine the XDG data directory".into())
}

pub fn load(directory: &Path) -> Result<Config, String> {
    let path = directory.join("settings.json");
    match File::open(&path) {
        Ok(file) => {
            let mut config: Config = serde_json::from_reader(file)
                .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
            // A removed runtime must not make an existing installation inaccessible.
            // Executability is checked when selecting a runtime and when launching.
            config.settings.game_dir = normalize_path(&config.settings.game_dir)?;
            config.settings.compat_data = normalize_path(&config.settings.compat_data)?;
            if !config.settings.proton_path.is_empty() {
                config.settings.proton_path = normalize_path(&config.settings.proton_path)?;
            }
            Ok(config)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config {
            settings: defaults()?,
            verified_installation: None,
        }),
        Err(error) => Err(format!("Cannot open {}: {error}", path.display())),
    }
}

pub fn save(directory: &Path, config: &Config) -> Result<(), String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("Cannot create {}: {error}", directory.display()))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".settings-")
        .tempfile_in(directory)
        .map_err(|error| format!("Cannot create temporary settings: {error}"))?;
    serde_json::to_writer_pretty(&mut temporary, config)
        .map_err(|error| format!("Cannot serialize settings: {error}"))?;
    temporary.write_all(b"\n").map_err(|error| error.to_string())?;
    temporary.as_file().sync_all().map_err(|error| error.to_string())?;
    temporary
        .persist(directory.join("settings.json"))
        .map_err(|error| format!("Cannot save settings: {}", error.error))?;
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("Cannot synchronize settings directory: {error}"))
}

pub fn validate(settings: Settings) -> Result<Settings, String> {
    let settings = Settings {
        game_dir: normalize_path(&settings.game_dir)?,
        compat_data: normalize_path(&settings.compat_data)?,
        proton_path: if settings.proton_path.is_empty() {
            String::new()
        } else {
            normalize_path(&settings.proton_path)?
        },
    };
    if !settings.proton_path.is_empty() && !is_executable(Path::new(&settings.proton_path)) {
        return Err("The selected Proton path is not an executable file".into());
    }
    for (name, path) in [
        ("Game directory", &settings.game_dir),
        ("Compatibility data", &settings.compat_data),
    ] {
        match fs::metadata(path) {
            Ok(metadata) if !metadata.is_dir() => return Err(format!("{name} is not a directory")),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot inspect {name}: {error}")),
        }
    }
    Ok(settings)
}

pub fn normalize_path(value: &str) -> Result<String, String> {
    if value.as_bytes().contains(&0) || !Path::new(value).is_absolute() {
        return Err("Selected paths must be absolute and contain no NUL bytes".into());
    }
    let mut normalized = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    path_string(&normalized)
}

pub fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn path_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "The selected path cannot be represented as UTF-8".into())
}

fn defaults() -> Result<Settings, String> {
    let data = dirs::data_dir().ok_or("Cannot determine the XDG data directory")?;
    let compat_data = data.join("arknights-proton/compatdata");
    let game_dir = compat_data.join("pfx/drive_c/Program Files/Hypergryph Launcher/games/Arknights");
    let proton_path = discover_proton(&data)
        .map(|path| path_string(&path))
        .transpose()?
        .unwrap_or_default();
    Ok(Settings {
        game_dir: path_string(&game_dir)?,
        compat_data: path_string(&compat_data)?,
        proton_path,
    })
}

fn discover_proton(data: &Path) -> Option<PathBuf> {
    let standard = PathBuf::from("/usr/share/steam/compatibilitytools.d/dwproton/proton");
    if is_executable(&standard) {
        return Some(standard);
    }
    let mut roots = vec![
        data.join("lutris/runners/proton"),
        data.join("lutris/runners/wine"),
        data.join("Steam/compatibilitytools.d"),
    ];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".steam/root/compatibilitytools.d"));
    }
    let mut candidates = Vec::new();
    for root in roots {
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().to_ascii_lowercase().starts_with("dwproton") {
                    let candidate = entry.path().join("proton");
                    if is_executable(&candidate) {
                        candidates.push(candidate);
                    }
                }
            }
        }
    }
    candidates.sort();
    candidates.pop().or_else(|| {
        let candidate = PathBuf::from("/usr/share/wegame-dwproton/dwproton/proton");
        is_executable(&candidate).then_some(candidate)
    })
}
