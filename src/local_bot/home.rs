//! Paths under one `.tradectl` folder.

use std::path::{Path, PathBuf};

/// The `.tradectl` folder every local-bot function works in. The Lab passes
/// its own (`Home::at`), the CLI the sdk rule (`Home::from_env`), tests a
/// temp folder, so all of them agree on one folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// `$TRADECTL_HOME/.tradectl`, else `~/.tradectl`.
    pub fn from_env() -> Home {
        Home::at(crate::paths::tradectl_dir())
    }

    /// The `.tradectl` folder at `root`.
    pub fn at(root: impl Into<PathBuf>) -> Home {
        Home { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `bot/`: one folder per bot.
    pub fn bots_dir(&self) -> PathBuf {
        self.root.join("bot")
    }

    pub fn bot_dir(&self, name: &str) -> PathBuf {
        self.bots_dir().join(name)
    }

    pub fn config_path(&self, name: &str) -> PathBuf {
        self.bot_dir(name).join("config.json")
    }

    pub fn pid_path(&self, name: &str) -> PathBuf {
        self.bot_dir(name).join("bot.pid")
    }

    pub fn status_path(&self, name: &str) -> PathBuf {
        self.bot_dir(name).join("status.json")
    }

    pub fn bot_id_path(&self, name: &str) -> PathBuf {
        self.bot_dir(name).join("bot-id")
    }

    pub fn crash_log_path(&self, name: &str) -> PathBuf {
        self.bot_dir(name).join("crash.log")
    }

    /// `locks/`, outside every bot folder.
    pub fn locks_dir(&self) -> PathBuf {
        self.root.join("locks")
    }

    pub fn lock_path(&self, name: &str) -> PathBuf {
        self.locks_dir().join(format!("{name}.lock"))
    }

    pub fn holder_path(&self, name: &str) -> PathBuf {
        self.locks_dir().join(format!("{name}.holder"))
    }

    /// The record of the last holder, kept for messages only.
    pub fn holder_last_path(&self, name: &str) -> PathBuf {
        self.locks_dir().join(format!("{name}.holder.last"))
    }

    pub fn ports_lock_path(&self) -> PathBuf {
        self.locks_dir().join(".ports.lock")
    }

    /// v0.2.7's O_EXCL port lock, still taken for one release.
    pub fn old_port_lock_path(&self) -> PathBuf {
        self.bots_dir().join(".port-lock")
    }

    /// `trash/`: deleted folders on their way out, outside `bot/`.
    pub fn trash_dir(&self) -> PathBuf {
        self.root.join("trash")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_sit_where_the_layout_says() {
        let h = Home::at("/h/.tradectl");
        assert_eq!(h.bot_dir("a"), Path::new("/h/.tradectl/bot/a"));
        assert_eq!(h.pid_path("a"), Path::new("/h/.tradectl/bot/a/bot.pid"));
        assert_eq!(h.status_path("a"), Path::new("/h/.tradectl/bot/a/status.json"));
        assert_eq!(h.bot_id_path("a"), Path::new("/h/.tradectl/bot/a/bot-id"));
        assert_eq!(h.lock_path("a"), Path::new("/h/.tradectl/locks/a.lock"));
        assert_eq!(h.holder_path("a"), Path::new("/h/.tradectl/locks/a.holder"));
        assert_eq!(h.holder_last_path("a"), Path::new("/h/.tradectl/locks/a.holder.last"));
        assert_eq!(h.ports_lock_path(), Path::new("/h/.tradectl/locks/.ports.lock"));
        assert_eq!(h.trash_dir(), Path::new("/h/.tradectl/trash"));
    }
}
