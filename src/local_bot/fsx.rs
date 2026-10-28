//! Atomic writes and renames that survive Windows sharing errors.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long [`write_atomic`] retries its rename on Windows.
const WRITE_RENAME_WAIT: Duration = Duration::from_secs(2);

/// Write `bytes` to `path` through a temp file beside it and a rename, so a
/// reader never sees half a file. `sync`: `sync_all` before the rename.
pub fn write_atomic(path: &Path, bytes: &[u8], sync: bool) -> io::Result<()> {
    write_with(path, bytes, sync, false)
}

/// [`write_atomic`] for a file only this user may read (0600 on Unix).
pub fn write_atomic_private(path: &Path, bytes: &[u8], sync: bool) -> io::Result<()> {
    write_with(path, bytes, sync, true)
}

fn write_with(path: &Path, bytes: &[u8], sync: bool, private: bool) -> io::Result<()> {
    let tmp = tmp_path(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let result = opts.open(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        if sync {
            f.sync_all()?;
        }
        Ok(())
    });
    let result = result.and_then(|()| rename_retry(&tmp, path, WRITE_RENAME_WAIT));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `<file>.<pid>.<n>.tmp` beside `path`: no two writers share a temp file.
fn tmp_path(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{name}.{}.{}.tmp", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)))
}

/// `fs::rename`, retried on Windows for up to `total` while another program
/// (antivirus, an indexer, an open editor) holds a file in the way without
/// `FILE_SHARE_DELETE`. Backoff 100 ms, doubling to 1 s. One try elsewhere.
pub fn rename_retry(from: &Path, to: &Path, total: Duration) -> io::Result<()> {
    let deadline = Instant::now() + total;
    let mut pause = Duration::from_millis(100);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if is_sharing_error(&e) && Instant::now() < deadline => {
                std::thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now())));
                pause = (pause * 2).min(Duration::from_secs(1));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Windows `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION`,
/// `ERROR_LOCK_VIOLATION`: another handle is in the way. Never on Unix.
pub(crate) fn is_sharing_error(e: &io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_bot::testutil::TempHome;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp() {
        let t = TempHome::new();
        let p = t.home.root().join("f.json");
        write_atomic(&p, b"one", false).unwrap();
        write_atomic(&p, b"two", true).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        let left: Vec<_> = std::fs::read_dir(t.home.root()).unwrap().filter_map(Result::ok).collect();
        assert_eq!(left.len(), 1, "only the file, no temp");
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempHome::new();
        let p = t.home.root().join("port");
        write_atomic_private(&p, b"9103", false).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_failed_write_leaves_no_temp() {
        let t = TempHome::new();
        let p = t.home.root().join("no-such-dir").join("f");
        assert!(write_atomic(&p, b"x", false).is_err());
        assert!(!t.home.root().join("no-such-dir").exists());
    }
}
