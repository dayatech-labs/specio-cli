//! Small filesystem helpers: atomic writes (temporary file + rename) and advisory locks.
use crate::error::{Error, IoContext, Result};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Readable by other users of the machine (workspace files).
    Shared,
    /// Owner only (credentials-adjacent state such as the capability snapshot).
    Private,
}

/// Write `bytes` to `path` so that readers see either the old or the new content, never a mix.
pub fn atomic_write(path: &Path, bytes: &[u8], visibility: Visibility) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Other(format!("no parent directory for {}", path.display())))?;
    create_dirs(parent, visibility)?;
    let mut tmp = NamedTempFile::new_in(parent)
        .ctx(format!("create temporary file in {}", parent.display()))?;
    tmp.write_all(bytes)
        .ctx(format!("write {}", path.display()))?;
    tmp.as_file()
        .sync_all()
        .ctx(format!("sync {}", path.display()))?;
    set_file_mode(tmp.as_file(), visibility)?;
    tmp.persist(path).map_err(|e| Error::Io {
        context: format!("replace {}", path.display()),
        source: e.error,
    })?;
    Ok(())
}

pub fn create_dirs(dir: &Path, visibility: Visibility) -> Result<()> {
    if dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(dir).ctx(format!("create directory {}", dir.display()))?;
    #[cfg(unix)]
    if visibility == Visibility::Private {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .ctx(format!("restrict {}", dir.display()))?;
    }
    #[cfg(not(unix))]
    let _ = visibility;
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(file: &File, visibility: Visibility) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if visibility == Visibility::Private {
        0o600
    } else {
        0o644
    };
    file.set_permissions(fs::Permissions::from_mode(mode))
        .ctx("set file permissions")
}

#[cfg(not(unix))]
fn set_file_mode(_file: &File, _visibility: Visibility) -> Result<()> {
    Ok(())
}

pub fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io {
            context: format!("read {}", path.display()),
            source: e,
        }),
    }
}

pub fn remove_optional(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io {
            context: format!("remove {}", path.display()),
            source: e,
        }),
    }
}

/// An OS advisory lock held until dropped (or the process dies).
#[derive(Debug)]
pub struct FileLock {
    _file: File,
}

fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        create_dirs(parent, Visibility::Private)?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .ctx(format!("open lock {}", path.display()))
}

impl FileLock {
    /// Block until the lock is free.
    pub fn exclusive(path: &Path) -> Result<FileLock> {
        let file = open_lock_file(path)?;
        file.lock().ctx(format!("lock {}", path.display()))?;
        Ok(FileLock { _file: file })
    }

    /// Like [`FileLock::try_acquire`], but tolerate a briefly held lock: another process may be
    /// finishing, or a child process may still share the descriptor between `fork` and `exec`.
    pub fn try_acquire_within(
        path: &Path,
        shared: bool,
        grace: std::time::Duration,
    ) -> Result<Option<FileLock>> {
        let deadline = std::time::Instant::now() + grace;
        loop {
            if let Some(lock) = FileLock::try_acquire(path, shared)? {
                return Ok(Some(lock));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// Fail immediately when another process holds the lock; `None` means it is busy.
    pub fn try_acquire(path: &Path, shared: bool) -> Result<Option<FileLock>> {
        let file = open_lock_file(path)?;
        let result = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match result {
            Ok(()) => Ok(Some(FileLock { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(Error::Io {
                context: format!("lock {}", path.display()),
                source: e,
            }),
        }
    }
}
