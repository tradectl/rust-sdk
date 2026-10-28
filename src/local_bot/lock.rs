//! One OS lock per bot: `locks/<bot>.lock`, held with `File::try_lock`.
//!
//! Unix: `flock(2)`, freed when the last fd closes, so also on kill -9 and
//! power loss. Windows: `LockFileEx`, mandatory, so the lock file stays empty
//! and is never read; the holder record is a separate file. Both: a second
//! handle in the same process conflicts with the first, so the holder never
//! probes its own lock.
//!
//! A lock file is never deleted: with `flock`, deleting a held lock file lets
//! the next locker create a new inode at the same path, and then two
//! processes "hold" the lock.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use super::names::valid_bot_name;
use super::Home;

/// How often `acquire` retries while the lock is held.
const RETRY: Duration = Duration::from_millis(50);
const NETWORK_DRIVE: &str = "~/.tradectl is on a network drive";

/// The bot's lock, held for the life of the holder (or a Delete, or an
/// import). Never pass its handle to a child: std opens it `O_CLOEXEC` /
/// non-inheritable, keep it so.
#[derive(Debug)]
pub struct BotLock {
    file: File,
    name: String,
    home: Home,
}

#[derive(Debug)]
pub enum LockError {
    /// Another handle still held it when the wait ran out.
    Held,
    /// This disk cannot lock files (NFS without lockd, some SMB/FUSE, a
    /// platform without `flock`), or `~/.tradectl` is on a network drive.
    Unsupported(io::Error),
    /// `locks/` or the lock file is not this user's (an old `sudo` run).
    Permission(io::Error),
    Io(io::Error),
}

impl LockError {
    /// The `start-result: refused reason=` value; `None` for [`LockError::Held`],
    /// which the caller decides with `bot_running` (section 8.4).
    pub fn reason(&self) -> Option<&'static str> {
        use super::exit::reason;
        match self {
            LockError::Held => None,
            LockError::Permission(_) => Some(reason::LOCK_PERMISSION),
            LockError::Unsupported(_) | LockError::Io(_) => Some(reason::LOCK_UNSUPPORTED),
        }
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Held => write!(f, "another process holds the bot's lock"),
            LockError::Permission(e) => {
                write!(f, "Cannot open {e}. Was tradectl once run with sudo? Give ~/.tradectl/locks back to your user.")
            }
            LockError::Unsupported(e) if e.to_string().starts_with(NETWORK_DRIVE) => {
                write!(f, "{e}. Move ~/.tradectl to a local disk.")
            }
            LockError::Unsupported(e) | LockError::Io(e) => {
                write!(f, "This folder is on a disk that cannot lock files ({e}). Move ~/.tradectl to a local disk.")
            }
        }
    }
}

impl std::error::Error for LockError {}

impl BotLock {
    /// Take bot `name`'s lock, exclusive. Retries every 50 ms until `wait`
    /// (callers use 5 s: Windows frees a dead process's lock a moment late).
    /// Creates `locks/` (0700) and refuses a network drive first.
    pub fn acquire(home: &Home, name: &str, wait: Duration) -> Result<BotLock, LockError> {
        if !valid_bot_name(name) {
            return Err(LockError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("'{name}' is not a bot name"),
            )));
        }
        let file = lock_file(home, &home.lock_path(name), wait)?;
        Ok(BotLock { file, name: name.to_string(), home: home.clone() })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    /// Unlock, then close. Call before `process::exit`, which skips
    /// destructors.
    pub fn release(self) {
        let _ = self.file.unlock();
    }

    /// `bot/<bot>/bot.pid` = this process, temp + rename. Only the holder
    /// writes it; the format is plain decimal so v0.2.7 readers still work.
    pub fn write_pid(&self) -> io::Result<()> {
        let path = self.home.pid_path(&self.name);
        super::fsx::write_atomic(&path, std::process::id().to_string().as_bytes(), false)
    }

    /// Remove `bot.pid` when it names this process (a clean exit).
    pub fn remove_own_pid(&self) {
        let path = self.home.pid_path(&self.name);
        let mine =
            std::fs::read_to_string(&path).is_ok_and(|s| s.trim().parse::<u32>().ok() == Some(std::process::id()));
        if mine {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// `try_lock_shared` then `unlock` at once.
#[derive(Debug)]
pub enum LockProbe {
    /// No lock file: no v0.2.8 process ever ran this bot here.
    Absent,
    Free,
    Held,
    Error(io::Error),
}

/// Probe bot `name`'s lock. Opens read-only and never creates the file. A
/// starting bot retries its own `acquire` for 5 s, so the microseconds a
/// probe holds the shared lock never decide anything.
pub fn probe(home: &Home, name: &str) -> LockProbe {
    if !valid_bot_name(name) {
        return LockProbe::Absent;
    }
    let file = match open(&home.lock_path(name), false) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return LockProbe::Absent,
        Err(e) => return LockProbe::Error(e),
    };
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            LockProbe::Free
        }
        Err(TryLockError::WouldBlock) => LockProbe::Held,
        Err(TryLockError::Error(e)) => LockProbe::Error(e),
    }
}

