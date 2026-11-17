use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{mkdirat, openat, statat, AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

use crate::archive::Entry;
use crate::work::{Control, WorkError};

pub fn archive_path(name: &str) -> Result<PathBuf, String> {
    let name = name.strip_suffix('/').unwrap_or(name);
    if name.is_empty()
        || name.starts_with('/')
        || name.contains(['\\', ':', '\0'])
        || name.split('/').any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("Refusing an unsafe ZIP path: {name:?}"));
    }
    Ok(PathBuf::from(name))
}

pub struct SafeRoot {
    directory: File,
}

impl SafeRoot {
    pub fn open(path: &Path, create: bool) -> Result<Option<Self>, WorkError> {
        if create {
            fs::create_dir_all(path)
                .map_err(|error| format!("Cannot create game directory {}: {error}", path.display()))?;
        }
        let directory = match rustix::fs::open(path, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()) {
            Ok(descriptor) => File::from(descriptor),
            Err(Errno::NOENT) if !create => return Ok(None),
            Err(error) => return Err(format!("Cannot open game directory {}: {error}", path.display()).into()),
        };
        if !directory.metadata()?.is_dir() {
            return Err("The selected game path is not a directory".to_string().into());
        }
        // The selected root may itself be a user-chosen symlink. All archive
        // traversal below it is descriptor-relative and refuses every symlink.
        Ok(Some(Self { directory }))
    }

    pub fn check_selected(&self, path: &Path) -> Result<(), WorkError> {
        let selected = fs::metadata(path)
            .map_err(|error| format!("The selected game directory disappeared: {error}"))?;
        let anchored = self.directory.metadata()?;
        if !selected.is_dir() || selected.dev() != anchored.dev() || selected.ino() != anchored.ino() {
            return Err("The selected game directory was replaced during the operation".to_string().into());
        }
        Ok(())
    }

    pub fn open_file(&self, path: &Path) -> Result<Option<File>, WorkError> {
        let Some(parent) = self.parent(path, false)? else { return Ok(None) };
        let name = filename(path)?;
        let descriptor = match openat(&parent, name, OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK, Mode::empty()) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(format!("Cannot safely open {} (symlinks are refused): {error}", path.display()).into()),
        };
        let file = File::from(descriptor);
        if !file.metadata()?.is_file() {
            return Err(format!("Refusing a non-regular game file: {}", path.display()).into());
        }
        Ok(Some(file))
    }

    pub fn ensure_directory(&self, path: &Path) -> Result<(), WorkError> {
        self.walk(path, true)?.ok_or_else(|| WorkError::Failed("Cannot create ZIP directory".into()))?;
        Ok(())
    }

    pub fn replace(
        &self,
        entry: &Entry,
        reader: &mut impl Read,
        buffer: &mut [u8],
        control: &Control,
        mut progress: impl FnMut(u64),
        mut before_commit: impl FnMut() -> Result<(), WorkError>,
    ) -> Result<(), WorkError> {
        control.check()?;
        let parent = self.parent(&entry.path, true)?.ok_or("Missing destination parent".to_string())?;
        let name = filename(&entry.path)?;
        let mode = target_mode(&parent, name)?;
        // /proc/self/fd anchors tempfile and rename to the directory already
        // opened with O_NOFOLLOW, even if an ancestor is concurrently renamed.
        let parent_path = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd()));
        let mut temporary = tempfile::Builder::new()
            .prefix(".arknights-")
            .tempfile_in(&parent_path)
            .map_err(|error| format!("Cannot create temporary {}: {error}", entry.path.display()))?;
        let mut hash = crc32fast::Hasher::new();
        let mut written = 0_u64;
        loop {
            control.check()?;
            let count = reader.read(buffer).map_err(|error| control.error(format!("Cannot extract {}: {error}", entry.path.display())))?;
            if count == 0 { break; }
            written = written.checked_add(count as u64).ok_or("Extracted file size overflows".to_string())?;
            if written > entry.size {
                return Err(format!("Extracted file exceeds its ZIP size: {}", entry.path.display()).into());
            }
            temporary.write_all(&buffer[..count])
                .map_err(|error| format!("Cannot write {}: {error}", entry.path.display()))?;
            hash.update(&buffer[..count]);
            progress(count as u64);
        }
        if written != entry.size || hash.finalize() != entry.crc32 {
            return Err(format!("ZIP size/CRC verification failed; the existing file was preserved: {}", entry.path.display()).into());
        }
        temporary.as_file().set_permissions(fs::Permissions::from_mode(mode))?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        control.check()?;
        before_commit()?;
        control.check()?;
        target_mode(&parent, name)?;
        temporary.persist(parent_path.join(name))
            .map_err(|error| format!("Cannot atomically replace {}: {}", entry.path.display(), error.error))?;
        parent.sync_all().map_err(|error| format!("Cannot synchronize {}: {error}", entry.path.display()))?;
        Ok(())
    }

    fn parent(&self, path: &Path, create: bool) -> Result<Option<File>, WorkError> {
        validate_relative(path)?;
        self.walk(path.parent().unwrap_or_else(|| Path::new("")), create)
    }

    fn walk(&self, path: &Path, create: bool) -> Result<Option<File>, WorkError> {
        let mut directory = self.directory.try_clone()?;
        for component in path.components() {
            let Component::Normal(name) = component else {
                return Err("Refusing an unsafe destination path".to_string().into());
            };
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
            let descriptor = match openat(&directory, name, flags, Mode::empty()) {
                Ok(descriptor) => descriptor,
                Err(Errno::NOENT) if create => {
                    match mkdirat(&directory, name, Mode::from_bits_truncate(0o755)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(error) => return Err(format!("Cannot create {}: {error}", path.display()).into()),
                    }
                    openat(&directory, name, flags, Mode::empty())
                        .map_err(|error| format!("Cannot safely open created directory {}: {error}", path.display()))?
                }
                Err(Errno::NOENT) => return Ok(None),
                Err(error) => return Err(format!("Refusing unsafe game directory {} (symlinks are refused): {error}", path.display()).into()),
            };
            directory = File::from(descriptor);
        }
        Ok(Some(directory))
    }
}

