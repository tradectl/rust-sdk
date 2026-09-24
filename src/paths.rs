//! On-disk layout under `~/.tradectl`, shared by the bot, the CLI and the Lab.
//!
//! Everything that belongs to one bot lives in `~/.tradectl/bot/<bot>/`, so a
//! bot moves between hosts by copying that one folder:
//!
//! ```text
//! ~/.tradectl/bot/<bot>/
//!   config.json, versions/, trades.db, sessions.db, resources.db
//!   cert.pem, key.pem, creds.json, port, host, published
//!   bot.pid, crash.log, disk-stopped
//!   logs/<bot>.YYYY-MM-DD.log, logs/<bot>.stderr.log
//! ```
//!
//! Account-wide files (credentials, license, `bin/`, `lib/`, `data/`, Lab
//! settings) stay at the top of `~/.tradectl`.

use std::path::PathBuf;

/// `$TRADECTL_HOME/.tradectl`, else `~/.tradectl`.
pub fn tradectl_dir() -> PathBuf {
    std::env::var("TRADECTL_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tradectl")
}

/// `~/.tradectl/bot` — one subfolder per bot.
pub fn bots_root() -> PathBuf {
    tradectl_dir().join("bot")
}

/// `~/.tradectl/bot/<bot>` — everything that belongs to one bot.
pub fn bot_dir(bot_name: &str) -> PathBuf {
    bots_root().join(bot_name)
}

/// `~/.tradectl/bot/<bot>/logs` — the default log folder.
pub fn bot_logs_dir(bot_name: &str) -> PathBuf {
    bot_dir(bot_name).join("logs")
}

/// `~/.tradectl/bot/<bot>/bot.pid`.
pub fn bot_pid_path(bot_name: &str) -> PathBuf {
    bot_dir(bot_name).join("bot.pid")
}

/// Where a bot lived before the `bot/<bot>` layout: `~/.tradectl/run/<bot>`.
/// Only used to detect a host that has not been migrated yet.
pub fn legacy_run_dir(bot_name: &str) -> PathBuf {
    tradectl_dir().join("run").join(bot_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_rooted_in_bot_folder() {
        let d = bot_dir("bnum");
        assert!(d.ends_with(".tradectl/bot/bnum"), "got {}", d.display());
        assert!(bot_logs_dir("bnum").ends_with(".tradectl/bot/bnum/logs"));
        assert!(bot_pid_path("bnum").ends_with(".tradectl/bot/bnum/bot.pid"));
        assert!(legacy_run_dir("bnum").ends_with(".tradectl/run/bnum"));
    }
}
