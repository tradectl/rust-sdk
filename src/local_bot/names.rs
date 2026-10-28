//! Bot names.

/// The watchdog's name (`tradectl service install watchdog`).
pub const WATCHDOG: &str = "watchdog";

/// A bot name: `[a-z0-9_-]{1,32}`, not starting with `-` (it would read as
/// an option), not `watchdog`. The CLI's rule. A folder under `bot/` with any
/// other name is not a bot.
pub fn valid_bot_name(s: &str) -> bool {
    s != WATCHDOG
        && !s.is_empty()
        && s.len() <= 32
        && !s.starts_with('-')
        && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}

/// Names a NEW bot may not take: `manual`, `autostart`, names starting
/// `watchdog`, Windows device names. An existing folder with such a name
/// still runs.
pub fn reserved_for_new(name: &str) -> bool {
    const DEVICES: [&str; 4] = ["con", "prn", "aux", "nul"];
    let port = |p: &str| name.strip_prefix(p).is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'9'));
    name == "manual"
        || name == "autostart"
        || name.starts_with(WATCHDOG)
        || DEVICES.contains(&name)
        || port("com")
        || port("lpt")
}

/// A name the CLI skeleton and the Lab's New bot form accept.
pub fn valid_new_bot_name(name: &str) -> bool {
    valid_bot_name(name) && !reserved_for_new(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names_follow_the_cli_rule() {
        for ok in ["paper", "a", "bot-1", "b_2", "x".repeat(32).as_str(), "manual", "con"] {
            assert!(valid_bot_name(ok), "{ok}");
        }
        for bad in ["", "-x", "Paper", "paper copy", "a.b", "a/b", "watchdog", &"x".repeat(33), "é"] {
            assert!(!valid_bot_name(bad), "{bad}");
        }
    }

    #[test]
    fn reserved_only_for_new() {
        for r in [
            "manual",
            "autostart",
            "watchdog2",
            "watchdog-x",
            "con",
            "prn",
            "aux",
            "nul",
            "com1",
            "com9",
            "lpt1",
            "lpt9",
        ] {
            assert!(reserved_for_new(r), "{r}");
            assert!(!valid_new_bot_name(r), "{r}");
        }
        // Existing folders with these names are still bots.
        for r in ["manual", "autostart", "watchdog2", "con", "com1"] {
            assert!(valid_bot_name(r), "{r}");
        }
        for ok in ["paper", "com", "com0", "com10", "lpt", "console", "manually", "my-watchdog"] {
            assert!(!reserved_for_new(ok), "{ok}");
            assert!(valid_new_bot_name(ok), "{ok}");
        }
    }
}