/// `locks/.ports.lock`: one port resolution at a time on this computer.
#[derive(Debug)]
pub struct PortsLock(File);

impl PortsLock {
    pub fn acquire(home: &Home, wait: Duration) -> Result<PortsLock, LockError> {
        lock_file(home, &home.ports_lock_path(), wait).map(PortsLock)
    }

    pub fn release(self) {
        let _ = self.0.unlock();
    }
}

fn lock_file(home: &Home, path: &Path, wait: Duration) -> Result<File, LockError> {
    let dir = home.locks_dir();
    make_locks_dir(home, &dir)?;
    refuse_network(&dir)?;
    let deadline = Instant::now() + wait;
    let mut file: Option<File> = None;
    loop {
        let last = match file.take().map(Ok).unwrap_or_else(|| open(path, true)) {
            Ok(f) => match f.try_lock() {
                Ok(()) => return Ok(f),
                Err(TryLockError::WouldBlock) => {
                    file = Some(f);
                    LockError::Held
                }
                Err(TryLockError::Error(e)) => return Err(classify(with_path(path, e))),
            },
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Err(LockError::Permission(with_path(path, e)))
            }
            // Antivirus, a signal: try again inside the same window.
            Err(e) if transient_open_error(&e) => LockError::Io(with_path(path, e)),
            Err(e) => return Err(LockError::Io(with_path(path, e))),
        };
        let now = Instant::now();
        if now >= deadline {
            return Err(last);
        }
        std::thread::sleep(RETRY.min(deadline - now));
    }
}

fn make_locks_dir(home: &Home, dir: &Path) -> Result<(), LockError> {
    let fail = |e: io::Error| {
        if e.kind() == io::ErrorKind::PermissionDenied {
            LockError::Permission(with_path(dir, e))
        } else {
            LockError::Io(with_path(dir, e))
        }
    };
    std::fs::create_dir_all(home.root()).map_err(fail)?;
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    match b.create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(e) => Err(fail(e)),
    }
}

/// Read-write (and create) for a locker, read-only for a probe. Windows:
/// no `FILE_SHARE_DELETE`, so nobody can delete or rename the file while a
/// handle is open.
fn open(path: &Path, write: bool) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    if write {
        o.write(true).create(true).truncate(false);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        o.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    o.open(path)
}

fn transient_open_error(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::ResourceBusy)
        || (cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)))
}

/// The error text with the path in front; the kind is kept.
fn with_path(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// A `try_lock` error: `Unsupported` when the disk cannot lock at all.
pub(crate) fn classify(e: io::Error) -> LockError {
    if is_unsupported(&e) {
        LockError::Unsupported(e)
    } else {
        LockError::Io(e)
    }
}

fn is_unsupported(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    let codes = [libc::ENOLCK, libc::EOPNOTSUPP, libc::ENOTSUP];
    // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED
    #[cfg(windows)]
    let codes = [1, 50];
    #[cfg(not(any(unix, windows)))]
    let codes: [i32; 0] = [];
    e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

/// What kind of filesystem a folder is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsKind {
    Local,
    /// A network filesystem, by name.
    Network(String),
    /// Could not tell; treated as local.
    Unknown,
}

/// Linux `statfs.f_type`, macOS `statfs.f_fstypename`, Windows
/// `GetDriveTypeW`. FUSE is local (encrypted homes use it).
pub fn fs_kind(path: &Path) -> FsKind {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return FsKind::Unknown };
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return FsKind::Unknown;
        }
        linux_fs_kind(st.f_type as u32)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return FsKind::Unknown };
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return FsKind::Unknown;
        }
        let name: Vec<u8> = st.f_fstypename.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        macos_fs_kind(&String::from_utf8_lossy(&name))
    }
    #[cfg(windows)]
    {
        windows_fs_kind_of(path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = path;
        FsKind::Unknown
    }
}

