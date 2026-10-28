//! Windows refused to run or load a file: code integrity (Smart App Control,
//! WDAC, a group policy) or an antivirus (section 8.2). tradectl's files are
//! not signed, so the fix is the user's, and the text must say so instead of
//! a raw loader error.
//!
//! Pure: it reads only the error code, so it is tested on every OS. No
//! Linux or macOS errno is this high.

use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Code integrity or a policy; the Windows error code.
    AppControl(i32),
    /// An antivirus blocked or removed the file; the Windows error code.
    Antivirus(i32),
}

impl Blocked {
    pub fn code(self) -> i32 {
        match self {
            Blocked::AppControl(c) | Blocked::Antivirus(c) => c,
        }
    }
}

/// `ERROR_INVALID_IMAGE_HASH`: the signature cannot be verified.
const INVALID_IMAGE_HASH: i32 = 577;
/// `ERROR_ACCESS_DISABLED_BY_POLICY`: AppLocker, a software restriction policy.
const DISABLED_BY_POLICY: i32 = 1260;
/// `ERROR_SYSTEM_INTEGRITY_ROLLBACK_DETECTED` ..
/// `ERROR_SYSTEM_INTEGRITY_REPUTATION_OFFLINE` (4560 is VSM, not ours).
const INTEGRITY: std::ops::RangeInclusive<i32> = 4550..=4559;
/// `ERROR_SYSTEM_INTEGRITY_REPUTATION_UNFRIENDLY_FILE`, `_UNATTAINABLE`,
/// `_EXPLICIT_DENY_FILE`: Smart App Control.
const REPUTATION: std::ops::RangeInclusive<i32> = 4580..=4582;
/// `ERROR_VIRUS_INFECTED`, `ERROR_VIRUS_DELETED`.
const VIRUS: [i32; 2] = [225, 226];

/// What blocked the file, when `err` says Windows did.
pub fn blocked(err: &io::Error) -> Option<Blocked> {
    blocked_code(err.raw_os_error()?)
}

/// [`blocked`] for a raw Windows error code.
pub fn blocked_code(code: i32) -> Option<Blocked> {
    if code == INVALID_IMAGE_HASH || code == DISABLED_BY_POLICY || INTEGRITY.contains(&code) || REPUTATION.contains(&code)
    {
        Some(Blocked::AppControl(code))
    } else if VIRUS.contains(&code) {
        Some(Blocked::Antivirus(code))
    } else {
        None
    }
}

/// [`blocked`] for the first `io::Error` in `err` or its sources (a
/// `libloading::Error` wraps the one `LoadLibraryExW` gave).
pub fn blocked_in(err: &(dyn std::error::Error + 'static)) -> Option<Blocked> {
    let mut e = Some(err);
    while let Some(cur) = e {
        if let Some(io) = cur.downcast_ref::<io::Error>() {
            return blocked(io);
        }
        e = cur.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_map_to_what_blocked() {
        for (code, want) in [
            (577, Some(Blocked::AppControl(577))),
            (1260, Some(Blocked::AppControl(1260))),
            (4550, Some(Blocked::AppControl(4550))),
            (4551, Some(Blocked::AppControl(4551))),
            (4556, Some(Blocked::AppControl(4556))),
            (4559, Some(Blocked::AppControl(4559))),
            (4580, Some(Blocked::AppControl(4580))),
            (4582, Some(Blocked::AppControl(4582))),
            (225, Some(Blocked::Antivirus(225))),
            (226, Some(Blocked::Antivirus(226))),
            (2, None),
            (5, None),
            (126, None),
            (193, None),
            (4560, None),
            (4583, None),
        ] {
            assert_eq!(blocked(&io::Error::from_raw_os_error(code)), want, "{code}");
        }
        assert_eq!(blocked(&io::Error::other("no code")), None);
        assert_eq!(Blocked::Antivirus(225).code(), 225);
    }

    #[test]
    fn the_io_error_is_found_under_a_wrapper() {
        #[derive(Debug)]
        struct Wrap(io::Error);
        impl std::fmt::Display for Wrap {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "LoadLibraryExW failed")
            }
        }
        impl std::error::Error for Wrap {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let e = Wrap(io::Error::from_raw_os_error(4551));
        assert_eq!(blocked_in(&e), Some(Blocked::AppControl(4551)));
        assert_eq!(blocked_in(&Wrap(io::Error::from_raw_os_error(126))), None);
        assert_eq!(blocked_in(&io::Error::from_raw_os_error(226)), Some(Blocked::Antivirus(226)));
    }
}