fn validate_relative(path: &Path) -> Result<(), WorkError> {
    if path.as_os_str().is_empty() || path.components().any(|component| !matches!(component, Component::Normal(_))) {
        return Err("Refusing an unsafe destination path".to_string().into());
    }
    Ok(())
}

fn filename(path: &Path) -> Result<&OsStr, WorkError> {
    path.file_name().ok_or_else(|| WorkError::Failed("Destination has no file name".into()))
}

fn target_mode(parent: &File, name: &OsStr) -> Result<u32, WorkError> {
    match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) if FileType::from_raw_mode(metadata.st_mode).is_file() => Ok(metadata.st_mode & 0o777),
        Ok(_) => Err("Refusing to replace a symbolic link or non-regular game file".to_string().into()),
        Err(Errno::NOENT) => Ok(0o644),
        Err(error) => Err(format!("Cannot inspect game destination: {error}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::unix::fs::symlink;

    #[test]
    fn archive_paths_and_existing_symlinks_cannot_escape() {
        for name in ["../outside", "/outside", "C:/outside", "a/../outside", "a\\..\\outside", "a//b", "a\0b"] {
            assert!(archive_path(name).is_err(), "{name:?}");
        }
        assert_eq!(archive_path("Arknights_Data/data.bin").unwrap(), Path::new("Arknights_Data/data.bin"));
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("payload"), b"untouched").unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        symlink(outside.path().join("payload"), root.path().join("direct")).unwrap();
        let safe = SafeRoot::open(root.path(), false).unwrap().unwrap();
        assert!(safe.open_file(Path::new("escape/payload")).is_err());
        assert!(safe.open_file(Path::new("direct")).is_err());
        assert!(safe.ensure_directory(Path::new("escape/nested")).is_err());
        assert!(!outside.path().join("nested").exists());
    }

    #[test]
    fn failed_crc_never_replaces_an_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Arknights.exe"), b"original").unwrap();
        let safe = SafeRoot::open(root.path(), false).unwrap().unwrap();
        let entry = Entry {
            index: 0,
            path: PathBuf::from("Arknights.exe"),
            size: 7,
            compressed_size: 7,
            crc32: crc32fast::hash(b"correct"),
            header_start: 0,
        };
        let result = safe.replace(&entry, &mut Cursor::new(b"corrupt"), &mut [0; 3], &Control::default(), |_| {}, || Ok(()));
        assert!(result.is_err());
        assert_eq!(fs::read(root.path().join("Arknights.exe")).unwrap(), b"original");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
