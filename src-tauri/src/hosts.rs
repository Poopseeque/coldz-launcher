//! The Windows hosts file for the sign-in route (0.4.0, signin.rs).
//!
//! `C:\Windows\System32\drivers\etc\hosts` can be written only as administrator. The launcher runs without those
//! rights, so it starts **itself** once more with `--coldz-hosts <file>` through the Windows «Run as administrator»
//! prompt (`ShellExecuteExW`, verb `runas`): the player sees one prompt and confirms. That second copy does exactly
//! one thing (`apply`): reads the block from `<file>` (only the two marker lines and `<loopback ip> <host>` lines
//! pass, signin.rs `valid_block_line`), replaces the launcher's old block in the hosts file with it (or removes it
//! when the file is empty), tells the DNS client to forget its cache, and exits. It never writes anything else into
//! the hosts file and touches no other file.
//!
//! Known side effects, for the README: Windows Defender watches the hosts file (it flags redirects of Microsoft
//! hosts; ours are Rockstar and Cfx.re names), some antiviruses (Kaspersky, Dr.Web) may ask about any change: the
//! player sees the launcher's name. While the block is in, those hosts go through the launcher for every program
//! on the PC, which is why the launcher removes it when the route is turned off or the launcher closes.

use std::path::{Path, PathBuf};

/// The file itself.
pub fn path() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    Path::new(&root).join("System32").join("drivers").join("etc").join("hosts")
}

pub fn read() -> String {
    std::fs::read(path()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

/// The launcher's block now in the hosts file (signin.rs `installed`).
pub fn installed() -> Option<(String, Vec<String>)> {
    crate::signin::installed(&read())
}

/// The elevated copy: `--coldz-hosts <file>`. Exit code 0 = done, 2 = the file is unreadable, 3 = a line in it is
/// not ours, 4 = the hosts file could not be written.
pub fn apply(block_file: &str) -> i32 {
    let Ok(raw) = std::fs::read(block_file) else { return 2 };
    if raw.len() > 16 * 1024 {
        return 3;
    }
    let block = String::from_utf8_lossy(&raw).into_owned();
    let block = block.trim_matches(|c| c == '\r' || c == '\n' || c == ' ' || c == '\u{feff}');
    let new_block = if block.is_empty() {
        None
    } else {
        if !block.lines().all(crate::signin::valid_block_line) {
            return 3;
        }
        let mut b = String::new();
        for line in block.lines() {
            b.push_str(line.trim_end_matches('\r'));
            b.push_str("\r\n");
        }
        if !b.starts_with(crate::signin::BEGIN) || !b.trim_end().ends_with(crate::signin::END) {
            return 3;
        }
        Some(b)
    };
    let file = path();
    let old = std::fs::read(&file).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let new = crate::signin::with_block(&old, new_block.as_deref());
    if new == old {
        flush_dns();
        return 0;
    }
    // some tools leave the file read-only: clear the flag, keep the rest
    if let Ok(meta) = std::fs::metadata(&file) {
        let mut perm = meta.permissions();
        if perm.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(&file, perm);
        }
    }
    if std::fs::write(&file, new.as_bytes()).is_err() {
        return 4;
    }
    flush_dns();
    0
}

/// The DNS client picks hosts changes up by itself; the flush makes it immediate.
fn flush_dns() {
    let mut cmd = std::process::Command::new("ipconfig");
    cmd.arg("/flushdns");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd.output();
}

/// Why the elevated copy did not do its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The player pressed «Нет» on the Windows prompt.
    Cancelled,
    /// Windows could not start the elevated copy, it did not finish in time, or it reported an error.
    Failed,
}

/// Writes `block` (None: remove the launcher's block) through the elevated copy of this exe. Blocks the calling
/// thread for as long as the Windows prompt is open: call it from a blocking task. `work` — a folder the launcher
/// may write to (the block goes to a file there).
#[cfg(windows)]
pub fn request(block: Option<&str>, work: &Path) -> Result<(), Refused> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NO_CONSOLE, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let _ = std::fs::create_dir_all(work);
    let file = work.join("hosts-block.txt");
    std::fs::write(&file, block.unwrap_or_default()).map_err(|_| Refused::Failed)?;
    let exe = std::env::current_exe().map_err(|_| Refused::Failed)?;
    let wide = |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let verb = wide(std::ffi::OsStr::new("runas"));
    let exe_w = wide(exe.as_os_str());
    let params = wide(std::ffi::OsStr::new(&format!("--coldz-hosts \"{}\"", file.display())));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_NO_CONSOLE;
    info.lpVerb = verb.as_ptr();
    info.lpFile = exe_w.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE as i32;
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        let _ = std::fs::remove_file(&file);
        return Err(if err == ERROR_CANCELLED { Refused::Cancelled } else { Refused::Failed });
    }
    if info.hProcess.is_null() {
        let _ = std::fs::remove_file(&file);
        return Err(Refused::Failed);
    }
    let waited = unsafe { WaitForSingleObject(info.hProcess, 120_000) };
    let mut code: u32 = 1;
    unsafe {
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    let _ = std::fs::remove_file(&file);
    if waited == WAIT_OBJECT_0 && code == 0 {
        Ok(())
    } else {
        Err(Refused::Failed)
    }
}

#[cfg(not(windows))]
pub fn request(_block: Option<&str>, _work: &Path) -> Result<(), Refused> {
    Err(Refused::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_refuses_foreign_lines() {
        let dir = std::env::temp_dir().join(format!("coldz-hosts-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("block.txt");
        std::fs::write(&f, "127.0.0.1 evil.example\r\n").unwrap();
        // not between the markers
        assert_eq!(apply(f.to_str().unwrap()), 3);
        std::fs::write(&f, format!("{}\r\n8.8.8.8 scui.rockstargames.com\r\n{}\r\n", crate::signin::BEGIN, crate::signin::END)).unwrap();
        assert_eq!(apply(f.to_str().unwrap()), 3);
        assert_eq!(apply(dir.join("missing.txt").to_str().unwrap()), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
