//! systemd unit files (Linux). A bot with one is the service's: Delete
//! refuses it and offers `tradectl service remove <bot>`.

use std::path::PathBuf;

/// `tradectl-<bot>.service`.
pub fn unit_name(bot: &str) -> String {
    format!("tradectl-{bot}.service")
}

/// The unit files that exist for bot `name`: the user's
/// (`$XDG_CONFIG_HOME/systemd/user/`, `~/.config/systemd/user/`) and the
/// system's (`/etc/systemd/system/`). Empty off Linux.
pub fn unit_files(name: &str) -> Vec<PathBuf> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    candidates(name, dirs::home_dir(), std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .into_iter()
        .filter(|p| p.is_file())
        .collect()
}

fn candidates(name: &str, home: Option<PathBuf>, xdg: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(x) = xdg.filter(|p| p.is_absolute()) {
        dirs.push(x.join("systemd/user"));
    }
    if let Some(h) = home {
        let d = h.join(".config/systemd/user");
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.push(PathBuf::from("/etc/systemd/system"));
    let file = unit_name(name);
    dirs.into_iter().map(|d| d.join(&file)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_and_system_places() {
        let c = candidates("paper", Some("/home/u".into()), Some("/cfg".into()));
        assert_eq!(
            c,
            [
                PathBuf::from("/cfg/systemd/user/tradectl-paper.service"),
                PathBuf::from("/home/u/.config/systemd/user/tradectl-paper.service"),
                PathBuf::from("/etc/systemd/system/tradectl-paper.service"),
            ]
        );
        // A relative XDG_CONFIG_HOME is ignored, as systemd does; no duplicates.
        assert_eq!(candidates("paper", Some("/home/u".into()), Some("rel".into())).len(), 2);
        assert_eq!(candidates("paper", Some("/home/u".into()), Some("/home/u/.config".into())).len(), 2);
        if !cfg!(target_os = "linux") {
            assert!(unit_files("paper").is_empty());
        }
    }
}
