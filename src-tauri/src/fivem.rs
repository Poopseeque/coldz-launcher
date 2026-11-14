//! Where is FiveM? (14.11.2026: a player's launcher 0.3.1 said "FiveM not found" while FiveM was installed — 0.3.1 looked
//! only at two link-handler registry keys and %LOCALAPPDATA%\FiveM.)
//!
//! Order, the first FiveM.exe found wins:
//!   1. the path the player chose by hand ("Choose FiveM.exe manually", settings.json `fivemPath`);
//!   2. the registry: the fivem:// link handler (HKCU, HKLM and HKCR classes), FiveM's own "Last Run Location"
//!      (HKCU\Software\CitizenFX\FiveM), uninstall entries of FiveM (InstallLocation, DisplayIcon, UninstallString);
//!   3. a running FiveM: the full path of its process;
//!   4. usual folders: %LOCALAPPDATA%\FiveM, Program Files (both), C:\ D:\ E:\ F:\FiveM, Desktop and Downloads
//!      (FiveM.exe and FiveM\FiveM.exe), the FiveM.lnk shortcut on the desktop and in the Start menu.
//! Any path counts when it leads to a file named FiveM.exe: the exe itself, a folder with it, or the FiveM.app folder
//! (and anything inside it) with FiveM.exe next to it (5). Every step goes to the trace: the launcher writes it to
//! `fivem-search.log` next to settings.json, so we can see where it looked on a player's PC.
//!
//! The launcher never changes FiveM files: it only reads the registry, folders and shortcuts.

use std::path::{Path, PathBuf};

pub const EXE: &str = "FiveM.exe";
const APP_DIR: &str = "FiveM.app";

/// A path (or a command line, a shortcut) from one source and where it came from (for the log).
#[derive(Debug, Clone, PartialEq)]
pub struct Hint {
    pub from: String,
    pub value: String,
}

