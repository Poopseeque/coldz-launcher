//! The Windows hosts file for the sign-in through the COLDZ server (0.4.0, signin.rs).
//!
//! `C:\Windows\System32\drivers\etc\hosts` can be written only as administrator. The launcher runs without those
//! rights, so it starts **itself** once more with `--coldz-hosts <file>` through the Windows «Run as administrator»
//! prompt (`ShellExecuteExW`, verb `runas`): the player sees one prompt and confirms. That second copy reads the block
//! from `<file>` (only the two marker lines and `<loopback ip> <host>` lines pass, signin.rs `valid_block_line`),
//! replaces the launcher's old block in the hosts file with it (or removes it when the file is empty), tells the DNS
//! client to forget its cache. It never writes anything else into the hosts file and touches no other file.
//!
//! 0.5.7, one prompt instead of two (`guard`): with `--coldz-watch <pid> <created> <name>` the elevated copy stays and
//! removes the block itself, so closing the launcher asks nothing. It ends, always removing the block first, when
//!   * the launcher's process ends, whatever the reason (closed, crashed, killed, updated): the copy holds its handle;
//!   * the launcher asks (the «stop» event: «Выключить» in the window, an update about to be installed);
//!   * Windows logs off or shuts down (a hidden window gets `WM_ENDSESSION`).
//! Nothing starts it again after a reboot: there is no autostart, no service, no scheduled task. What it checks before
//! it writes anything: the process `<pid>` runs this very exe and was started at `<created>` (so a reused pid, another
//! program or a later start of the launcher is refused, exit code 5), and both events exist (the launcher made them).
//! What can reach it while it runs: the end of that process, and the «stop» event. Both only make it remove the block.
//! The prompt answered with another Windows account (a standard user typing an administrator's password): the copy may
//! not watch that user's process, so it works as in 0.4.0-0.5.4 (writes the block and exits; the launcher then asks
//! once more when it closes).
//!
//! Known side effects, for the README: Windows Defender watches the hosts file (it flags redirects of Microsoft
//! hosts; ours are Rockstar and Cfx.re names), some antiviruses (Kaspersky, Dr.Web) may ask about any change: the
//! player sees the launcher's name. While the block is in, those hosts go through the launcher for every program
//! on the PC, which is why it goes away with the launcher.

use std::path::{Path, PathBuf};

/// `C:\Windows\System32`, asked from Windows itself: the elevated copy must not believe `%SystemRoot%` (a user can
/// change it for his own processes).
#[cfg(windows)]
fn system_dir() -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buf = [0u16; 512];
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return PathBuf::from("C:\\Windows\\System32");
    }
    PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]))
}

#[cfg(not(windows))]
fn system_dir() -> PathBuf {
    PathBuf::from("C:\\Windows\\System32")
}

/// The file itself.
pub fn path() -> PathBuf {
    system_dir().join("drivers").join("etc").join("hosts")
}

pub fn read() -> String {
    std::fs::read(path()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

/// The launcher's block now in the hosts file (signin.rs `installed`).
pub fn installed() -> Option<(String, Vec<String>)> {
    crate::signin::installed(&read())
}

/// The block the launcher asks for, every line checked. Ok(None): the file is empty, the block is to be removed.
/// Err: the exit code (2 = the file is unreadable, 3 = something in it is not ours).
fn read_block(block_file: &str) -> Result<Option<String>, i32> {
    let Ok(raw) = std::fs::read(block_file) else { return Err(2) };
    if raw.len() > 16 * 1024 {
        return Err(3);
    }
    let block = String::from_utf8_lossy(&raw).into_owned();
    let block = block.trim_matches(|c| c == '\r' || c == '\n' || c == ' ' || c == '\u{feff}');
    if block.is_empty() {
        return Ok(None);
    }
    let lines: Vec<&str> = block.lines().map(|l| l.trim_end_matches('\r')).collect();
    let count = |marker: &str| lines.iter().filter(|l| **l == marker).count();
    let ours = lines.iter().all(|l| crate::signin::valid_block_line(l))
        && lines.first() == Some(&crate::signin::BEGIN)
        && lines.last() == Some(&crate::signin::END)
        && count(crate::signin::BEGIN) == 1
        && count(crate::signin::END) == 1;
    if !ours {
        return Err(3);
    }
    let mut out = String::new();
    for l in lines {
        out.push_str(l);
        out.push_str("\r\n");
    }
    Ok(Some(out))
}

/// Replaces (Some) or removes (None) the launcher's block in `file`. 0 = done, 4 = could not be written.
fn write_block(file: &Path, block: Option<&str>) -> i32 {
    let old = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        // a file that cannot be read now must not be overwritten with nothing but our lines
        Err(_) => return 4,
    };
    let new = crate::signin::with_block(&old, block);
    if new == old {
        flush_dns();
        return 0;
    }
    // some tools leave the file read-only: clear the flag, keep the rest
    if let Ok(meta) = std::fs::metadata(file) {
        let mut perm = meta.permissions();
        if perm.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(file, perm);
        }
    }
    if std::fs::write(file, &new).is_err() {
        return 4;
    }
    flush_dns();
    0
}

