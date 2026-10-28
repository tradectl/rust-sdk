//! macOS: `proc_pidinfo` and `sysctl`.
//!
//! `PROC_PIDUNIQIDENTIFIERINFO` works for any user's process; the BSD info
//! (zombie state, start time) only for this user's.

use std::ffi::c_void;
use std::path::PathBuf;

use super::{parse_procargs2, Lookup, ProcInfo};

/// `<sys/proc_info.h>`; not in libc.
const PROC_PIDUNIQIDENTIFIERINFO: libc::c_int = 17;

#[repr(C)]
struct ProcUniqIdentifierInfo {
    p_uuid: [u8; 16],
    p_uniqueid: u64,
    p_puniqueid: u64,
    p_idversion: i32,
    p_orig_ppidversion: i32,
    p_reserve2: u64,
    p_reserve3: u64,
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `proc_pidinfo(pid, flavor)` into a `T`; `Err(errno)` on failure.
fn pidinfo<T>(pid: u32, flavor: libc::c_int) -> Result<T, i32> {
    let mut out: T = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<T>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, flavor, 0, &mut out as *mut T as *mut c_void, size) };
    if n == size {
        Ok(out)
    } else if n <= 0 {
        Err(errno())
    } else {
        Err(libc::EINVAL)
    }
}

pub(super) fn lookup(pid: u32, full: bool) -> Lookup {
    let bsd = pidinfo::<libc::proc_bsdinfo>(pid, libc::PROC_PIDTBSDINFO);
    if let Ok(b) = &bsd {
        if b.pbi_status == libc::SZOMB {
            return Lookup::Gone;
        }
    }
    let uniq = match pidinfo::<ProcUniqIdentifierInfo>(pid, PROC_PIDUNIQIDENTIFIERINFO) {
        Ok(u) => u,
        Err(libc::ESRCH) => return Lookup::Gone,
        // A zombie has no task to ask; its BSD info may still read.
        Err(_) if matches!(bsd, Err(libc::ESRCH)) => return Lookup::Gone,
        Err(_) => return Lookup::Denied,
    };
    Lookup::Alive(ProcInfo {
        start: format!("mac:{}", uniq.p_uniqueid),
        start_unix_ms: bsd.ok().map(|b| b.pbi_start_tvsec as i64 * 1000 + b.pbi_start_tvusec as i64 / 1000),
        argv: full.then(|| argv(pid)).flatten(),
        exe: full.then(|| exe(pid)).flatten(),
    })
}

fn argv(pid: u32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut argmax: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    let r = unsafe {
        libc::sysctl(mib.as_mut_ptr(), 2, &mut argmax as *mut _ as *mut c_void, &mut len, std::ptr::null_mut(), 0)
    };
    if r != 0 || argmax <= 0 {
        return None;
    }
    let mut buf = vec![0u8; argmax as usize];
    let mut size = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let r = unsafe {
        libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut c_void, &mut size, std::ptr::null_mut(), 0)
    };
    if r != 0 {
        return None;
    }
    parse_procargs2(&buf[..size])
}

fn exe(pid: u32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(PathBuf::from(String::from_utf8_lossy(&buf).into_owned()))
}

fn sysctl_string(name: &str) -> Option<String> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut size = 0usize;
    if unsafe { libc::sysctlbyname(cname.as_ptr(), std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    if unsafe {
        libc::sysctlbyname(cname.as_ptr(), buf.as_mut_ptr() as *mut c_void, &mut size, std::ptr::null_mut(), 0)
    } != 0
    {
        return None;
    }
    buf.truncate(size);
    let s = String::from_utf8_lossy(&buf).trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn boottime() -> Option<libc::timeval> {
    let cname = std::ffi::CString::new("kern.boottime").ok()?;
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::timeval>();
    let r = unsafe {
        libc::sysctlbyname(cname.as_ptr(), &mut tv as *mut _ as *mut c_void, &mut size, std::ptr::null_mut(), 0)
    };
    (r == 0 && tv.tv_sec > 0).then_some(tv)
}

pub(super) fn boot_id() -> Option<String> {
    sysctl_string("kern.bootsessionuuid")
        .or_else(|| boottime().map(|tv| format!("boottime:{}.{}", tv.tv_sec, tv.tv_usec)))
}

pub(super) fn boot_unix_ms() -> Option<i64> {
    boottime().map(|tv| tv.tv_sec * 1000 + tv.tv_usec as i64 / 1000)
}
