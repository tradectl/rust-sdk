//! Two boot clocks, to tell a sleep from a stall (section 6.2). Neither moves
//! when the wall clock is set. `since_boot_ns() - awake_ns()` is the time the
//! computer slept; it only grows.
//!
//! | | awake (no sleep) | since boot (with sleep) |
//! |---|---|---|
//! | Linux | `CLOCK_MONOTONIC` | `CLOCK_BOOTTIME` |
//! | macOS | `CLOCK_UPTIME_RAW` | `CLOCK_MONOTONIC_RAW` |
//! | Windows | `QueryUnbiasedInterruptTime` | `QueryInterruptTime` |
//!
//! Elsewhere both count from the first call and never see a sleep.

/// Nanoseconds the computer was awake since boot; 0 when the OS cannot say.
pub fn awake_ns() -> u64 {
    os::awake_ns()
}

/// Nanoseconds since boot, sleep included; 0 when the OS cannot say.
pub fn since_boot_ns() -> u64 {
    os::since_boot_ns()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod os {
    #[cfg(target_os = "linux")]
    const AWAKE: libc::clockid_t = libc::CLOCK_MONOTONIC;
    #[cfg(target_os = "linux")]
    const SINCE_BOOT: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(target_os = "macos")]
    const AWAKE: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
    #[cfg(target_os = "macos")]
    const SINCE_BOOT: libc::clockid_t = libc::CLOCK_MONOTONIC_RAW;

    fn read(id: libc::clockid_t) -> u64 {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        if unsafe { libc::clock_gettime(id, &mut ts) } != 0 {
            return 0;
        }
        (ts.tv_sec as u64).saturating_mul(1_000_000_000).saturating_add(ts.tv_nsec as u64)
    }

    pub(super) fn awake_ns() -> u64 {
        read(AWAKE)
    }

    pub(super) fn since_boot_ns() -> u64 {
        read(SINCE_BOOT)
    }
}

#[cfg(windows)]
mod os {
    use windows_sys::Win32::System::WindowsProgramming::{QueryInterruptTime, QueryUnbiasedInterruptTime};

    // Both count 100 ns units.
    pub(super) fn awake_ns() -> u64 {
        let mut t = 0u64;
        if unsafe { QueryUnbiasedInterruptTime(&mut t) } == 0 {
            return 0;
        }
        t.saturating_mul(100)
    }

    pub(super) fn since_boot_ns() -> u64 {
        let mut t = 0u64;
        unsafe { QueryInterruptTime(&mut t) };
        t.saturating_mul(100)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod os {
    use std::sync::OnceLock;
    use std::time::Instant;

    fn since_first() -> u64 {
        static T0: OnceLock<Instant> = OnceLock::new();
        T0.get_or_init(Instant::now).elapsed().as_nanos() as u64
    }

    pub(super) fn awake_ns() -> u64 {
        since_first()
    }

    pub(super) fn since_boot_ns() -> u64 {
        since_first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn both_clocks_run_and_sleep_never_goes_negative() {
        // Awake first: since boot read after it is never behind.
        let (a1, b1) = (awake_ns(), since_boot_ns());
        assert!(a1 > 0 && b1 >= a1, "awake {a1}, since boot {b1}");
        std::thread::sleep(Duration::from_millis(20));
        let (a2, b2) = (awake_ns(), since_boot_ns());
        assert!(b2 >= a2, "awake {a2}, since boot {b2}");
        // Both moved about 20 ms; the sleep total did not shrink.
        assert!(a2 - a1 >= 15_000_000 && b2 - b1 >= 15_000_000, "{} {}", a2 - a1, b2 - b1);
        assert!(b2 - a2 + 5_000_000 >= b1 - a1, "slept total shrank: {} then {}", b1 - a1, b2 - a2);
    }
}