impl Hint {
    pub fn new(from: impl Into<String>, value: impl Into<String>) -> Self {
        Hint { from: from.into(), value: value.into() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    /// Which source found it: "manual", "registry", "process", "folder", "shortcut".
    pub stage: &'static str,
    pub from: String,
}

/// Where the hints come from: the real PC (`System`) or a test.
pub trait Sources {
    /// The path the player chose by hand.
    fn manual(&self) -> Option<PathBuf>;
    /// Registry values: link handler commands, "Last Run Location", uninstall entries.
    fn registry(&self) -> Vec<Hint>;
    /// Full paths of running FiveM processes.
    fn processes(&self) -> Vec<Hint>;
    /// Usual folders and files.
    fn folders(&self) -> Vec<Hint>;
    /// FiveM.lnk shortcuts (paths of .lnk files that exist).
    fn shortcuts(&self) -> Vec<Hint>;
}

fn is_named(p: &Path, name: &str) -> bool {
    p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.eq_ignore_ascii_case(name))
}

fn is_fivem_exe(p: &Path) -> bool {
    is_named(p, EXE) && p.is_file()
}

/// Leads this path to FiveM.exe? The exe itself; a folder with FiveM.exe or FiveM\FiveM.exe; the FiveM.app folder or a
/// file inside it (helpers live in FiveM.app\data\cache\subprocess) — FiveM.exe is next to FiveM.app.
pub fn resolve(p: &Path) -> Option<PathBuf> {
    if p.as_os_str().is_empty() {
        return None;
    }
    if is_fivem_exe(p) {
        return Some(p.to_path_buf());
    }
    if p.is_dir() {
        for exe in [p.join(EXE), p.join("FiveM").join(EXE)] {
            if is_fivem_exe(&exe) {
                return Some(exe);
            }
        }
    }
    // up the tree to FiveM.app (not further: an unrelated folder above must not count)
    let mut cur = Some(p);
    for _ in 0..8 {
        let Some(dir) = cur else { break };
        if is_named(dir, APP_DIR) {
            let exe = dir.parent()?.join(EXE);
            return is_fivem_exe(&exe).then_some(exe);
        }
        cur = dir.parent();
    }
    None
}

/// `%LOCALAPPDATA%\FiveM` -> `C:\Users\...\AppData\Local\FiveM`. Unknown variables stay as they are.
pub fn expand_env(s: &str) -> String {
    expand_with(s, |name| std::env::var(name).ok())
}

pub fn expand_with(s: &str, var: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match var(name) {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The program path from a command line or an icon value: `"C:\x\FiveM.exe" "%1"`, `C:\Program Files\FiveM\FiveM.exe %1`
/// (no quotes, spaces in the path), `C:\x\FiveM.exe,0`, `%LOCALAPPDATA%\FiveM`. Variables are expanded.
pub fn path_from_command(cmd: &str) -> Option<PathBuf> {
    let s = expand_env(cmd.trim());
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('"') {
        let inner = rest.split('"').next()?.trim();
        return (!inner.is_empty()).then(|| PathBuf::from(inner));
    }
    // ASCII lowercase keeps byte positions
    if let Some(i) = s.to_ascii_lowercase().find(".exe") {
        return Some(PathBuf::from(&s[..i + 4]));
    }
    let t = s.split(',').next().unwrap_or(s).trim();
    (!t.is_empty()).then(|| PathBuf::from(t))
}

/// Looks for FiveM in the order above. Returns what was found and the trace (one line per place looked at).
pub fn search(src: &dyn Sources) -> (Option<Found>, Vec<String>) {
    let mut trace = Vec::new();
    match src.manual() {
        Some(p) => match resolve(&p) {
            Some(exe) => {
                trace.push(format!("вручную: {} -> найден", p.display()));
                return (Some(Found { path: exe, stage: "manual", from: p.display().to_string() }), trace);
            }
            None => trace.push(format!("вручную: {} -> нет FiveM.exe", p.display())),
        },
        None => trace.push("вручную: не указан".into()),
    }
    let stages: [(&'static str, &dyn Fn() -> Vec<Hint>); 4] = [
        ("registry", &|| src.registry()),
        ("process", &|| src.processes()),
        ("folder", &|| src.folders()),
        ("shortcut", &|| src.shortcuts()),
    ];
    for (stage, hints) in stages {
        let list = hints();
        if list.is_empty() {
            trace.push(format!("{}: ничего", stage_name(stage)));
        }
        for h in list {
            let paths: Vec<PathBuf> = if stage == "shortcut" {
                read_lnk(Path::new(&h.value)).unwrap_or_default()
            } else {
                path_from_command(&h.value).into_iter().collect()
            };
            let hit = paths.iter().find_map(|p| resolve(p));
            match &hit {
                Some(exe) => trace.push(format!("{} {}: {} -> найден {}", stage_name(stage), h.from, h.value, exe.display())),
                None if paths.is_empty() => trace.push(format!("{} {}: {} -> не прочитать", stage_name(stage), h.from, h.value)),
                None => trace.push(format!("{} {}: {} -> нет FiveM.exe", stage_name(stage), h.from, h.value)),
            }
            if let Some(exe) = hit {
                return (Some(Found { path: exe, stage, from: h.from }), trace);
            }
        }
    }
    trace.push("FiveM.exe не найден".into());
    (None, trace)
}

fn stage_name(stage: &str) -> &'static str {
    match stage {
        "registry" => "реестр",
        "process" => "процесс",
        "folder" => "папка",
        _ => "ярлык",
    }
}

// ---------------------------------------------------------------- shortcuts (.lnk)

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// A zero-terminated 8-bit string (the system ANSI code page; ASCII outside Windows).
fn ansi_at(b: &[u8], i: usize) -> Option<String> {
    let rest = b.get(i..)?;
    let end = rest.iter().position(|&c| c == 0)?;
    Some(ansi(&rest[..end]))
}

/// A zero-terminated UTF-16 string.
fn wide_at(b: &[u8], i: usize) -> Option<String> {
    let rest = b.get(i..)?;
    let mut units = Vec::new();
    for pair in rest.chunks_exact(2) {
        let u = u16::from_le_bytes([pair[0], pair[1]]);
        if u == 0 {
            return Some(String::from_utf16_lossy(&units));
        }
        units.push(u);
    }
    None
}

#[cfg(windows)]
fn ansi(bytes: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_ACP};
    if bytes.is_empty() || bytes.is_ascii() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    unsafe {
        let n = MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), bytes.len() as i32, std::ptr::null_mut(), 0);
        if n <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut out = vec![0u16; n as usize];
        MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), bytes.len() as i32, out.as_mut_ptr(), n);
        String::from_utf16_lossy(&out)
    }
}