/// Refuse a network drive before the first lock (owner, O5). On NFS even
/// with lockd, Linux emulates `flock` with POSIX locks: two handles in one
/// process do not conflict and any close drops the process's lock.
pub fn refuse_network(dir: &Path) -> Result<(), LockError> {
    match fs_kind(dir) {
        FsKind::Network(fs) => Err(LockError::Unsupported(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{NETWORK_DRIVE} ({}: {fs})", dir.display()),
        ))),
        FsKind::Local | FsKind::Unknown => Ok(()),
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn linux_fs_kind(f_type: u32) -> FsKind {
    match f_type {
        0x6969 => FsKind::Network("nfs".into()),
        0x517B => FsKind::Network("smb".into()),
        0xFF53_4D42 => FsKind::Network("cifs".into()),
        0xFE53_4D42 => FsKind::Network("smb2".into()),
        _ => FsKind::Local,
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn macos_fs_kind(name: &str) -> FsKind {
    match name {
        "nfs" | "smbfs" | "afpfs" | "webdav" => FsKind::Network(name.into()),
        "" => FsKind::Unknown,
        _ => FsKind::Local,
    }
}

/// `GetDriveTypeW` result: `DRIVE_REMOTE` (4) is a network drive.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn windows_fs_kind(drive_type: u32) -> FsKind {
    match drive_type {
        4 => FsKind::Network("remote drive".into()),
        // DRIVE_UNKNOWN, DRIVE_NO_ROOT_DIR
        0 | 1 => FsKind::Unknown,
        _ => FsKind::Local,
    }
}

#[cfg(windows)]
fn windows_fs_kind_of(path: &Path) -> FsKind {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};
    let Some(Component::Prefix(p)) = path.components().next() else { return FsKind::Unknown };
    let root = match p.kind() {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:\\", d as char),
        Prefix::UNC(..) | Prefix::VerbatimUNC(..) => return FsKind::Network("unc".into()),
        _ => return FsKind::Unknown,
    };
    let wide: Vec<u16> = std::ffi::OsStr::new(&root).encode_wide().chain(Some(0)).collect();
    let t = unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(wide.as_ptr()) };
    windows_fs_kind(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_bot::testutil::{child_env, idle_until_stopped, respawn, wait_for, ChildGuard, TempHome};

    const CHILD: &str = "local_bot::lock::tests::child_holds_lock";

    /// Child side of the cross-process tests: hold the lock, then idle.
    /// `TRADECTL_SDK_TEST_PID=1` also writes `bot.pid`, as a holder does;
    /// `TRADECTL_SDK_TEST_SPAWN=1` first starts an idle grandchild.
    #[test]
    #[ignore]
    fn child_holds_lock() {
        let Some((home, name)) = child_env() else { return };
        let lock = BotLock::acquire(&home, &name, Duration::from_secs(5)).expect("child lock");
        if std::env::var("TRADECTL_SDK_TEST_PID").is_ok() {
            lock.write_pid().unwrap();
        }
        if std::env::var("TRADECTL_SDK_TEST_SPAWN").is_ok() {
            let g = respawn("local_bot::lock::tests::grandchild_idles", &home, &name).spawn().unwrap();
            std::fs::write(home.root().join("grandchild.pid"), g.id().to_string()).unwrap();
            std::mem::forget(g);
        }
        std::fs::write(home.root().join("child.ready"), "").unwrap();
        idle_until_stopped(&home, "child");
        drop(lock);
    }

    /// Grandchild: idles while its parent, the holder, exits.
    #[test]
    #[ignore]
    fn grandchild_idles() {
        let Some((home, _)) = child_env() else { return };
        idle_until_stopped(&home, "grandchild");
    }

    fn holder_child(t: &TempHome, name: &str, extra: &[(&str, &str)]) -> ChildGuard {
        let mut cmd = respawn(CHILD, &t.home, name);
        cmd.envs(extra.iter().copied());
        let g = ChildGuard(cmd.spawn().unwrap());
        assert!(wait_for(&t.home.root().join("child.ready"), Duration::from_secs(20)), "child took the lock");
        g
    }

    #[test]
    fn a_second_handle_in_another_process_sees_held() {
        let t = TempHome::new();
        let _child = holder_child(&t, "paper", &[]);
        assert!(matches!(probe(&t.home, "paper"), LockProbe::Held));
        let t0 = Instant::now();
        assert!(matches!(BotLock::acquire(&t.home, "paper", Duration::from_millis(300)), Err(LockError::Held)));
        assert!(t0.elapsed() >= Duration::from_millis(250), "it waited");
    }

    #[test]
    fn a_second_handle_in_this_process_conflicts_too() {
        let t = TempHome::new();
        let lock = BotLock::acquire(&t.home, "paper", Duration::ZERO).unwrap();
        assert!(matches!(BotLock::acquire(&t.home, "paper", Duration::ZERO), Err(LockError::Held)));
        lock.release();
        BotLock::acquire(&t.home, "paper", Duration::ZERO).unwrap().release();
    }

    #[test]
    fn kill_then_acquire_within_5s() {
        let t = TempHome::new();
        let mut child = holder_child(&t, "paper", &[]);
        child.0.kill().unwrap();
        let t0 = Instant::now();
        let lock = BotLock::acquire(&t.home, "paper", Duration::from_secs(5)).expect("lock after kill");
        assert!(t0.elapsed() < Duration::from_secs(5));
        lock.release();
    }

    #[test]
    fn a_child_of_the_holder_does_not_keep_the_lock() {
        let t = TempHome::new();
        let mut child = holder_child(&t, "paper", &[("TRADECTL_SDK_TEST_SPAWN", "1")]);
        let gpid: u32 = std::fs::read_to_string(t.home.root().join("grandchild.pid")).unwrap().parse().unwrap();
        // The grandchild is ours too: stop it however the test ends.
        struct Grandchild<'a>(&'a Home, u32);
        impl Drop for Grandchild<'_> {
            fn drop(&mut self) {
                let _ = std::fs::write(self.0.root().join("grandchild.stop"), "");
                let start = match crate::local_bot::proc::lookup(self.1) {
                    crate::local_bot::proc::Lookup::Alive(p) => p.start,
                    _ => return,
                };
                let _ = crate::local_bot::signal::kill(&crate::local_bot::signal::Target { pid: self.1, start });
            }
        }
        let _g = Grandchild(&t.home, gpid);
        std::fs::write(t.home.root().join("child.stop"), "").unwrap();
        child.0.wait().unwrap();
        assert!(
            matches!(crate::local_bot::proc::lookup(gpid), crate::local_bot::proc::Lookup::Alive(_)),
            "the grandchild still runs"
        );
        let lock = BotLock::acquire(&t.home, "paper", Duration::from_secs(5)).expect("the lock is free");
        lock.release();
    }

    #[test]
    fn probe_never_creates_the_file() {
        let t = TempHome::new();
        assert!(matches!(probe(&t.home, "paper"), LockProbe::Absent));
        assert!(!t.home.lock_path("paper").exists());
        assert!(!t.home.locks_dir().exists());
        assert!(matches!(probe(&t.home, "../x"), LockProbe::Absent), "not a bot name");
    }

    #[test]
    fn probe_is_free_after_release() {
        let t = TempHome::new();
        let lock = BotLock::acquire(&t.home, "paper", Duration::ZERO).unwrap();
        lock.release();
        assert!(matches!(probe(&t.home, "paper"), LockProbe::Free));
        // A probe leaves it free for the next locker.
        BotLock::acquire(&t.home, "paper", Duration::ZERO).unwrap().release();
        assert!(t.home.lock_path("paper").exists(), "never deleted");
    }

    #[cfg(unix)]
    #[test]
    fn the_locks_folder_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempHome::new();
        BotLock::acquire(&t.home, "paper", Duration::ZERO).unwrap().release();
        let mode = std::fs::metadata(t.home.locks_dir()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn ports_lock_is_exclusive() {
        let t = TempHome::new();
        let p = PortsLock::acquire(&t.home, Duration::ZERO).unwrap();
        assert!(matches!(PortsLock::acquire(&t.home, Duration::from_millis(60)), Err(LockError::Held)));
        p.release();
        PortsLock::acquire(&t.home, Duration::ZERO).unwrap().release();
    }

    #[test]
    fn a_bad_name_is_refused() {
        let t = TempHome::new();
        let e = BotLock::acquire(&t.home, "../etc", Duration::ZERO).unwrap_err();
        assert!(matches!(e, LockError::Io(ref e) if e.kind() == io::ErrorKind::InvalidInput));
    }

    #[test]
    fn unsupported_maps_to_refusal() {
        use crate::local_bot::exit::reason;
        assert!(matches!(classify(io::Error::from(io::ErrorKind::Unsupported)), LockError::Unsupported(_)));
        #[cfg(unix)]
        for code in [libc::ENOLCK, libc::EOPNOTSUPP, libc::ENOTSUP] {
            assert!(matches!(classify(io::Error::from_raw_os_error(code)), LockError::Unsupported(_)), "{code}");
        }
        #[cfg(windows)]
        for code in [1, 50] {
            assert!(matches!(classify(io::Error::from_raw_os_error(code)), LockError::Unsupported(_)), "{code}");
        }
        let other = classify(io::Error::other("disk on fire"));
        assert!(matches!(other, LockError::Io(_)));
        // Both refuse the start with the same reason.
        assert_eq!(other.reason(), Some(reason::LOCK_UNSUPPORTED));
        assert_eq!(classify(io::Error::from(io::ErrorKind::Unsupported)).reason(), Some(reason::LOCK_UNSUPPORTED));
        let perm = LockError::Permission(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(perm.reason(), Some(reason::LOCK_PERMISSION));
        assert!(perm.to_string().contains("sudo"), "{perm}");
        assert!(!perm.to_string().contains("cannot lock files"));
        assert_eq!(LockError::Held.reason(), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_locks_folder_of_another_user_is_a_permission_refusal() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // root opens anything
        }
        let t = TempHome::new();
        std::fs::create_dir_all(t.home.locks_dir()).unwrap();
        std::fs::set_permissions(t.home.locks_dir(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let e = BotLock::acquire(&t.home, "paper", Duration::ZERO).unwrap_err();
        std::fs::set_permissions(t.home.locks_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(e, LockError::Permission(_)), "{e:?}");
    }

    #[test]
    fn network_drive_is_refused() {
        assert_eq!(linux_fs_kind(0x6969), FsKind::Network("nfs".into()));
        assert_eq!(linux_fs_kind(0x517B), FsKind::Network("smb".into()));
        assert_eq!(linux_fs_kind(0xFF53_4D42), FsKind::Network("cifs".into()));
        assert_eq!(linux_fs_kind(0xFE53_4D42), FsKind::Network("smb2".into()));
        // A signed 32-bit f_type (some arches) truncates to the same magic.
        assert_eq!(linux_fs_kind(0xFF53_4D42_u32 as i32 as i64 as u32), FsKind::Network("cifs".into()));
        assert_eq!(linux_fs_kind(0xEF53), FsKind::Local, "ext4");
        assert_eq!(linux_fs_kind(0x6573_5546), FsKind::Local, "fuse");
        for n in ["nfs", "smbfs", "afpfs", "webdav"] {
            assert_eq!(macos_fs_kind(n), FsKind::Network(n.into()));
        }
        assert_eq!(macos_fs_kind("apfs"), FsKind::Local);
        assert_eq!(macos_fs_kind("macfuse"), FsKind::Local);
        assert_eq!(windows_fs_kind(4), FsKind::Network("remote drive".into()));
        assert_eq!(windows_fs_kind(3), FsKind::Local, "DRIVE_FIXED");
        assert_eq!(windows_fs_kind(2), FsKind::Local, "DRIVE_REMOVABLE");
        // The refusal maps to lock-unsupported, with its own text.
        let net = LockError::Unsupported(io::Error::new(io::ErrorKind::Unsupported, format!("{NETWORK_DRIVE} (/n: nfs)")));
        assert_eq!(net.reason(), Some(crate::local_bot::exit::reason::LOCK_UNSUPPORTED));
        assert_eq!(net.to_string(), "~/.tradectl is on a network drive (/n: nfs). Move ~/.tradectl to a local disk.");
        let t = TempHome::new();
        assert!(refuse_network(t.home.root()).is_ok(), "the temp dir is local");
        assert_ne!(fs_kind(t.home.root()), FsKind::Network(String::new()));
    }

    /// L2: on Unix an `rm` of a held lock file lets a second locker in (a
    /// new inode); the holder's `bot.pid` is the backstop.
    #[cfg(unix)]
    #[test]
    fn rm_of_a_held_file_is_caught() {
        use std::os::unix::process::CommandExt;
        let t = TempHome::new();
        t.bot("paper");
        // The holder looks like `tradectl run paper` to the argv check.
        let mut cmd = respawn(CHILD, &t.home, "paper");
        cmd.arg0("tradectl").args(["run", "paper"]).env("TRADECTL_SDK_TEST_PID", "1");
        let child = ChildGuard(cmd.spawn().unwrap());
        assert!(wait_for(&t.home.root().join("child.ready"), Duration::from_secs(20)));
        std::fs::remove_file(t.home.lock_path("paper")).unwrap();
        let second = BotLock::acquire(&t.home, "paper", Duration::ZERO).expect("a new inode: the lock is no guard now");
        let legacy = crate::local_bot::legacy::check(&t.home, "paper").expect("bot.pid names a live copy");
        assert_eq!(legacy.pid, child.pid());
        assert!(legacy.verified, "{legacy:?}");
        // The start refuses: a copy runs.
        assert_eq!(
            crate::local_bot::exit::when_held(&crate::local_bot::Liveness::Legacy(legacy)),
            Some((crate::local_bot::exit::ALREADY_RUNNING, None))
        );
        second.release();
    }
}
