//! Local bot rules, shared by the CLI, the Lab and the engine: one OS lock
//! per bot, the holder record, process identity, bots started by v0.2.7 and
//! older (no lock), the bot id, `status.json` and Delete.
//!
//! "Running" means: the bot's lock is held and its holder record names a
//! live process ([`running::bot_running`]). Every function takes a [`Home`]
//! (the `.tradectl` folder) and never reads the environment for it.
//!
//! ```text
//! ~/.tradectl/locks/<bot>.lock          empty, held with File::try_lock, never deleted
//! ~/.tradectl/locks/<bot>.holder        holder record (JSON), .holder.last once released
//! ~/.tradectl/locks/.ports.lock         port resolution
//! ~/.tradectl/bot/<bot>/bot.pid         plain pid, written by the holder only
//! ~/.tradectl/bot/<bot>/bot-id          bot id, folder path, host
//! ~/.tradectl/bot/<bot>/status.json     what the holder is doing
//! ~/.tradectl/trash/<bot>-<16 hex>      a deleted folder on its way out
//! ```

pub mod exit;
pub mod folders;
pub mod fsx;
pub mod holder;
pub mod home;
pub mod legacy;
pub mod lock;
pub mod names;
pub mod proc;
pub mod running;
pub mod signal;
pub mod status;
pub mod units;
pub mod win;

pub use home::Home;
pub use running::{bot_running, bot_running_now, Liveness};

/// Wall-clock now, unix ms.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `t` as unix ms, `None` before 1970.
pub(crate) fn unix_ms(t: std::time::SystemTime) -> Option<i64> {
    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

/// 128 random bits as 32 hex chars.
pub(crate) fn random_hex128() -> String {
    format!("{:032x}", rand::random::<u128>())
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use super::Home;

    /// Env var naming the `.tradectl` folder a re-executed test child works in.
    pub const ENV_HOME: &str = "TRADECTL_SDK_TEST_HOME";
    /// Env var naming the bot a child works on.
    pub const ENV_NAME: &str = "TRADECTL_SDK_TEST_NAME";

    /// A fresh `.tradectl` folder under the temp dir, removed on drop.
    pub struct TempHome {
        base: PathBuf,
        pub home: Home,
    }

    impl TempHome {
        pub fn new() -> TempHome {
            static N: AtomicU32 = AtomicU32::new(0);
            let base = std::env::temp_dir().join(format!(
                "tradectl-sdk-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(base.join(".tradectl")).unwrap();
            // Canonical, so paths compare equal to what the code records.
            let root = std::fs::canonicalize(base.join(".tradectl")).unwrap();
            TempHome { base, home: Home::at(root) }
        }

        /// `bot/<name>/` with a `config.json`.
        pub fn bot(&self, name: &str) -> PathBuf {
            let dir = self.home.bot_dir(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.json"), "{}").unwrap();
            std::fs::canonicalize(dir).unwrap()
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// A child this test started: killed and reaped on drop, also when the
    /// test fails half way.
    pub struct ChildGuard(pub Child);

    impl ChildGuard {
        pub fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// This test binary again, running only the ignored test `test`.
    pub fn respawn(test: &str, home: &Home, name: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", test, "--ignored", "--test-threads=1"])
            .env(ENV_HOME, home.root())
            .env(ENV_NAME, name)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd
    }

    /// The child's `Home` and bot name, or `None` when the test runs on its
    /// own (`cargo test -- --ignored`).
    pub fn child_env() -> Option<(Home, String)> {
        let home = std::env::var_os(ENV_HOME)?;
        let name = std::env::var(ENV_NAME).ok()?;
        Some((Home::at(home), name))
    }

    /// Wait for `path` to exist, at most `limit`.
    pub fn wait_for(path: &Path, limit: Duration) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < limit {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        path.exists()
    }

    /// Child side: sleep until `<home>/<tag>.stop` exists, at most 60 s, so a
    /// child never outlives a test run by much even if nobody kills it.
    pub fn idle_until_stopped(home: &Home, tag: &str) {
        let stop = home.root().join(format!("{tag}.stop"));
        let t0 = Instant::now();
        while !stop.exists() && t0.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