#[cfg(not(windows))]
fn ansi(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Paths a Windows shortcut points to, best first (MS-SHLLINK): LinkInfo LocalBasePath (Unicode, then ANSI) with its
/// suffix, the environment-variable target (`%LOCALAPPDATA%\FiveM\FiveM.exe`), the working folder, the relative path
/// next to the shortcut. None — not a shortcut.
pub fn read_lnk(lnk: &Path) -> Option<Vec<PathBuf>> {
    let b = std::fs::read(lnk).ok()?;
    parse_lnk(&b, lnk.parent())
}

pub fn parse_lnk(b: &[u8], dir: Option<&Path>) -> Option<Vec<PathBuf>> {
    const HAS_ID_LIST: u32 = 0x1;
    const HAS_LINK_INFO: u32 = 0x2;
    const HAS_NAME: u32 = 0x4;
    const HAS_RELATIVE_PATH: u32 = 0x8;
    const HAS_WORKING_DIR: u32 = 0x10;
    const HAS_ARGUMENTS: u32 = 0x20;
    const HAS_ICON_LOCATION: u32 = 0x40;
    const IS_UNICODE: u32 = 0x80;
    if u32_at(b, 0)? != 0x4C {
        return None;
    }
    let flags = u32_at(b, 0x14)?;
    let mut at = 0x4Cusize;
    if flags & HAS_ID_LIST != 0 {
        at += 2 + u16_at(b, at)? as usize;
    }
    let mut out: Vec<PathBuf> = Vec::new();
    if flags & HAS_LINK_INFO != 0 {
        let info = at;
        let size = u32_at(b, info)? as usize;
        let header = u32_at(b, info + 4)? as usize;
        let info_flags = u32_at(b, info + 8)?;
        if info_flags & 0x1 != 0 {
            let base_off = u32_at(b, info + 16)? as usize;
            let suffix_off = u32_at(b, info + 24)? as usize;
            let (base_u, suffix_u) = if header >= 0x24 {
                (u32_at(b, info + 28).map(|o| o as usize), u32_at(b, info + 32).map(|o| o as usize))
            } else {
                (None, None)
            };
            let suffix = match suffix_u.filter(|&o| o > 0) {
                Some(o) => wide_at(b, info + o).unwrap_or_default(),
                None => ansi_at(b, info + suffix_off).unwrap_or_default(),
            };
            if let Some(base) = base_u.filter(|&o| o > 0).and_then(|o| wide_at(b, info + o)) {
                out.push(PathBuf::from(format!("{base}{suffix}")));
            }
            if let Some(base) = ansi_at(b, info + base_off) {
                out.push(PathBuf::from(format!("{base}{suffix}")));
            }
        }
        at = info + size;
    }
    // StringData: NAME, RELATIVE_PATH, WORKING_DIR, ARGUMENTS, ICON_LOCATION (count in characters)
    let unicode = flags & IS_UNICODE != 0;
    let mut strings: Vec<(u32, String)> = Vec::new();
    for bit in [HAS_NAME, HAS_RELATIVE_PATH, HAS_WORKING_DIR, HAS_ARGUMENTS, HAS_ICON_LOCATION] {
        if flags & bit == 0 {
            continue;
        }
        let count = u16_at(b, at)? as usize;
        at += 2;
        let s = if unicode {
            let raw = b.get(at..at + count * 2)?;
            let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            at += count * 2;
            String::from_utf16_lossy(&units)
        } else {
            let raw = b.get(at..at + count)?;
            at += count;
            ansi(raw)
        };
        strings.push((bit, s));
    }
    // ExtraData: EnvironmentVariableDataBlock (0xA0000001): TargetAnsi[260], TargetUnicode[520 bytes]
    while let Some(size) = u32_at(b, at) {
        let size = size as usize;
        if size < 8 {
            break;
        }
        if u32_at(b, at + 4) == Some(0xA000_0001) {
            let target = wide_at(b, at + 8 + 260).filter(|s| !s.is_empty()).or_else(|| ansi_at(b, at + 8));
            if let Some(t) = target.filter(|s| !s.is_empty()) {
                out.push(PathBuf::from(expand_env(&t)));
            }
        }
        at += size;
    }
    for (bit, s) in &strings {
        if s.is_empty() {
            continue;
        }
        if *bit == HAS_WORKING_DIR {
            out.push(PathBuf::from(expand_env(s)));
        } else if *bit == HAS_RELATIVE_PATH {
            if let Some(d) = dir {
                out.push(d.join(s));
            }
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- the real PC

/// Hints from this PC. `manual` — the path from settings.json.
pub struct System {
    pub manual: Option<PathBuf>,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn push_unique(list: &mut Vec<PathBuf>, p: Option<PathBuf>) {
    if let Some(p) = p {
        let key = p.to_string_lossy().to_lowercase();
        if !list.iter().any(|q| q.to_string_lossy().to_lowercase() == key) {
            list.push(p);
        }
    }
}

#[cfg(windows)]
fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
    unsafe {
        let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw);
        if hr < 0 || raw.is_null() {
            if !raw.is_null() {
                CoTaskMemFree(raw as *const core::ffi::c_void);
            }
            return None;
        }
        let mut len = 0usize;
        while *raw.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
        CoTaskMemFree(raw as *const core::ffi::c_void);
        (!s.is_empty()).then(|| PathBuf::from(s))
    }
}

/// Desktops (own, OneDrive, public), Downloads, Start menus (own and all users).
struct Places {
    desktops: Vec<PathBuf>,
    downloads: Vec<PathBuf>,
    start_menus: Vec<PathBuf>,
}

fn places() -> Places {
    let mut desktops = Vec::new();
    let mut downloads = Vec::new();
    let mut start_menus = Vec::new();
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::{
            FOLDERID_CommonStartMenu, FOLDERID_Desktop, FOLDERID_Downloads, FOLDERID_PublicDesktop, FOLDERID_StartMenu,
        };
        push_unique(&mut desktops, known_folder(&FOLDERID_Desktop));
        push_unique(&mut desktops, known_folder(&FOLDERID_PublicDesktop));
        push_unique(&mut downloads, known_folder(&FOLDERID_Downloads));
        push_unique(&mut start_menus, known_folder(&FOLDERID_StartMenu));
        push_unique(&mut start_menus, known_folder(&FOLDERID_CommonStartMenu));
    }
    let home = env_path("USERPROFILE");
    push_unique(&mut desktops, home.as_ref().map(|h| h.join("Desktop")));
    push_unique(&mut desktops, home.as_ref().map(|h| h.join("OneDrive").join("Desktop")));
    push_unique(&mut desktops, env_path("PUBLIC").map(|p| p.join("Desktop")));
    push_unique(&mut downloads, home.as_ref().map(|h| h.join("Downloads")));
    push_unique(&mut start_menus, env_path("APPDATA").map(|p| p.join(r"Microsoft\Windows\Start Menu")));
    push_unique(&mut start_menus, env_path("ProgramData").map(|p| p.join(r"Microsoft\Windows\Start Menu")));
    Places { desktops, downloads, start_menus }
}

#[cfg(windows)]
fn registry_hints() -> Vec<Hint> {
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::{RegKey, HKEY};
    let mut out = Vec::new();
    let roots: [(HKEY, &str, &str); 5] = [
        (HKEY_CURRENT_USER, "HKCU", r"Software\Classes\"),
        (HKEY_LOCAL_MACHINE, "HKLM", r"Software\Classes\"),
        (HKEY_CLASSES_ROOT, "HKCR", ""),
        (HKEY_CURRENT_USER, "HKCU", ""),
        (HKEY_LOCAL_MACHINE, "HKLM", ""),
    ];
    // 1) the fivem:// link handler
    for (root, name, prefix) in roots.iter().take(3) {
        for class in ["FiveM.ProtocolHandler", "fivem"] {
            let path = format!(r"{prefix}{class}\shell\open\command");
            if let Ok(cmd) = RegKey::predef(*root).open_subkey(&path).and_then(|k| k.get_value::<String, _>("")) {
                out.push(Hint::new(format!(r"{name}\{path}"), cmd));
            }
        }
    }
    // 2) where FiveM ran last time (FiveM writes it on every start: the FiveM.app folder)
    for (root, name, _) in roots.iter().skip(3) {
        let path = r"Software\CitizenFX\FiveM";
        if let Ok(k) = RegKey::predef(*root).open_subkey(path) {
            for value in ["Last Run Location", "InstallLocation", "Path"] {
                if let Ok(v) = k.get_value::<String, _>(value) {
                    out.push(Hint::new(format!(r"{name}\{path} «{value}»"), v));
                }
            }
        }
    }
    // 3) uninstall entries of FiveM (own and all users, 64- and 32-bit views)
    let uninstall = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
    let views: [(HKEY, &str, u32); 3] = [
        (HKEY_CURRENT_USER, "HKCU", KEY_READ),
        (HKEY_LOCAL_MACHINE, "HKLM", KEY_READ | KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, "HKLM32", KEY_READ | KEY_WOW64_32KEY),
    ];
    for (root, name, flags) in views {
        let Ok(list) = RegKey::predef(root).open_subkey_with_flags(uninstall, flags) else { continue };
        for sub in list.enum_keys().flatten() {
            let Ok(k) = list.open_subkey_with_flags(&sub, flags) else { continue };
            let display: String = k.get_value("DisplayName").unwrap_or_default();
            let low = format!("{sub} {display}").to_lowercase();
            if !(low.contains("fivem") || low.contains("citizenfx")) {
                continue;
            }
            for value in ["InstallLocation", "DisplayIcon", "UninstallString"] {
                if let Ok(v) = k.get_value::<String, _>(value) {
                    if !v.trim().is_empty() {
                        out.push(Hint::new(format!(r"{name}\…\Uninstall\{sub} «{value}»"), v));
                    }
                }
            }
        }
    }
    out
}

#[cfg(not(windows))]
fn registry_hints() -> Vec<Hint> {
    Vec::new()
}

impl Sources for System {
    fn manual(&self) -> Option<PathBuf> {
        self.manual.clone().filter(|p| !p.as_os_str().is_empty())
    }

    fn registry(&self) -> Vec<Hint> {
        registry_hints()
    }

    fn processes(&self) -> Vec<Hint> {
        crate::running::processes()
            .into_iter()
            .filter(|p| crate::running::is_fivem(&p.exe))
            .filter_map(|p| crate::running::exe_path(p.pid).map(|path| Hint::new(p.exe, path.display().to_string())))
            .collect()
    }

    fn folders(&self) -> Vec<Hint> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        push_unique(&mut dirs, env_path("LOCALAPPDATA").map(|p| p.join("FiveM")));
        push_unique(&mut dirs, env_path("ProgramFiles").map(|p| p.join("FiveM")));
        push_unique(&mut dirs, env_path("ProgramW6432").map(|p| p.join("FiveM")));
        push_unique(&mut dirs, env_path("ProgramFiles(x86)").map(|p| p.join("FiveM")));
        for drive in ['C', 'D', 'E', 'F'] {
            push_unique(&mut dirs, Some(PathBuf::from(format!(r"{drive}:\FiveM"))));
        }
        let pl = places();
        for d in pl.desktops.iter().chain(pl.downloads.iter()) {
            push_unique(&mut dirs, Some(d.join(EXE)));
            push_unique(&mut dirs, Some(d.join("FiveM")));
        }
        dirs.into_iter().map(|p| Hint::new("", p.display().to_string())).collect()
    }

    fn shortcuts(&self) -> Vec<Hint> {
        let pl = places();
        let mut lnks: Vec<PathBuf> = Vec::new();
        for d in &pl.desktops {
            push_unique(&mut lnks, Some(d.join("FiveM.lnk")));
        }
        for m in &pl.start_menus {
            for rel in [r"Programs\FiveM.lnk", r"Programs\FiveM\FiveM.lnk", "FiveM.lnk"] {
                push_unique(&mut lnks, Some(m.join(rel)));
            }
        }
        lnks.into_iter().filter(|p| p.is_file()).map(|p| Hint::new("", p.display().to_string())).collect()
    }
}

// ---------------------------------------------------------------- choosing FiveM.exe by hand

/// The Windows "Open" dialog for FiveM.exe. None — closed without a choice. `owner` — the launcher window (HWND).
#[cfg(windows)]
pub fn pick_file(title: &str, owner: isize) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_DONTADDTORECENT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let filter: Vec<u16> = "FiveM.exe\0FiveM.exe\0*.exe\0*.exe\0\0".encode_utf16().collect();
    let title = wide(title);
    let start = wide(&env_path("LOCALAPPDATA").map(|p| p.display().to_string()).unwrap_or_default());
    let mut file = vec![0u16; 32768];
    unsafe {
        let com = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = owner as _;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.nFilterIndex = 1;
        ofn.lpstrFile = file.as_mut_ptr();
        ofn.nMaxFile = file.len() as u32;
        ofn.lpstrInitialDir = start.as_ptr();
        ofn.lpstrTitle = title.as_ptr();
        ofn.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY | OFN_NOCHANGEDIR | OFN_DONTADDTORECENT;
        let ok = GetOpenFileNameW(&mut ofn) != 0;
        if com >= 0 {
            CoUninitialize();
        }
        if !ok {
            return None;
        }
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&file[..len])))
}

