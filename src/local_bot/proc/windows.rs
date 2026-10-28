//! Windows: process handles. Never `tasklist`.

use std::path::PathBuf;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, FILETIME, HANDLE,
    UNICODE_STRING, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

use super::{Lookup, ProcInfo};

/// FILETIME of 1970-01-01, in 100 ns units.
const UNIX_EPOCH_FT: u64 = 116_444_736_000_000_000;
const STILL_ACTIVE: u32 = 259;

/// An open process handle, closed on drop.
pub(crate) struct Proc(pub(crate) HANDLE);

impl Drop for Proc {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// `OpenProcess`, or the error code it failed with.
pub(crate) fn open(pid: u32, access: u32) -> Result<Proc, u32> {
    let h = unsafe { OpenProcess(access, 0, pid) };
    if h.is_null() {
        Err(unsafe { GetLastError() })
    } else {
        Ok(Proc(h))
    }
}

/// The creation FILETIME of an open process, as one number.
pub(crate) fn creation(p: &Proc) -> Option<u64> {
    let mut times: [FILETIME; 4] = unsafe { std::mem::zeroed() };
    let [c, e, k, u] = &mut times;
    if unsafe { GetProcessTimes(p.0, c, e, k, u) } == 0 {
        return None;
    }
    Some(((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
}

pub(crate) fn token(ft: u64) -> String {
    format!("win:{ft}")
}

fn alive(p: &Proc, synchronize: bool) -> bool {
    if synchronize {
        return unsafe { WaitForSingleObject(p.0, 0) } == WAIT_TIMEOUT;
    }
    let mut code = 0u32;
    let ok = unsafe { GetExitCodeProcess(p.0, &mut code) } != 0;
    ok && code == STILL_ACTIVE
}

pub(super) fn lookup(pid: u32, full: bool) -> Lookup {
    let (p, sync) = match open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE) {
        Ok(p) => (p, true),
        // Some protected processes refuse SYNCHRONIZE alone.
        Err(ERROR_ACCESS_DENIED) => match open(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
            Ok(p) => (p, false),
            Err(ERROR_INVALID_PARAMETER) => return Lookup::Gone,
            Err(_) => return Lookup::Denied,
        },
        Err(ERROR_INVALID_PARAMETER) => return Lookup::Gone,
        Err(_) => return Lookup::Denied,
    };
    if !alive(&p, sync) {
        return Lookup::Gone;
    }
    let Some(ft) = creation(&p) else { return Lookup::Denied };
    Lookup::Alive(ProcInfo {
        start: token(ft),
        start_unix_ms: ft.checked_sub(UNIX_EPOCH_FT).map(|t| (t / 10_000) as i64),
        argv: full.then(|| argv(&p)).flatten(),
        exe: full.then(|| exe(&p)).flatten(),
    })
}

fn exe(p: &Proc) -> Option<PathBuf> {
    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    if unsafe { QueryFullProcessImageNameW(p.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// `NtQueryInformationProcess(ProcessCommandLineInformation)` (Windows 8.1+,
/// `PROCESS_QUERY_LIMITED_INFORMATION` is enough), then `CommandLineToArgvW`.
fn argv(p: &Proc) -> Option<Vec<String>> {
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    // u64 cells keep the UNICODE_STRING at the buffer start aligned.
    let mut buf = vec![0u64; 512];
    let mut ret = 0u32;
    let mut status = unsafe {
        NtQueryInformationProcess(
            p.0,
            ProcessCommandLineInformation,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut ret,
        )
    };
    if status == STATUS_INFO_LENGTH_MISMATCH && ret > 0 {
        buf = vec![0u64; (ret as usize).div_ceil(8)];
        status = unsafe {
            NtQueryInformationProcess(
                p.0,
                ProcessCommandLineInformation,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut ret,
            )
        };
    }
    if status < 0 {
        return None;
    }
    let us = unsafe { &*(buf.as_ptr() as *const UNICODE_STRING) };
    if us.Buffer.is_null() || us.Length == 0 {
        return None;
    }
    let mut line: Vec<u16> = unsafe { std::slice::from_raw_parts(us.Buffer, us.Length as usize / 2) }.to_vec();
    line.push(0);
    split_command_line(&line)
}

/// `CommandLineToArgvW` over a NUL-terminated, non-empty command line (an
/// empty one would return this process's own path).
fn split_command_line(line: &[u16]) -> Option<Vec<String>> {
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;
    let mut argc = 0i32;
    let list = unsafe { CommandLineToArgvW(line.as_ptr(), &mut argc) };
    if list.is_null() {
        return None;
    }
    let mut out = Vec::with_capacity(argc.max(0) as usize);
    for i in 0..argc.max(0) as usize {
        let arg = unsafe { *list.add(i) };
        let mut n = 0;
        while unsafe { *arg.add(n) } != 0 {
            n += 1;
        }
        out.push(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(arg, n) }));
    }
    unsafe { LocalFree(list as _) };
    (!out.is_empty()).then_some(out)
}

pub(super) fn boot_unix_ms() -> Option<i64> {
    let up = unsafe { GetTickCount64() } as i64;
    Some(crate::local_bot::now_ms() - up)
}
