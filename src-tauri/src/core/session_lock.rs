//! Cross-platform advisory file lock so two concurrent instances of the
//! toggler cannot interleave writes against the same `CLAUDE.md`. Held only
//! for the duration of a single toggle/CRUD operation — never long-lived.
//!
//! The lock file lives next to the target (e.g. `~/.claude/.toggler.lock`)
//! and is created on demand. Using `fs2` so the lock semantics match the
//! native primitive on each OS (flock on Unix, LockFileEx on Windows).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;

/// RAII guard — the lock is released when this value is dropped.
pub struct SessionGuard {
    file: File,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Acquire an exclusive advisory lock on `lock_path`, blocking until granted.
/// The lock file is created if it doesn't exist; the file itself carries no
/// payload — its presence and the OS lock state are the entire signal.
pub fn acquire_blocking(lock_path: &Path) -> io::Result<SessionGuard> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    file.lock_exclusive()?;
    Ok(SessionGuard { file })
}

/// Try to acquire the lock without blocking; returns `Ok(None)` if another
/// instance currently holds it.
pub fn try_acquire(lock_path: &Path) -> io::Result<Option<SessionGuard>> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(SessionGuard { file })),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e),
    }
}

/// Resolve the default lock path next to the target file.
pub fn default_lock_path(target: &Path) -> PathBuf {
    target
        .parent()
        .map(|p| p.join(".toggler.lock"))
        .unwrap_or_else(|| PathBuf::from(".toggler.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn acquire_blocking_creates_lock_file_when_missing() {
        let dir = tempdir().unwrap();
        let lock = dir.path().join(".toggler.lock");
        let _guard = acquire_blocking(&lock).unwrap();
        assert!(lock.exists());
    }

    #[test]
    fn try_acquire_returns_none_when_lock_already_held() {
        let dir = tempdir().unwrap();
        let lock = dir.path().join(".toggler.lock");
        let _held = acquire_blocking(&lock).unwrap();
        let second = try_acquire(&lock).unwrap();
        assert!(second.is_none());
    }

    #[test]
    fn drop_releases_the_lock() {
        let dir = tempdir().unwrap();
        let lock = dir.path().join(".toggler.lock");
        {
            let _g = acquire_blocking(&lock).unwrap();
        }
        // After scope exit, another acquirer should succeed without blocking.
        let again = try_acquire(&lock).unwrap();
        assert!(again.is_some());
    }

    #[test]
    fn acquire_blocking_waits_for_release() {
        let dir = tempdir().unwrap();
        let lock = dir.path().join(".toggler.lock");
        let held = acquire_blocking(&lock).unwrap();
        let lock_clone = lock.clone();
        let handle = thread::spawn(move || acquire_blocking(&lock_clone).unwrap());
        // Give the spawned thread time to attempt the lock; it should be
        // blocked because we still hold the guard.
        thread::sleep(Duration::from_millis(50));
        drop(held);
        let second = handle.join().unwrap();
        drop(second);
    }
}