#[cfg(not(windows))]
pub fn pick_file(_title: &str, _owner: isize) -> Option<PathBuf> {
    None
}

// ---------------------------------------------------------------- log

/// Date and time (UTC) for the log, without extra crates: "2026-11-14 11:05:09 UTC".
pub fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil from days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Appends the trace to the log; the file is cut to its newer half once it grows over 256 KB.
pub fn write_log(path: &Path, title: &str, trace: &[String]) {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(path).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        if let Ok(old) = std::fs::read_to_string(path) {
            let cut = old.len() / 2;
            let start = old[cut..].find('\n').map(|i| cut + i + 1).unwrap_or(cut);
            let _ = std::fs::write(path, &old[start..]);
        }
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {}", utc_stamp(secs), title);
        for line in trace {
            let _ = writeln!(f, "    {line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("coldz-fivem-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"MZ").unwrap();
    }

    #[test]
    fn reads_paths_from_commands_and_icons() {
        let p = |s: &str| path_from_command(s).map(|p| p.display().to_string());
        assert_eq!(p(r#""C:\Users\a\AppData\Local\FiveM\FiveM.exe" "%1""#).as_deref(), Some(r"C:\Users\a\AppData\Local\FiveM\FiveM.exe"));
        assert_eq!(p(r"C:\Program Files\FiveM\FiveM.exe %1").as_deref(), Some(r"C:\Program Files\FiveM\FiveM.exe"));
        assert_eq!(p(r"C:\Games\FiveM\FiveM.EXE,0").as_deref(), Some(r"C:\Games\FiveM\FiveM.EXE"));
        assert_eq!(p(r#""D:\FiveM\FiveM.exe" -uninstall app"#).as_deref(), Some(r"D:\FiveM\FiveM.exe"));
        assert_eq!(p(r"D:\FiveM\FiveM.app\").as_deref(), Some(r"D:\FiveM\FiveM.app\"));
        assert_eq!(p("   "), None);
        assert_eq!(p(r#""""#), None);
    }

    #[test]
    fn expands_environment_variables() {
        let var = |n: &str| match n {
            "LOCALAPPDATA" => Some(r"C:\Users\Вася\AppData\Local".to_string()),
            _ => None,
        };
        assert_eq!(expand_with(r"%LOCALAPPDATA%\FiveM\FiveM.exe", var), r"C:\Users\Вася\AppData\Local\FiveM\FiveM.exe");
        assert_eq!(expand_with(r"%NOPE%\x", var), r"%NOPE%\x");
        assert_eq!(expand_with("100% sure", var), "100% sure");
        assert_eq!(expand_with("%%", var), "%%");
    }

    #[test]
    fn resolves_exe_folders_and_fivem_app() {
        let d = tmp("resolve");
        let exe = d.join("Games").join("FiveM").join("FiveM.exe");
        touch(&exe);
        let app = d.join("Games").join("FiveM").join("FiveM.app");
        let helper = app.join("data").join("cache").join("subprocess").join("FiveM_b3258_GTAProcess.exe");
        touch(&helper);
        assert_eq!(resolve(&exe), Some(exe.clone()));
        // folder with FiveM.exe, folder with FiveM\FiveM.exe
        assert_eq!(resolve(&d.join("Games").join("FiveM")), Some(exe.clone()));
        assert_eq!(resolve(&d.join("Games")), Some(exe.clone()));
        // FiveM.app and anything inside it: FiveM.exe next to FiveM.app (5)
        assert_eq!(resolve(&app), Some(exe.clone()));
        assert_eq!(resolve(&helper), Some(exe.clone()));
        // not FiveM: another exe, a missing file, an empty path, an unrelated folder above
        let other = d.join("Other").join("game.exe");
        touch(&other);
        assert_eq!(resolve(&other), None);
        assert_eq!(resolve(&d.join("nope").join("FiveM.exe")), None);
        assert_eq!(resolve(Path::new("")), None);
        assert_eq!(resolve(&d), None);
        // FiveM.app without FiveM.exe next to it
        let lonely = d.join("Lonely").join("FiveM.app").join("data");
        std::fs::create_dir_all(&lonely).unwrap();
        assert_eq!(resolve(&lonely), None);
        // name case does not matter
        let low = d.join("low").join("fivem.exe");
        touch(&low);
        assert_eq!(resolve(&low), Some(low.clone()));
        let _ = std::fs::remove_dir_all(&d);
    }

    struct Fake {
        manual: Option<PathBuf>,
        registry: Vec<Hint>,
        processes: Vec<Hint>,
        folders: Vec<Hint>,
        shortcuts: Vec<Hint>,
    }

    impl Sources for Fake {
        fn manual(&self) -> Option<PathBuf> {
            self.manual.clone()
        }
        fn registry(&self) -> Vec<Hint> {
            self.registry.clone()
        }
        fn processes(&self) -> Vec<Hint> {
            self.processes.clone()
        }
        fn folders(&self) -> Vec<Hint> {
            self.folders.clone()
        }
        fn shortcuts(&self) -> Vec<Hint> {
            self.shortcuts.clone()
        }
    }

    fn empty() -> Fake {
        Fake { manual: None, registry: vec![], processes: vec![], folders: vec![], shortcuts: vec![] }
    }

    #[test]
    fn searches_in_order_and_traces_every_place() {
        let d = tmp("order");
        let a = d.join("A").join("FiveM.exe");
        let b = d.join("B").join("FiveM.exe");
        touch(&a);
        touch(&b);
        // nothing anywhere: every stage is in the trace
        let (found, trace) = search(&empty());
        assert!(found.is_none());
        assert_eq!(trace.len(), 6, "{trace:?}");
        assert_eq!(trace.last().unwrap(), "FiveM.exe не найден");

        // the registry points to a deleted FiveM (moved), the uninstall entry to the new place
        let mut f = empty();
        f.registry = vec![
            Hint::new(r"HKCU\Software\Classes\fivem\shell\open\command", format!("\"{}\" \"%1\"", d.join("Old").join("FiveM.exe").display())),
            Hint::new(r"HKCU\…\Uninstall\CitizenFX_FiveM «InstallLocation»", d.join("B").display().to_string()),
        ];
        f.folders = vec![Hint::new("", a.display().to_string())];
        let (found, trace) = search(&f);
        let found = found.unwrap();
        assert_eq!(found.path, b);
        assert_eq!(found.stage, "registry");
        assert!(trace.iter().any(|l| l.contains("Old") && l.ends_with("нет FiveM.exe")), "{trace:?}");

        // the path chosen by hand wins over everything
        f.manual = Some(a.clone());
        let (found, _) = search(&f);
        assert_eq!(found.unwrap(), Found { path: a.clone(), stage: "manual", from: a.display().to_string() });

        // the chosen file is gone: the search goes on
        f.manual = Some(d.join("Gone").join("FiveM.exe"));
        let (found, trace) = search(&f);
        assert_eq!(found.unwrap().path, b);
        assert!(trace[0].ends_with("нет FiveM.exe"));

        // a running FiveM: the path of its game process inside FiveM.app
        let app_exe = d.join("C").join("FiveM.exe");
        touch(&app_exe);
        let game = d.join("C").join("FiveM.app").join("data").join("cache").join("subprocess").join("FiveM_b3258_GTAProcess.exe");
        touch(&game);
        let mut g = empty();
        g.processes = vec![Hint::new("FiveM_b3258_GTAProcess.exe", game.display().to_string())];
        let (found, _) = search(&g);
        assert_eq!(found.unwrap(), Found { path: app_exe, stage: "process", from: "FiveM_b3258_GTAProcess.exe".into() });
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A shortcut as Windows writes it: header, LinkInfo with a local path (ANSI, optionally Unicode), working folder.
    fn lnk(local_ansi: &str, local_unicode: Option<&str>, working: Option<&str>) -> Vec<u8> {
        let mut b = vec![0u8; 0x4C];
        b[0..4].copy_from_slice(&0x4Cu32.to_le_bytes());
        let mut flags = 0x2u32 | 0x80;
        if working.is_some() {
            flags |= 0x10;
        }
        b[0x14..0x18].copy_from_slice(&flags.to_le_bytes());
        // LinkInfo
        let header: u32 = if local_unicode.is_some() { 0x24 } else { 0x1C };
        let mut tail: Vec<u8> = Vec::new();
        let base_off = header as usize;
        tail.extend(local_ansi.as_bytes());
        tail.push(0);
        let suffix_off = base_off + tail.len();
        tail.push(0);
        let (mut base_u_off, mut suffix_u_off) = (0usize, 0usize);
        if let Some(u) = local_unicode {
            base_u_off = header as usize + tail.len();
            for c in u.encode_utf16() {
                tail.extend(c.to_le_bytes());
            }
            tail.extend([0, 0]);
            suffix_u_off = header as usize + tail.len();
            tail.extend([0, 0]);
        }
        let size = header as usize + tail.len();
        let mut info = Vec::new();
        info.extend((size as u32).to_le_bytes());
        info.extend(header.to_le_bytes());
        info.extend(1u32.to_le_bytes()); // VolumeIDAndLocalBasePath
        info.extend(0u32.to_le_bytes()); // VolumeIDOffset (not read)
        info.extend((base_off as u32).to_le_bytes());
        info.extend(0u32.to_le_bytes()); // CommonNetworkRelativeLinkOffset
        info.extend((suffix_off as u32).to_le_bytes());
        if local_unicode.is_some() {
            info.extend((base_u_off as u32).to_le_bytes());
            info.extend((suffix_u_off as u32).to_le_bytes());
        }
        info.extend(tail);
        b.extend(info);
        if let Some(w) = working {
            let units: Vec<u16> = w.encode_utf16().collect();
            b.extend((units.len() as u16).to_le_bytes());
            for c in units {
                b.extend(c.to_le_bytes());
            }
        }
        b.extend(0u32.to_le_bytes()); // TerminalBlock
        b
    }

    #[test]
    fn reads_windows_shortcuts() {
        let d = tmp("lnk");
        let exe = d.join("FiveM").join("FiveM.exe");
        touch(&exe);
        let ascii_dir = exe.parent().unwrap().display().to_string();
        // local path (ANSI) in LinkInfo
        let paths = parse_lnk(&lnk(&exe.display().to_string(), None, None), None).unwrap();
        assert_eq!(paths.iter().find_map(|p| resolve(p)), Some(exe.clone()));
        // Unicode path first; a Cyrillic folder only in Unicode (ANSI would be broken) — the working folder still leads
        let cyr = d.join("Вася").join("FiveM").join("FiveM.exe");
        touch(&cyr);
        let paths = parse_lnk(&lnk("C:\\broken?\\FiveM.exe", Some(&cyr.display().to_string()), None), None).unwrap();
        assert_eq!(paths[0], cyr);
        let paths = parse_lnk(&lnk("C:\\broken?\\FiveM.exe", None, Some(&cyr.parent().unwrap().display().to_string())), None).unwrap();
        assert_eq!(paths.iter().find_map(|p| resolve(p)), Some(cyr.clone()));
        // a shortcut file on disk goes through the search as the last stage
        let file = d.join("FiveM.lnk");
        std::fs::write(&file, lnk(&exe.display().to_string(), None, Some(&ascii_dir))).unwrap();
        let mut f = empty();
        f.shortcuts = vec![Hint::new("", file.display().to_string())];
        let (found, _) = search(&f);
        assert_eq!(found.unwrap().stage, "shortcut");
        // not a shortcut
        assert!(parse_lnk(b"MZ\x90\x00", None).is_none());
        assert!(parse_lnk(&[], None).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn writes_and_cuts_the_log() {
        let d = tmp("log");
        let log = d.join("fivem-search.log");
        write_log(&log, "поиск FiveM", &["реестр: ничего".to_string()]);
        let s = std::fs::read_to_string(&log).unwrap();
        assert!(s.contains("UTC] поиск FiveM\n    реестр: ничего\n"), "{s}");
        std::fs::write(&log, "x\n".repeat(200_000)).unwrap();
        write_log(&log, "ещё раз", &[]);
        assert!(std::fs::metadata(&log).unwrap().len() < 256 * 1024);
        assert!(std::fs::read_to_string(&log).unwrap().ends_with("ещё раз\n"));
        assert_eq!(utc_stamp(1_794_625_200), "2026-11-14 03:00:00 UTC");
        assert_eq!(utc_stamp(0), "1970-01-01 00:00:00 UTC");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn looks_at_the_real_pc_without_errors() {
        // whatever is installed here: the search finishes and says where it looked
        let sys = System { manual: None };
        let (found, trace) = search(&sys);
        // cargo test real_pc -- --nocapture: what this PC gives, and what each source would give alone
        eprintln!("{found:?}\n{}", trace.join("\n"));
        assert!(!trace.is_empty());
        if let Some(f) = found {
            assert!(is_fivem_exe(&f.path), "{f:?}");
        }
        for (stage, hints) in [("registry", sys.registry()), ("process", sys.processes()), ("folder", sys.folders()),
            ("shortcut", sys.shortcuts())] {
            for h in hints {
                let paths = if stage == "shortcut" {
                    read_lnk(Path::new(&h.value)).unwrap_or_default()
                } else {
                    path_from_command(&h.value).into_iter().collect()
                };
                if let Some(exe) = paths.iter().find_map(|p| resolve(p)) {
                    eprintln!("{stage} {} {} -> {}", h.from, h.value, exe.display());
                    assert!(is_fivem_exe(&exe));
                }
            }
        }
        assert!(!places().desktops.is_empty());
    }
}
