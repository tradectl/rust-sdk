//! Background log-file janitor.
//!
//! Runs at startup and every 24h. Two responsibilities:
//!   1. Gzip files whose date is before today.
//!   2. Delete files (compressed or not) older than `retention_days`.
//!
//! Filenames are expected as `<prefix>_YYYY-MM-DD.log` (or `.log.gz`).
//! The date in the filename is authoritative — file mtime is ignored
//! so that NTP clock skew can't cause spurious deletes.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use flate2::write::GzEncoder;
use flate2::Compression;

const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Owns the background sweep thread. Drop the handle to stop the thread
/// after its current sweep completes (the sender side of a `crossbeam` /
/// `mpsc` channel could be added later if shutdown latency matters; for
/// now we rely on process exit).
pub struct LogJanitor {
    _handle: thread::JoinHandle<()>,
}

impl LogJanitor {
    /// Spawn a janitor for `dir` watching files named `<prefix>_*.log[.gz]`.
    /// Runs one sweep immediately, then every 24h.
    pub fn spawn(dir: PathBuf, prefix: String, retention_days: u32) -> Self {
        let handle = thread::Builder::new()
            .name("tradectl-log-janitor".into())
            .spawn(move || {
                loop {
                    if let Err(e) = sweep_once(&dir, &prefix, retention_days) {
                        eprintln!("log janitor: sweep failed: {e}");
                    }
                    thread::sleep(SWEEP_INTERVAL);
                }
            })
            .expect("spawn log janitor");
        Self { _handle: handle }
    }
}

/// Run a single sweep: gzip past-day `.log` files, delete files older than
/// `retention_days`. Public for tests and for one-shot invocations.
pub fn sweep_once(dir: &Path, prefix: &str, retention_days: u32) -> io::Result<()> {
    let today = Utc::now().date_naive();
    let cutoff = today - chrono::Duration::days(retention_days as i64);

    if !dir.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let parsed = match parse_log_filename(name, prefix) {
            Some(p) => p,
            None => continue,
        };

        // 1. Delete files older than cutoff (regardless of compression).
        if parsed.date < cutoff {
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("log janitor: failed to delete {}: {e}", path.display());
            }
            continue;
        }

        // 2. Gzip past-day uncompressed files.
        if !parsed.compressed && parsed.date < today {
            if let Err(e) = gzip_file(&path) {
                eprintln!("log janitor: failed to gzip {}: {e}", path.display());
            }
        }
    }

    Ok(())
}

struct ParsedName {
    date: NaiveDate,
    compressed: bool,
}

/// Parse `<prefix>_YYYY-MM-DD.log` or `<prefix>_YYYY-MM-DD.log.gz`.
/// Returns None for any other filename.
fn parse_log_filename(name: &str, prefix: &str) -> Option<ParsedName> {
    let expected_prefix = format!("{prefix}_");
    let rest = name.strip_prefix(&expected_prefix)?;

    let (date_part, compressed) = if let Some(d) = rest.strip_suffix(".log.gz") {
        (d, true)
    } else if let Some(d) = rest.strip_suffix(".log") {
        (d, false)
    } else {
        return None;
    };

    let date = NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()?;
    Some(ParsedName { date, compressed })
}

