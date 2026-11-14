//! Is FiveM already running? (13.11.2026, a player: FiveM shows "Failed to initialize LinkProtocolIPC, master is
//! already initialized" every time he joins.)
//!
//! How FiveM handles `fivem://connect/...` (citizenfx/fivem, code/components/glue/src/ConnectToNative.cpp and
//! LinkProtocolIPC.cpp): every FiveM process shares state through named shared memory. A FiveM.exe started while
//! FiveM is already running does not start a second game: it passes the link to the running game process over
//! "LinkProtocolIPC" and exits. The game process registers itself as the IPC "master" when it starts; if the shared
//! memory still says a master exists (an old game process is still alive: hung in the background, closing, crashed with
//! its error window open), the new game process stops with "master is already initialized". If FiveM.exe is started
//! again while the first one is still starting (no game process yet), it fails with "no master process".
//!
//! So before "Play" the launcher looks at the FiveM processes:
//!   * none — start FiveM with the link (as before);
//!   * the game process has a visible window — FiveM is open: hand it the link (FiveM itself passes it over IPC);
//!   * the launcher started FiveM a moment ago and the game window is not there yet — it is still starting, wait;
//!   * anything else (game process without a window, only helper processes, an error window) — FiveM is stuck:
//!     do not start it again, offer to close all FiveM processes.

use serde::Serialize;

/// FiveM processes: FiveM.exe, FiveM_b3258_GTAProcess.exe and helpers that FiveM runs from files without ".exe"
/// (`%LOCALAPPDATA%\FiveM\FiveM.app\data\cache\subprocess`: FiveM_ChromeBrowser, FiveM_DumpServer — it shows the error
/// window, FiveM_ROSLauncher, FiveM_ROSService).
pub fn is_fivem(exe: &str) -> bool {
    let e = exe.to_ascii_lowercase();
    e == "fivem.exe" || e.starts_with("fivem_")
}

/// The game itself: FiveM_GTAProcess.exe, FiveM_b3258_GTAProcess.exe (older builds: *_GameProcess.exe).
pub fn is_game(exe: &str) -> bool {
    let e = exe.to_ascii_lowercase();
    is_fivem(exe) && (e.contains("gtaprocess") || e.contains("gameprocess"))
}

/// One running process: exe name, pid, has a visible top-level window.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub exe: String,
    pub pid: u32,
    pub window: bool,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FivemState {
    /// FiveM processes of any kind.
    pub count: u32,
    /// A game process is running.
    pub game: bool,
    /// The game process has a visible window (FiveM menu or the game).
    pub game_window: bool,
}

pub fn state_of(procs: &[Proc]) -> FivemState {
    let mut s = FivemState::default();
    for p in procs.iter().filter(|p| is_fivem(&p.exe)) {
        s.count += 1;
        if is_game(&p.exe) {
            s.game = true;
            s.game_window |= p.window;
        }
    }
    s
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    /// No FiveM: start it with the link.
    Launch,
    /// FiveM is open: hand it the link, it connects by itself.
    PassLink,
    /// The launcher started FiveM a moment ago: do not start it again.
    Starting,
    /// FiveM processes without a game window: starting again breaks it. Offer to close FiveM.
    Stuck,
}

/// `launched_recently` — the launcher started FiveM within the last couple of minutes.
pub fn decide(s: &FivemState, launched_recently: bool) -> Decision {
    if s.count == 0 {
        Decision::Launch
    } else if s.game_window {
        Decision::PassLink
    } else if launched_recently {
        Decision::Starting
    } else {
        Decision::Stuck
    }
}

// ---------------------------------------------------------------- Windows

/// All processes with their visible-window flag. Empty outside Windows or if the snapshot fails.
#[cfg(windows)]
pub fn processes() -> Vec<Proc> {
    use std::collections::HashSet;
    use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HWND, INVALID_HANDLE_VALUE, LPARAM};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible};

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if IsWindowVisible(hwnd) != 0 {
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let set = &mut *(lparam as *mut HashSet<u32>);
            set.insert(pid);
        }
        1
    }

    let mut with_window: HashSet<u32> = HashSet::new();
    let mut out = Vec::new();
    unsafe {
        EnumWindows(Some(collect), &mut with_window as *mut HashSet<u32> as LPARAM);
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
            out.push(Proc { window: with_window.contains(&entry.th32ProcessID), pid: entry.th32ProcessID, exe });
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
    }
    out
}

#[cfg(not(windows))]
pub fn processes() -> Vec<Proc> {
    Vec::new()
}

/// Full path of a process exe (fivem.rs: where a running FiveM lives). None — no rights or already gone.
#[cfg(windows)]
pub fn exe_path(pid: u32) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        (ok && len > 0).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