/// Takes the block out, trying again for a while: an antivirus may hold the file for a moment.
fn remove_block(file: &Path, tries: u32) -> i32 {
    for i in 0..tries {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        if write_block(file, None) == 0 {
            return 0;
        }
    }
    4
}

/// The elevated copy: `--coldz-hosts <file>`. Exit code 0 = done, 2 = the file is unreadable, 3 = a line in it is
/// not ours, 4 = the hosts file could not be written.
pub fn apply(block_file: &str) -> i32 {
    match read_block(block_file) {
        Ok(block) => write_block(&path(), block.as_deref()),
        Err(code) => code,
    }
}

/// The DNS client picks hosts changes up by itself; the flush makes it immediate. The full path: an elevated program
/// must not look for `ipconfig.exe` next to itself.
fn flush_dns() {
    let mut cmd = std::process::Command::new(system_dir().join("ipconfig.exe"));
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

// ---------------------------------------------------------------- the copy that stays (0.5.7)

/// `--coldz-watch <pid> <created> <name>`: the launcher to watch (its pid and start time) and the name of the two
/// events the launcher made.
#[derive(Debug, Clone, PartialEq)]
pub struct Watch {
    pub pid: u32,
    /// The launcher's start time (FILETIME as one number).
    pub created: u64,
    /// 32 hex digits, random for every request.
    pub name: String,
}

impl Watch {
    pub fn parse(args: &[String]) -> Option<Watch> {
        let [pid, created, name, ..] = args else { return None };
        let hex = name.len() == 32 && name.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
        let w = Watch { pid: pid.parse().ok()?, created: created.parse().ok()?, name: name.clone() };
        (hex && w.pid != 0 && w.pid != std::process::id()).then_some(w)
    }

    fn args(&self) -> String {
        format!("--coldz-watch {} {} {}", self.pid, self.created, self.name)
    }

    fn event(&self, what: &str) -> String {
        format!("Local\\coldz-hosts-{}-{what}", self.name)
    }
}

/// The elevated copy with `--coldz-watch`: see the module doc. Exit codes of `apply`, and 5 = the launcher that asked
/// is not there (nothing was written).
pub fn guard(block_file: &str, watch: &Watch) -> i32 {
    #[cfg(windows)]
    return win::guard(&path(), block_file, watch);
    #[cfg(not(windows))]
    {
        let _ = watch;
        apply(block_file)
    }
}

/// The elevated copy that stays, as the launcher sees it.
#[derive(Debug)]
pub struct Helper {
    #[cfg(windows)]
    process: std::os::windows::io::OwnedHandle,
    #[cfg(windows)]
    stop: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
pub use win::{request, request_guarded};

#[cfg(not(windows))]
pub fn request(_block: Option<&str>, _work: &Path, _owner: isize) -> Result<(), Refused> {
    Err(Refused::Failed)
}

#[cfg(not(windows))]
pub fn request_guarded(_block: &str, _work: &Path, _owner: isize) -> Result<Option<Helper>, Refused> {
    Err(Refused::Failed)
}

#[cfg(not(windows))]
impl Helper {
    pub fn alive(&self) -> bool {
        false
    }
    pub fn stop(&self, _wait: std::time::Duration) -> bool {
        false
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::cell::RefCell;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_CANCELLED, FILETIME, HANDLE, HWND, LPARAM,
        LRESULT, WAIT_OBJECT_0, WPARAM,
    };
    use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenEventW, OpenProcess,
        QueryFullProcessImageNameW, SetEvent, WaitForMultipleObjects, WaitForSingleObject, EVENT_MODIFY_STATE,
        INFINITE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NO_CONSOLE, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW,
        RegisterClassW, MSG, PM_REMOVE, QS_ALLINPUT, SW_HIDE, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW,
    };

    /// How long the launcher waits for the copy to write the block once the prompt is confirmed (an antivirus may look
    /// at the exe first).
    const READY_TIMEOUT: Duration = Duration::from_secs(60);
    /// The one-shot copy (remove the block).
    const DONE_TIMEOUT: Duration = Duration::from_secs(120);
    pub(super) const WINDOW_CLASS: &str = "coldz-hosts-guard";

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn raw(h: &OwnedHandle) -> HANDLE {
        h.as_raw_handle() as HANDLE
    }

    /// Takes over a handle Windows returned. None: null.
    unsafe fn owned(h: HANDLE) -> Option<OwnedHandle> {
        (!h.is_null()).then(|| OwnedHandle::from_raw_handle(h as _))
    }

    // ------------------------------------------------------------ the elevated side

    fn process_created(h: HANDLE) -> Option<u64> {
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut a, mut b, mut c) = (zero, zero, zero, zero);
        let ok = unsafe { GetProcessTimes(h, &mut created, &mut a, &mut b, &mut c) };
        (ok != 0).then(|| ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
    }

    fn process_image(h: HANDLE) -> Option<PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) };
        (ok != 0 && len > 0).then(|| PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize])))
    }

    /// The same file on disk (short names, subst drives and letter case aside).
    fn same_file(a: &Path, b: &Path) -> bool {
        let full = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
        full(a) == full(b)
    }

    enum Asked {
        /// The launcher that asked: its process, to wait for.
        Launcher(OwnedHandle),
        /// The prompt was confirmed by another Windows account: that process cannot be watched from here.
        OtherUser,
        /// No such process, another program, or another start of it.
        Gone,
    }

    fn launcher(watch: &Watch) -> Asked {
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, watch.pid) };
        let Some(h) = (unsafe { owned(h) }) else {
            return if unsafe { GetLastError() } == ERROR_ACCESS_DENIED { Asked::OtherUser } else { Asked::Gone };
        };
        let mine = std::env::current_exe().ok();
        let same = match (process_image(raw(&h)), mine) {
            (Some(theirs), Some(mine)) => same_file(&theirs, &mine),
            _ => false,
        };
        let running = unsafe { WaitForSingleObject(raw(&h), 0) } != WAIT_OBJECT_0;
        if same && running && process_created(raw(&h)) == Some(watch.created) {
            Asked::Launcher(h)
        } else {
            Asked::Gone
        }
    }

    fn open_event(name: &str, access: u32) -> Option<OwnedHandle> {
        let name = wide(std::ffi::OsStr::new(name));
        unsafe { owned(OpenEventW(access, 0, name.as_ptr())) }
    }

    thread_local! {
        /// The file the hidden window of this thread cleans when Windows logs off; true once it has.
        static SESSION: RefCell<Option<(PathBuf, bool)>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn session_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match msg {
            WM_QUERYENDSESSION => 1,
            WM_ENDSESSION => {
                if w != 0 {
                    // Windows may end this process as soon as the message is answered: clean up right here
                    let file = SESSION.with(|s| s.borrow().as_ref().map(|(f, _)| f.clone()));
                    if let Some(f) = file {
                        let _ = remove_block(&f, 8);
                        SESSION.with(|s| {
                            if let Some(slot) = s.borrow_mut().as_mut() {
                                slot.1 = true;
                            }
                        });
                    }
                }
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }

    /// A window nobody sees: only windows hear that Windows logs off or shuts down.
    struct SessionWindow(HWND);

    impl SessionWindow {
        fn create(file: &Path) -> Option<Self> {
            SESSION.with(|s| *s.borrow_mut() = Some((file.to_path_buf(), false)));
            let class = wide(std::ffi::OsStr::new(WINDOW_CLASS));
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let mut wc: WNDCLASSW = std::mem::zeroed();
                wc.lpfnWndProc = Some(session_proc);
                wc.hInstance = instance;
                wc.lpszClassName = class.as_ptr();
                RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                (!hwnd.is_null()).then_some(SessionWindow(hwnd))
            }
        }

        fn ended() -> bool {
            SESSION.with(|s| s.borrow().as_ref().is_some_and(|(_, done)| *done))
        }
    }

    impl Drop for SessionWindow {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.0) };
        }
    }

    /// Until the launcher ends, asks to stop, or Windows logs off.
    fn wait(launcher: &OwnedHandle, stop: &OwnedHandle) {
        let handles = [raw(launcher), raw(stop)];
        loop {
            let r = unsafe { MsgWaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE, QS_ALLINPUT) };
            if r != WAIT_OBJECT_0 + 2 {
                return; // one of the two, or the wait itself failed: the block goes away either way
            }
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    DispatchMessageW(&msg);
                }
            }
            if SessionWindow::ended() {
                return;
            }
        }
    }

    pub(super) fn guard(file: &Path, block_file: &str, watch: &Watch) -> i32 {
        let block = match read_block(block_file) {
            Ok(Some(b)) => b,
            Ok(None) => return write_block(file, None),
            Err(code) => return code,
        };
        let launcher = match launcher(watch) {
            Asked::Launcher(h) => h,
            Asked::OtherUser => return write_block(file, Some(&block)),
            Asked::Gone => return 5,
        };
        let ready = open_event(&watch.event("ready"), EVENT_MODIFY_STATE);
        let stop = open_event(&watch.event("stop"), SYNCHRONIZATION_SYNCHRONIZE);
        let (Some(ready), Some(stop)) = (ready, stop) else { return 5 };
        let window = SessionWindow::create(file);
        let code = write_block(file, Some(&block));
        if code != 0 {
            // half written (the disk is full, an antivirus cut in): what is there goes away again
            let _ = remove_block(file, 4);
            return code;
        }
        unsafe { SetEvent(raw(&ready)) };
        wait(&launcher, &stop);
        let code = remove_block(file, 40);
        drop(window);
        code
    }

    // ------------------------------------------------------------ the launcher's side

    /// This exe as administrator with `params`: one Windows prompt. `owner` — the launcher's window (HWND, 0 = none):
    /// the prompt then comes up in front of it, not blinking on the taskbar. Blocks while the prompt is open.
    fn start_elevated(params: &str, owner: isize) -> Result<OwnedHandle, Refused> {
        let exe = std::env::current_exe().map_err(|_| Refused::Failed)?;
        let verb = wide(std::ffi::OsStr::new("runas"));
        let exe_w = wide(exe.as_os_str());
        let params = wide(std::ffi::OsStr::new(params));
        // not the launcher's own folder: a copy that stays would keep it from being removed
        let dir = wide(system_dir().as_os_str());
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_NO_CONSOLE;
        info.hwnd = owner as _;
        info.lpVerb = verb.as_ptr();
        info.lpFile = exe_w.as_ptr();
        info.lpParameters = params.as_ptr();
        info.lpDirectory = dir.as_ptr();
        info.nShow = SW_HIDE as i32;
        let (ok, err) = unsafe {
            let com = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
            let ok = ShellExecuteExW(&mut info);
            let err = GetLastError();
            if com >= 0 {
                CoUninitialize();
            }
            (ok, err)
        };
        if ok == 0 {
            return Err(if err == ERROR_CANCELLED { Refused::Cancelled } else { Refused::Failed });
        }
        unsafe { owned(info.hProcess) }.ok_or(Refused::Failed)
    }

    /// The block goes to the elevated copy through a file in `work` (a folder the launcher may write to), removed
    /// after use.
    fn with_block_file<T>(block: &str, work: &Path, run: impl FnOnce(&Path) -> Result<T, Refused>) -> Result<T, Refused> {
        let _ = std::fs::create_dir_all(work);
        let file = work.join("hosts-block.txt");
        std::fs::write(&file, block).map_err(|_| Refused::Failed)?;
        let done = run(&file);
        let _ = std::fs::remove_file(&file);
        done
    }

    fn exit_code(process: &OwnedHandle) -> u32 {
        let mut code: u32 = 1;
        unsafe { GetExitCodeProcess(raw(process), &mut code) };
        code
    }

    /// Writes `block` (None: remove the launcher's block) through the elevated copy of this exe, which exits at once.
    /// Blocks the calling thread for as long as the Windows prompt is open: call it from a blocking task.
    pub fn request(block: Option<&str>, work: &Path, owner: isize) -> Result<(), Refused> {
        with_block_file(block.unwrap_or_default(), work, |file| {
            let process = start_elevated(&format!("--coldz-hosts \"{}\"", file.display()), owner)?;
            let waited = unsafe { WaitForSingleObject(raw(&process), DONE_TIMEOUT.as_millis() as u32) };
            if waited == WAIT_OBJECT_0 && exit_code(&process) == 0 {
                Ok(())
            } else {
                Err(Refused::Failed)
            }
        })
    }

    /// Writes `block` through the elevated copy, which stays and removes it when this process ends or asks (see the
    /// module doc). Ok(None): the block is written, but the copy could not stay (the prompt was confirmed by another
    /// Windows account): `request(None, ...)` removes the block then. Blocks like `request`.
    pub fn request_guarded(block: &str, work: &Path, owner: isize) -> Result<Option<Helper>, Refused> {
        let ask = Ask::new()?;
        with_block_file(block, work, |file| {
            let process = start_elevated(&format!("--coldz-hosts \"{}\" {}", file.display(), ask.watch.args()), owner)?;
            ask.wait(process, READY_TIMEOUT)
        })
    }

    /// The launcher's end of one request: who asks, and the two events.
    pub(super) struct Ask {
        pub watch: Watch,
        ready: OwnedHandle,
        stop: OwnedHandle,
    }

    impl Ask {
        pub(super) fn new() -> Result<Ask, Refused> {
            use ring::rand::SecureRandom;
            let mut id = [0u8; 16];
            ring::rand::SystemRandom::new().fill(&mut id).map_err(|_| Refused::Failed)?;
            let name: String = id.iter().map(|b| format!("{b:02x}")).collect();
            let created = process_created(unsafe { GetCurrentProcess() }).ok_or(Refused::Failed)?;
            let watch = Watch { pid: std::process::id(), created, name };
            let event = |what: &str| -> Result<OwnedHandle, Refused> {
                let name = wide(std::ffi::OsStr::new(&watch.event(what)));
                let h = unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) };
                let taken = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
                let h = unsafe { owned(h) }.ok_or(Refused::Failed)?;
                if taken {
                    Err(Refused::Failed)
                } else {
                    Ok(h)
                }
            };
            let (ready, stop) = (event("ready")?, event("stop")?);
            Ok(Ask { watch, ready, stop })
        }

        /// The copy has started: until it says the block is in, or ends.
        pub(super) fn wait(self, process: OwnedHandle, limit: Duration) -> Result<Option<Helper>, Refused> {
            let handles = [raw(&self.ready), raw(&process)];
            let r = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, limit.as_millis() as u32) };
            if r == WAIT_OBJECT_0 {
                return Ok(Some(Helper { process, stop: self.stop }));
            }
            if r == WAIT_OBJECT_0 + 1 {
                return if exit_code(&process) == 0 { Ok(None) } else { Err(Refused::Failed) };
            }
            // too long: should it come to write the block after all, it takes it out again at once
            unsafe { SetEvent(raw(&self.stop)) };
            Err(Refused::Failed)
        }
    }

    impl Helper {
        pub fn alive(&self) -> bool {
            unsafe { WaitForSingleObject(raw(&self.process), 0) != WAIT_OBJECT_0 }
        }

        /// Asks the copy to take the block out and waits for it to end. True: it did, and reported no error.
        pub fn stop(&self, limit: Duration) -> bool {
            unsafe { SetEvent(raw(&self.stop)) };
            let until = Instant::now() + limit;
            let left = until.saturating_duration_since(Instant::now()).as_millis() as u32;
            let waited = unsafe { WaitForSingleObject(raw(&self.process), left) };
            waited == WAIT_OBJECT_0 && exit_code(&self.process) == 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signin::{BEGIN, END};

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("coldz-hosts-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn block() -> String {
        crate::signin::block("127.0.0.1", &["scui.rockstargames.com".to_string(), "rsg.sc".to_string()])
    }

    #[test]
    fn apply_refuses_foreign_lines() {
        let dir = dir("lines");
        let f = dir.join("block.txt");
        let refused = |text: &str| {
            std::fs::write(&f, text).unwrap();
            read_block(f.to_str().unwrap())
        };
        // not between the markers
        assert_eq!(refused("127.0.0.1 evil.example\r\n"), Err(3));
        assert_eq!(refused(&format!("{BEGIN}\r\n8.8.8.8 scui.rockstargames.com\r\n{END}\r\n")), Err(3));
        // a second block, or lines after the end, inside one request
        assert_eq!(refused(&format!("{b}{b}", b = block())), Err(3));
        assert_eq!(refused(&format!("{}127.0.0.1 evil.example\r\n", block())), Err(3));
        assert_eq!(refused(&format!("{BEGIN}\r\n127.0.0.1 scui.rockstargames.com\r\n")), Err(3));
        assert_eq!(refused(&"# ".repeat(20_000)), Err(3));
        assert_eq!(refused(" \r\n"), Ok(None));
        assert_eq!(refused(&block()), Ok(Some(block())));
        // the real entry point refuses before it looks at the hosts file
        std::fs::write(&f, "127.0.0.1 evil.example\r\n").unwrap();
        assert_eq!(apply(f.to_str().unwrap()), 3);
        assert_eq!(apply(dir.join("missing.txt").to_str().unwrap()), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_and_removes_only_its_block() {
        let dir = dir("write");
        let hosts = dir.join("hosts");
        let original = b"# \xcf\xf0\xe8\xe2\xe5\xf2 (cp1251)\r\n127.0.0.1 localhost\r\n0.0.0.0 ads.example".to_vec();
        std::fs::write(&hosts, &original).unwrap();
        assert_eq!(write_block(&hosts, Some(&block())), 0);
        let now = std::fs::read(&hosts).unwrap();
        assert!(now.starts_with(&original) && now.ends_with(format!("{END}\r\n").as_bytes()));
        // again: replaced, not doubled
        assert_eq!(write_block(&hosts, Some(&block())), 0);
        assert_eq!(std::fs::read(&hosts).unwrap(), now);
        assert_eq!(remove_block(&hosts, 3), 0);
        // only the line end before the block stays
        assert_eq!(std::fs::read(&hosts).unwrap(), [&original[..], b"\r\n"].concat());
        // no file at all: made; a file that cannot be read (a folder here): nothing is written
        assert_eq!(write_block(&dir.join("new"), Some(&block())), 0);
        assert_eq!(write_block(&dir, Some(&block())), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watch_arguments() {
        let a = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let name = "0123456789abcdef0123456789abcdef";
        let w = Watch::parse(&a(&["4242424", "133000000000000000", name])).unwrap();
        assert_eq!(w, Watch { pid: 4242424, created: 133000000000000000, name: name.into() });
        assert_eq!(w.args(), format!("--coldz-watch 4242424 133000000000000000 {name}"));
        assert_eq!(w.event("stop"), format!("Local\\coldz-hosts-{name}-stop"));
        for bad in [
            a(&[]),
            a(&["1", "2"]),
            a(&["0", "2", name]),
            a(&["x", "2", name]),
            a(&["1", "-2", name]),
            a(&["1", "2", "..\\..\\evil"]),
            a(&["1", "2", &name.to_uppercase()]),
            a(&["1", "2", &format!("{name}0")]),
            a(&[&std::process::id().to_string(), "2", name]),
        ] {
            assert_eq!(Watch::parse(&bad), None, "{bad:?}");
        }
    }

    // ------------------------------------------------------------ the copy that stays: real processes, a hosts file
    // in the temp folder, no elevation. The «launcher» and the «elevated copy» are this test program started again
    // (so they run the same exe, like the launcher and its copy), with one ignored test each.

    #[cfg(windows)]
    mod guarded {
        use super::*;
        use std::os::windows::io::{AsRawHandle, OwnedHandle};
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};

        const SLOW: Duration = Duration::from_secs(30);

        /// Not a test: «the elevated copy», run by the tests below as a child.
        #[test]
        #[ignore]
        fn child_copy() {
            let Ok(args) = std::env::var("COLDZ_TEST_COPY") else { return };
            let a: Vec<String> = args.split('|').map(str::to_string).collect();
            let Some(w) = Watch::parse(&a[2..]) else { std::process::exit(2) };
            std::process::exit(win::guard(Path::new(&a[0]), &a[1], &w));
        }

        /// Not a test: «a launcher» that does nothing, run as a child.
        #[test]
        #[ignore]
        fn child_launcher() {
            if std::env::var_os("COLDZ_TEST_LAUNCHER").is_some() {
                std::thread::sleep(Duration::from_secs(60));
            }
        }

        fn child(test: &str, env: &str, value: &str) -> Child {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &format!("hosts::tests::guarded::{test}"), "--ignored", "--nocapture"])
                .env(env, value)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }

        struct Case {
            dir: PathBuf,
            hosts: PathBuf,
            block: PathBuf,
            original: Vec<u8>,
        }

        impl Case {
            fn new(name: &str) -> Case {
                let dir = dir(name);
                let (hosts, block_file) = (dir.join("hosts"), dir.join("block.txt"));
                let original = b"# \xcf\xf0\xe8\r\n127.0.0.1 localhost\r\n".to_vec();
                std::fs::write(&hosts, &original).unwrap();
                std::fs::write(&block_file, block()).unwrap();
                Case { dir, hosts, block: block_file, original }
            }
            fn copy(&self, w: &Watch) -> Child {
                let v = format!("{}|{}|{}|{}|{}", self.hosts.display(), self.block.display(), w.pid, w.created, w.name);
                child("child_copy", "COLDZ_TEST_COPY", &v)
            }
            fn now(&self) -> Vec<u8> {
                std::fs::read(&self.hosts).unwrap()
            }
            fn has_block(&self) -> bool {
                crate::signin::installed(&String::from_utf8_lossy(&self.now())).is_some()
            }
        }

        impl Drop for Case {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        fn code(mut c: Child) -> Option<i32> {
            let until = Instant::now() + SLOW;
            while Instant::now() < until {
                if let Some(s) = c.try_wait().unwrap() {
                    return s.code();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = c.kill();
            None
        }

        /// The second handle the launcher gets from ShellExecuteExW.
        fn handle(c: &Child) -> OwnedHandle {
            use std::os::windows::io::AsHandle;
            let _ = c.as_raw_handle();
            c.as_handle().try_clone_to_owned().unwrap()
        }

        #[test]
        fn stop_event_takes_the_block_out() {
            let case = Case::new("stop");
            let ask = win::Ask::new().unwrap();
            let copy = case.copy(&ask.watch);
            let helper = ask.wait(handle(&copy), SLOW).expect("ready").expect("stays");
            assert!(case.has_block());
            assert!(case.now().starts_with(&case.original));
            assert!(helper.alive());
            assert!(helper.stop(SLOW));
            assert!(!helper.alive());
            assert_eq!(case.now(), case.original);
            assert_eq!(code(copy), Some(0));
        }

        #[test]
        fn launcher_gone_takes_the_block_out() {
            let case = Case::new("gone");
            let mut launcher = child("child_launcher", "COLDZ_TEST_LAUNCHER", "1");
            let mut ask = win::Ask::new().unwrap();
            // the events are ours, the process to watch is the child
            ask.watch.pid = launcher.id();
            ask.watch.created = created_of(&launcher);
            let copy = case.copy(&ask.watch);
            let helper = ask.wait(handle(&copy), SLOW).expect("ready").expect("stays");
            assert!(case.has_block());
            // a crash, Task Manager, an update: the process is simply gone
            launcher.kill().unwrap();
            let _ = launcher.wait();
            assert_eq!(code(copy), Some(0));
            assert!(!helper.alive());
            assert_eq!(case.now(), case.original);
        }

        fn created_of(c: &Child) -> u64 {
            use windows_sys::Win32::Foundation::FILETIME;
            use windows_sys::Win32::System::Threading::GetProcessTimes;
            let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
            let (mut a, mut b, mut k, mut u) = (zero, zero, zero, zero);
            assert!(unsafe { GetProcessTimes(c.as_raw_handle() as _, &mut a, &mut b, &mut k, &mut u) } != 0);
            ((a.dwHighDateTime as u64) << 32) | a.dwLowDateTime as u64
        }

        #[test]
        fn refuses_another_process_and_writes_nothing() {
            let case = Case::new("refuse");
            // the right pid, another start time (a reused pid)
            let mut ask = win::Ask::new().unwrap();
            ask.watch.created += 1;
            assert_eq!(code(case.copy(&ask.watch)), Some(5));
            // a process that does not exist any more
            let mut launcher = child("child_launcher", "COLDZ_TEST_LAUNCHER", "1");
            let mut ask = win::Ask::new().unwrap();
            ask.watch.pid = launcher.id();
            ask.watch.created = created_of(&launcher);
            launcher.kill().unwrap();
            let _ = launcher.wait();
            assert_eq!(code(case.copy(&ask.watch)), Some(5));
            // another program
            let mut other = Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(Stdio::null()).spawn().unwrap();
            let mut ask = win::Ask::new().unwrap();
            ask.watch.pid = other.id();
            ask.watch.created = created_of(&other);
            assert_eq!(code(case.copy(&ask.watch)), Some(5));
            let _ = other.kill();
            // events nobody made
            let mut w = win::Ask::new().unwrap().watch;
            w.name = "f".repeat(32);
            assert_eq!(code(case.copy(&w)), Some(5));
            // foreign lines: refused before anything else
            let ask = win::Ask::new().unwrap();
            std::fs::write(&case.block, "127.0.0.1 evil.example\r\n").unwrap();
            assert_eq!(code(case.copy(&ask.watch)), Some(3));
            // what the launcher makes of an exit with an error
            let copy = case.copy(&ask.watch);
            assert!(matches!(ask.wait(handle(&copy), SLOW), Err(Refused::Failed)));
            assert_eq!(case.now(), case.original);
        }

        #[test]
        fn log_off_takes_the_block_out() {
            use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                EnumWindows, GetClassNameW, GetWindowThreadProcessId, SendMessageW, WM_ENDSESSION, WM_QUERYENDSESSION,
            };
            struct Find {
                pid: u32,
                hwnd: isize,
            }
            unsafe extern "system" fn look(hwnd: HWND, l: LPARAM) -> BOOL {
                let f = &mut *(l as *mut Find);
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, &mut pid);
                let mut class = [0u16; 64];
                let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) as usize;
                if pid == f.pid && String::from_utf16_lossy(&class[..n]) == win::WINDOW_CLASS {
                    f.hwnd = hwnd as isize;
                    return 0;
                }
                1
            }
            let case = Case::new("logoff");
            let ask = win::Ask::new().unwrap();
            let copy = case.copy(&ask.watch);
            let helper = ask.wait(handle(&copy), SLOW).expect("ready").expect("stays");
            assert!(case.has_block());
            let mut f = Find { pid: copy.id(), hwnd: 0 };
            unsafe { EnumWindows(Some(look), &mut f as *mut Find as LPARAM) };
            assert_ne!(f.hwnd, 0, "the hidden window");
            unsafe {
                assert_eq!(SendMessageW(f.hwnd as HWND, WM_QUERYENDSESSION, 0, 0), 1);
                // another program refused the log-off: the block stays
                SendMessageW(f.hwnd as HWND, WM_ENDSESSION, 0, 0);
                assert!(case.has_block() && helper.alive());
                SendMessageW(f.hwnd as HWND, WM_ENDSESSION, 1, 0);
            }
            // gone before the message is answered: Windows may end the process right after
            assert_eq!(case.now(), case.original);
            assert_eq!(code(copy), Some(0));
        }

        #[test]
        fn killed_copy_is_noticed() {
            let case = Case::new("killed");
            let ask = win::Ask::new().unwrap();
            let mut copy = case.copy(&ask.watch);
            let helper = ask.wait(handle(&copy), SLOW).expect("ready").expect("stays");
            copy.kill().unwrap();
            let _ = copy.wait();
            // the launcher then asks Windows once more when it closes (lib.rs signin_release)
            assert!(!helper.alive());
            assert!(!helper.stop(Duration::from_secs(1)));
            assert!(case.has_block());
        }
    }
}