/// Gzip `path` to `<path>.gz` and unlink the original on success.
fn gzip_file(path: &Path) -> io::Result<()> {
    let gz_path = {
        let mut p = path.as_os_str().to_os_string();
        p.push(".gz");
        PathBuf::from(p)
    };

    {
        let input = File::open(path)?;
        let output = File::create(&gz_path)?;
        let mut reader = BufReader::new(input);
        let mut encoder = GzEncoder::new(BufWriter::new(output), Compression::default());
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            encoder.write_all(&buf[..n])?;
        }
        encoder.finish()?.flush()?;
    }

    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn touch(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn tmpdir() -> tempdir_lite::TempDir {
        tempdir_lite::TempDir::new("janitor-test").unwrap()
    }

    #[test]
    fn parse_filename_log() {
        let p = parse_log_filename("mybot_2026-05-04.log", "mybot").unwrap();
        assert_eq!(p.date, NaiveDate::from_ymd_opt(2026, 5, 4).unwrap());
        assert!(!p.compressed);
    }

    #[test]
    fn parse_filename_log_gz() {
        let p = parse_log_filename("mybot_2026-05-04.log.gz", "mybot").unwrap();
        assert_eq!(p.date, NaiveDate::from_ymd_opt(2026, 5, 4).unwrap());
        assert!(p.compressed);
    }

    #[test]
    fn parse_filename_wrong_prefix_skipped() {
        assert!(parse_log_filename("other_2026-05-04.log", "mybot").is_none());
    }

    #[test]
    fn parse_filename_malformed_date_skipped() {
        assert!(parse_log_filename("mybot_not-a-date.log", "mybot").is_none());
    }

    #[test]
    fn parse_filename_unrelated_extension_skipped() {
        assert!(parse_log_filename("mybot_2026-05-04.txt", "mybot").is_none());
    }

    #[test]
    fn sweep_gzips_past_day_log() {
        let dir = tmpdir();
        let today = Utc::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let yname = format!("mybot_{}.log", yesterday.format("%Y-%m-%d"));
        let tname = format!("mybot_{}.log", today.format("%Y-%m-%d"));
        touch(dir.path(), &yname, "yesterday data\n");
        touch(dir.path(), &tname, "today data\n");

        sweep_once(dir.path(), "mybot", 30).unwrap();

        // Yesterday's .log is gone, replaced by .log.gz.
        assert!(!dir.path().join(&yname).exists());
        assert!(dir.path().join(format!("{yname}.gz")).exists());
        // Today's .log is untouched.
        assert!(dir.path().join(&tname).exists());

        // Confirm the .gz round-trips back to the original bytes.
        let gz = File::open(dir.path().join(format!("{yname}.gz"))).unwrap();
        let mut out = String::new();
        flate2::read::GzDecoder::new(gz).read_to_string(&mut out).unwrap();
        assert_eq!(out, "yesterday data\n");
    }

    #[test]
    fn sweep_deletes_files_older_than_retention() {
        let dir = tmpdir();
        let today = Utc::now().date_naive();
        let old = today - chrono::Duration::days(40);
        let recent = today - chrono::Duration::days(5);
        touch(dir.path(), &format!("mybot_{}.log.gz", old.format("%Y-%m-%d")), "x");
        touch(dir.path(), &format!("mybot_{}.log.gz", recent.format("%Y-%m-%d")), "x");

        sweep_once(dir.path(), "mybot", 30).unwrap();

        assert!(!dir.path().join(format!("mybot_{}.log.gz", old.format("%Y-%m-%d"))).exists());
        assert!(dir.path().join(format!("mybot_{}.log.gz", recent.format("%Y-%m-%d"))).exists());
    }

    #[test]
    fn sweep_ignores_unrelated_files() {
        let dir = tmpdir();
        touch(dir.path(), "README.md", "hi");
        touch(dir.path(), "other_2026-05-04.log", "not ours");
        sweep_once(dir.path(), "mybot", 30).unwrap();
        assert!(dir.path().join("README.md").exists());
        assert!(dir.path().join("other_2026-05-04.log").exists());
    }

    #[test]
    fn sweep_on_missing_dir_is_ok() {
        let nope = std::env::temp_dir().join("janitor-test-does-not-exist-xyz");
        // Make sure it really doesn't exist.
        let _ = std::fs::remove_dir_all(&nope);
        assert!(sweep_once(&nope, "mybot", 30).is_ok());
    }

    /// Tiny self-contained TempDir helper so we don't pull in the `tempfile`
    /// crate just for tests. Cleans up on drop.
    mod tempdir_lite {
        use std::path::{Path, PathBuf};
        pub struct TempDir(PathBuf);
        impl TempDir {
            pub fn new(label: &str) -> std::io::Result<Self> {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let p = std::env::temp_dir().join(format!("tradectl-{label}-{nanos}"));
                std::fs::create_dir_all(&p)?;
                Ok(Self(p))
            }
            pub fn path(&self) -> &Path { &self.0 }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