#[cfg(not(windows))]
pub fn exe_path(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

/// Ends every process whose exe matches `pick`. Returns (closed, failed) — failed: no rights (FiveM run as
/// administrator) or already gone.
#[cfg(windows)]
pub fn close_matching(pick: impl Fn(&str) -> bool) -> (u32, u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    let (mut closed, mut failed) = (0, 0);
    for p in processes().into_iter().filter(|p| pick(&p.exe)) {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, p.pid);
            if h.is_null() {
                failed += 1;
                continue;
            }
            if TerminateProcess(h, 1) != 0 {
                closed += 1;
            } else {
                failed += 1;
            }
            CloseHandle(h);
        }
    }
    (closed, failed)
}

#[cfg(not(windows))]
pub fn close_matching(_pick: impl Fn(&str) -> bool) -> (u32, u32) {
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(exe: &str, window: bool) -> Proc {
        Proc { exe: exe.into(), pid: 1, window }
    }

    #[test]
    fn knows_fivem_and_game_processes() {
        for exe in ["FiveM.exe", "FiveM_b3258_GTAProcess.exe", "FiveM_GTAProcess.exe", "FiveM_ChromeBrowser",
            "FiveM_DumpServer", "FiveM_ROSLauncher", "FiveM_ROSService", "fivem_steamchild.exe"] {
            assert!(is_fivem(exe), "{exe}");
        }
        for exe in ["COLDZ Launcher.exe", "explorer.exe", "GTA5.exe", "FiveM", "notFiveM.exe", "fivemanager.exe"] {
            assert!(!is_fivem(exe), "{exe}");
        }
        assert!(is_game("FiveM_b3258_GTAProcess.exe"));
        assert!(is_game("FiveM_GTAProcess.exe"));
        assert!(is_game("FiveM_b2699_GameProcess.exe"));
        assert!(!is_game("FiveM.exe"));
        assert!(!is_game("FiveM_ChromeBrowser.exe"));
    }

    #[test]
    fn decides_what_play_does() {
        // no FiveM — start it
        assert_eq!(decide(&state_of(&[p("explorer.exe", true)]), false), Decision::Launch);
        // FiveM open (menu or game) — hand it the link
        let open = state_of(&[p("FiveM.exe", false), p("FiveM_b3258_GTAProcess.exe", true), p("FiveM_ChromeBrowser.exe", false)]);
        assert_eq!(open, FivemState { count: 3, game: true, game_window: true });
        assert_eq!(decide(&open, false), Decision::PassLink);
        assert_eq!(decide(&open, true), Decision::PassLink);
        // just started by the launcher, the game window is not up yet (updater, splash) — wait, do not start again
        let starting = state_of(&[p("FiveM.exe", true)]);
        assert_eq!(decide(&starting, true), Decision::Starting);
        // game process hung in the background without a window — the case behind "master is already initialized"
        let hung = state_of(&[p("FiveM_b3258_GTAProcess.exe", false)]);
        assert_eq!(decide(&hung, false), Decision::Stuck);
        // only leftovers or an error window (FiveM_DumpServer), not started by us — stuck as well
        assert_eq!(decide(&state_of(&[p("FiveM.exe", true)]), false), Decision::Stuck);
        assert_eq!(decide(&state_of(&[p("FiveM_DumpServer", true)]), false), Decision::Stuck);
        assert_eq!(decide(&state_of(&[p("FiveM_ChromeBrowser", false)]), false), Decision::Stuck);
        // started a moment ago and the game process is up but has no window yet — still starting
        assert_eq!(decide(&hung, true), Decision::Starting);
    }

    #[cfg(windows)]
    #[test]
    fn sees_and_closes_real_processes() {
        // this test process is visible in the list and is not FiveM
        let me = std::process::id();
        let list = processes();
        let mine = list.iter().find(|p| p.pid == me).expect("own process in the list");
        assert!(!is_fivem(&mine.exe));
        // its full path is the test binary itself (fivem.rs reads paths of running FiveM this way)
        let path = exe_path(me).expect("own exe path");
        assert_eq!(path.file_name(), std::env::current_exe().unwrap().file_name());

        // a throwaway copy of cmd.exe under a unique name: close_matching ends it (never touches a real FiveM)
        let dir = std::env::temp_dir().join(format!("coldz-close-test-{me}"));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("coldz_close_test_dummy.exe");
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        std::fs::copy(std::path::Path::new(&windir).join("System32").join("cmd.exe"), &exe).unwrap();
        // cmd /k with a pipe for input just waits: no child processes left behind
        let mut child = std::process::Command::new(&exe)
            .arg("/k")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (closed, failed) = close_matching(|e| e.eq_ignore_ascii_case("coldz_close_test_dummy.exe"));
        assert_eq!((closed, failed), (1, 0));
        let status = child.wait().unwrap();
        assert!(!status.success());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
