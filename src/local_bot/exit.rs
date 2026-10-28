//! Exit codes of `tradectl run` and `tradectl stop`, and the refusal reasons
//! of `start-result: refused reason=<reason>`. The Lab maps on both.

use super::running::Liveness;

/// A verified bot holds the lock, or a verified legacy bot runs.
pub const ALREADY_RUNNING: i32 = 73;
/// `run`: alive but not ready in time, or the lock is held by a process with
/// no record yet. `stop`: still stopping.
pub const STILL_GOING: i32 = 75;
/// `stop --force` killed it; positions NOT closed.
pub const KILLED: i32 = 76;
/// `stop`: the bot stopped but said positions were NOT closed.
pub const STOPPED_NOT_CLOSED: i32 = 77;
/// Needs the user; see [`reason`].
pub const REFUSED: i32 = 78;

/// `reason=` values of exit [`REFUSED`].
pub mod reason {
    /// The disk cannot lock files, or `~/.tradectl` is on a network drive.
    pub const LOCK_UNSUPPORTED: &str = "lock-unsupported";
    /// `locks/` or a lock file is not this user's (an old `sudo` run).
    pub const LOCK_PERMISSION: &str = "lock-permission";
    /// This folder is a copy of a stopped bot.
    pub const COPY: &str = "copy";
    /// This folder is a copy of a running bot.
    pub const COPY_OF_LIVE: &str = "copy-of-live";
    /// The folder came from another computer.
    pub const MOVED_HOST: &str = "moved-host";
    /// A Delete holds the lock.
    pub const DELETING: &str = "deleting";
    /// Something may run this bot, but it cannot be confirmed.
    pub const MAYBE_RUNNING: &str = "maybe-running";
}

/// A result-line value with no space in it: `%`, space and every byte
/// outside `0x21..=0x7e` become `%XX`. Readers split a line on whitespace,
/// so a path stays one `key=value` word.
pub fn encode_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b == b'%' || !(0x21..=0x7e).contains(&b) {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// [`encode_value`] undone. A `%` without two hex digits after it is kept.
pub fn decode_value(s: &str) -> String {
    let hex = |b: Option<&u8>| b.and_then(|&b| (b as char).to_digit(16));
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let (Some(h), Some(l)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The decoded `key=` value of a `start-result:` or `stop-result:` line;
/// other keys are ignored.
pub fn line_value(line: &str, key: &str) -> Option<String> {
    line.split_whitespace().find_map(|w| w.strip_prefix(key)?.strip_prefix('=')).map(decode_value)
}

/// A start whose lock stayed held: the exit code and, for [`REFUSED`], the
/// reason (section 8.4). `None` when the lock is free after all (try again).
pub fn when_held(l: &Liveness) -> Option<(i32, Option<&'static str>)> {
    Some(match l {
        Liveness::Running(_) => (ALREADY_RUNNING, None),
        Liveness::Legacy(p) if p.verified => (ALREADY_RUNNING, None),
        Liveness::Legacy(_) => (REFUSED, Some(reason::MAYBE_RUNNING)),
        Liveness::Deleting(_) => (REFUSED, Some(reason::DELETING)),
        Liveness::Starting { .. } => (STILL_GOING, None),
        Liveness::Unknown(_) => (REFUSED, Some(reason::MAYBE_RUNNING)),
        Liveness::NotRunning(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_and_stay_one_word() {
        for s in [
            "/Users/x/.tradectl/bot/paper",
            "C:\\Users\\Анна Ш\\.tradectl\\bot\\paper",
            "100% sure",
            "tab\there\nand=equals",
            "",
            "~",
        ] {
            let e = encode_value(s);
            assert!(!e.contains(char::is_whitespace) && e.is_ascii(), "{e}");
            assert_eq!(decode_value(&e), s);
        }
        assert_eq!(encode_value("a b%c"), "a%20b%25c");
        assert_eq!(encode_value("ї"), "%D1%97");
        // A stray `%` from an older writer is kept as it is.
        assert_eq!(decode_value("50%"), "50%");
        assert_eq!(decode_value("%zz%4"), "%zz%4");
    }

    #[test]
    fn a_line_value_is_found_by_its_key() {
        let line = "start-result: refused reason=copy of=/Users/a%20b/.tradectl/bot/paper later=1";
        assert_eq!(line_value(line, "of").as_deref(), Some("/Users/a b/.tradectl/bot/paper"));
        assert_eq!(line_value(line, "reason").as_deref(), Some("copy"));
        assert_eq!(line_value(line, "o"), None, "a key is matched whole");
        assert_eq!(line_value("start-result: refused reason=copy", "of"), None);
        let stop = "stop-result: stopped close_out=unchecked venue=okx market=linear";
        assert_eq!(line_value(stop, "venue").as_deref(), Some("okx"));
    }
}
