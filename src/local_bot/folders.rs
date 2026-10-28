//! The bot folders under `bot/`.

use std::path::PathBuf;

use super::names::valid_bot_name;
use super::Home;

/// Folders under `bot/` whose name is a bot name, sorted. Skips
/// `<bot>.deleted`, `.deleted-N`, `.trash-*`, dot folders and copies with
/// spaces ("paper copy"), so no scan ever reads their `port` or `bot.pid`.
pub fn bot_folders(home: &Home) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(home.bots_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let path = e.path();
            (valid_bot_name(&name) && path.is_dir()).then_some((name, path))
        })
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_bot::testutil::TempHome;

    #[test]
    fn skips_deleted_trash_and_invalid() {
        let t = TempHome::new();
        let bots = t.home.bots_dir();
        for d in [
            "paper",
            "b-2",
            "paper.deleted",
            "paper.deleted-2",
            ".trash-paper-0123",
            ".hidden",
            "paper copy",
            "Paper",
            "watchdog",
        ] {
            std::fs::create_dir_all(bots.join(d)).unwrap();
            std::fs::write(bots.join(d).join("port"), "9103").unwrap();
        }
        std::fs::write(bots.join("file-not-dir"), "").unwrap();
        std::fs::write(bots.join(".port-lock"), "").unwrap();
        std::fs::create_dir_all(t.home.trash_dir().join("paper-0123456789abcdef")).unwrap();
        let names: Vec<String> = bot_folders(&t.home).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["b-2", "paper"]);
        assert_eq!(bot_folders(&t.home)[1].1, bots.join("paper"));
    }

    #[test]
    fn no_bot_folder_is_no_bots() {
        let t = TempHome::new();
        assert!(bot_folders(&t.home).is_empty());
    }
}
