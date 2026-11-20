//! 0.5.0: the support report («Собрать отчёт для поддержки») and the one-line crash hint at launcher start.
//!
//! Both only READ files on the player's PC: FiveM's own logs and crash dumps (`FiveM.app\logs`, `FiveM.app\crashes`),
//! FiveM's `CitizenFX.ini` (`IVPath`: the GTA folder FiveM uses), the Rockstar Games Launcher log
//! (`Documents\Rockstar Games\Launcher\launcher.log`: the GTA folder that launcher knows), the registry entries
//! Rockstar, Steam and Epic copies of GTA 5 leave, the free space and the kind of disk. Nothing is sent anywhere:
//! the report is a zip on the desktop, the player sends it to support himself (Discord).
//!
//! Privacy (Alexander 18.11): before the zip is made the window lists every file in it; the Windows user name and
//! the PC name in the texts become `<user>` / `<pc>`, IPv4 addresses (except our server and relays) `<ip>`, e-mail
//! addresses `<email>`, the Rockstar account number `<id>`. The crash dump itself is a memory image that cannot be
//! cleaned: it goes in only when the player ticks it.
//!
//! The crash hint (Alexander's list, research doc 3, item 3): after a fresh FiveM crash dump (or a damaged game file
//! in the newest FiveM log) the launcher says ONE simple line, the most likely cause first: a damaged game file
//! («Invalid fixup … .ydr»), little free space, two copies of GTA 5 (FiveM starts another one than the Rockstar
//! launcher updates), GTA or FiveM inside OneDrive (files «on demand» are not on the disk), GTA on a slow disk (HDD or
//! a USB stick). Little free space is said also without a crash.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A crash dump this fresh counts for the hint (3 days: players often come back the next evening).
pub const HINT_FOR: Duration = Duration::from_secs(3 * 24 * 3600);
/// Crash dumps listed in the report: the last day.
pub const REPORT_DUMPS_FOR: Duration = Duration::from_secs(24 * 3600);
/// Below this much free space on the FiveM disk the hint says so (FiveM keeps its cache there: several GB).
pub const LOW_DISK_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// A FiveM log is read up to this much from its end (they reach tens of MB on long sessions).
const LOG_TAIL: u64 = 4 * 1024 * 1024;
/// Model and texture files a damaged game shows up with in «Invalid fixup» lines.
const GAME_FILES: [&str; 6] = [".ydr", ".yft", ".ytd", ".ydd", ".ybn", ".ymap"];

// ---------------------------------------------------------------- files of FiveM

/// `FiveM.app` next to FiveM.exe.
pub fn app_dir(fivem_exe: &Path) -> PathBuf {
    fivem_exe.parent().map(|d| d.join("FiveM.app")).unwrap_or_default()
}

/// `key=value` from an ini text (any section), the first one.
pub fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim().eq_ignore_ascii_case(key)).then(|| v.trim().to_string()).filter(|v| !v.is_empty())
    })
}

/// The GTA folder FiveM uses (`CitizenFX.ini` → `IVPath`).
pub fn fivem_gta_dir(app: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(app.join("CitizenFX.ini")).ok()?;
    ini_value(&text, "IVPath").map(PathBuf::from)
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    #[serde(skip)]
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    /// Unix seconds of the last change.
    pub at: u64,
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn info(path: PathBuf) -> Option<FileInfo> {
    let m = std::fs::metadata(&path).ok()?;
    if !m.is_file() {
        return None;
    }
    Some(FileInfo {
        name: path.file_name()?.to_string_lossy().into_owned(),
        size: m.len(),
        at: m.modified().map(unix).unwrap_or(0),
        path,
    })
}

/// Files in `dir` whose name starts with `prefix` and ends with `suffix` (case-insensitive), newest first.
pub fn files(dir: &Path, prefix: &str, suffix: &str) -> Vec<FileInfo> {
    let (prefix, suffix) = (prefix.to_ascii_lowercase(), suffix.to_ascii_lowercase());
    let mut out: Vec<FileInfo> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            n.starts_with(&prefix) && n.ends_with(&suffix)
        })
        .filter_map(|e| info(e.path()))
        .collect();
    out.sort_by(|a, b| b.at.cmp(&a.at).then(b.name.cmp(&a.name)));
    out
}

/// The end of a text file (logs can be huge), lossy UTF-8.
pub fn read_tail(path: &Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max {
        let _ = f.seek(SeekFrom::Start(len - max));
    }
    let mut buf = Vec::new();
    let _ = f.take(max).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// «Invalid fixup» in a FiveM log: Some(file name or "") when the game reported a damaged model or texture file.
pub fn damaged_file(log: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for line in log.lines() {
        if !line.to_ascii_lowercase().contains("invalid fixup") {
            continue;
        }
        let name = line
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '(' | ')' | '[' | ']'))
            .find(|w| {
                let w = w.to_ascii_lowercase();
                GAME_FILES.iter().any(|ext| w.ends_with(ext))
            })
            .map(|w| w.rsplit(['/', '\\', ':']).next().unwrap_or(w).to_string());
        match name {
            Some(n) => return Some(n),
            None => found = found.or(Some(String::new())),
        }
    }
    found
}

// ---------------------------------------------------------------- GTA copies

/// GTA 5 folders in the Rockstar Games Launcher log: the «Installed titles» table, rows of gta5 with a Location.
pub fn rockstar_locations(log: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in log.lines() {
        let l = line.to_ascii_lowercase();
        let Some(i) = l.find("[titlestorage]") else { continue };
        let row = line[i + "[titlestorage]".len()..].trim_start();
        let first = row.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if !(first == "gta5" || first.starts_with("gta5_") || first == "gtav") {
            continue;
        }
        // the Location column: from the first «X:\» to the end of the row
        let bytes = row.as_bytes();
        if let Some(p) = (0..bytes.len().saturating_sub(2)).find(|&k| bytes[k].is_ascii_alphabetic() && bytes[k + 1] == b':' && bytes[k + 2] == b'\\') {
            let path = PathBuf::from(row[p..].trim());
            if !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// One spelling for comparing folders: lower case, `\`, no slash at the end.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

fn has_gta(dir: &Path) -> bool {
    dir.join("GTA5.exe").is_file() || dir.join("PlayGTAV.exe").is_file()
}

/// Folders with GTA 5 other than the one FiveM uses (existing ones only).
pub fn other_copies(fivem_gta: &Path, known: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for k in known {
        if !same_dir(k, fivem_gta) && has_gta(k) && !out.iter().any(|o| same_dir(o, k)) {
            out.push(k.clone());
        }
    }
    out
}

/// Every place on this PC that names a GTA 5 folder: the Rockstar launcher log and the registry entries.
pub fn known_gta_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(docs) = documents_dir() {
        let log = docs.join("Rockstar Games").join("Launcher").join("launcher.log");
        out.extend(rockstar_locations(&read_tail(&log, LOG_TAIL)));
    }
    out.extend(registry_gta_dirs());
    out
}

#[cfg(windows)]
fn documents_dir() -> Option<PathBuf> {
    crate::fivem::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Documents)
}

#[cfg(not(windows))]
fn documents_dir() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
pub fn desktop_dir() -> Option<PathBuf> {
    crate::fivem::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Desktop)
}

#[cfg(not(windows))]
pub fn desktop_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Desktop"))
}

/// Rockstar (`InstallFolder`), Steam (`InstallFolderSteam`) and Epic (`InstallFolderEpic`) copies of GTA 5.
#[cfg(windows)]
fn registry_gta_dirs() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY};
    use winreg::RegKey;
    let mut out = Vec::new();
    for key in [r"SOFTWARE\Rockstar Games\Grand Theft Auto V", r"SOFTWARE\Rockstar Games\GTAV"] {
        if let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(key, KEY_READ | KEY_WOW64_32KEY) {
            for value in ["InstallFolder", "InstallFolderSteam", "InstallFolderEpic"] {
                if let Ok(v) = k.get_value::<String, _>(value) {
                    let v = v.trim().trim_end_matches('\\');
                    if !v.is_empty() {
                        out.push(PathBuf::from(v));
                    }
                }
            }
        }
    }
    out
}

#[cfg(not(windows))]
fn registry_gta_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// GTA or FiveM inside OneDrive: the path names it, or lies under the OneDrive folders Windows knows.
pub fn in_onedrive(p: &Path, onedrive_roots: &[PathBuf]) -> bool {
    let s = p.to_string_lossy().to_lowercase();
    s.contains("\\onedrive\\") || s.contains("\\onedrive -") || onedrive_roots.iter().any(|r| !r.as_os_str().is_empty() && p.starts_with(r))
}

fn onedrive_roots() -> Vec<PathBuf> {
    ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"]
        .iter()
        .filter_map(|v| std::env::var_os(v))
        .map(PathBuf::from)
        .collect()
}

// ---------------------------------------------------------------- disks

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Drive {
    /// `C:\`
    pub root: String,
    pub free: u64,
    pub total: u64,
    /// "ssd" | "hdd" | "removable" | "network" | "unknown"
    pub kind: &'static str,
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Free space and the kind of disk a folder is on. HDD or SSD: Windows' «seek penalty» of the volume (the same
/// thing the «Optimize drives» window shows), asked without administrator rights.
#[cfg(windows)]
pub fn drive_of(p: &Path) -> Option<Drive> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceSeekPenaltyProperty, DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY,
        STORAGE_PROPERTY_QUERY,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None;
    }
    let letter = (b[0] as char).to_ascii_uppercase();
    let root = format!("{letter}:\\");
    unsafe {
        let w = wide(&root);
        let (mut free, mut total) = (0u64, 0u64);
        if GetDiskFreeSpaceExW(w.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) == 0 {
            return None;
        }
        let kind = match GetDriveTypeW(w.as_ptr()) {
            2 => "removable",
            4 => "network",
            3 => {
                let dev = wide(&format!("\\\\.\\{letter}:"));
                let h = CreateFileW(dev.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
                if h == INVALID_HANDLE_VALUE {
                    "unknown"
                } else {
                    let query = STORAGE_PROPERTY_QUERY {
                        PropertyId: StorageDeviceSeekPenaltyProperty,
                        QueryType: PropertyStandardQuery,
                        AdditionalParameters: [0],
                    };
                    let mut out: DEVICE_SEEK_PENALTY_DESCRIPTOR = std::mem::zeroed();
                    let mut got = 0u32;
                    let ok = DeviceIoControl(
                        h,
                        IOCTL_STORAGE_QUERY_PROPERTY,
                        &query as *const _ as *const core::ffi::c_void,
                        std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                        &mut out as *mut _ as *mut core::ffi::c_void,
                        std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
                        &mut got,
                        std::ptr::null_mut(),
                    );
                    CloseHandle(h);
                    if ok == 0 || got < 9 {
                        "unknown"
                    } else if out.IncursSeekPenalty != 0 {
                        "hdd"
                    } else {
                        "ssd"
                    }
                }
            }
            _ => "unknown",
        };
        Some(Drive { root, free, total, kind })
    }
}

#[cfg(not(windows))]
pub fn drive_of(_p: &Path) -> Option<Drive> {
    None
}

// ---------------------------------------------------------------- the hint

/// What the launcher found out (`gather`), kept apart from the decision (`hint`) so the decision can be tested.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// The newest crash dump within `HINT_FOR` (unix seconds).
    pub crash_at: Option<u64>,
    /// «Invalid fixup» in the newest FiveM log or the crash's game log: the file name ("" when the line names none).
    pub damaged: Option<String>,
    pub fivem_dir: Option<PathBuf>,
    pub gta_dir: Option<PathBuf>,
    /// GTA 5 copies other than the one FiveM uses.
    pub other_gta: Vec<PathBuf>,
    pub onedrive: bool,
    /// Free space on the FiveM disk.
    pub fivem_drive: Option<Drive>,
    pub gta_drive: Option<Drive>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hint {
    /// The window shows `hint_<key>`: fixup | low_disk | two_gta | onedrive | slow_disk | crash
    pub key: &'static str,
    /// For the text: the file name (fixup), the drive (low_disk), the folder FiveM uses (two_gta), GB free.
    pub detail: String,
    pub gb: u64,
    /// The crash this is about (unix seconds), 0: no crash (low disk). The window hides a hint the player closed
    /// until a newer crash comes.
    pub at: u64,
}

/// The one line, most likely cause first. None: nothing to say.
pub fn hint(f: &Facts) -> Option<Hint> {
    let at = f.crash_at.unwrap_or(0);
    let h = |key: &'static str, detail: String, gb: u64| Some(Hint { key, detail, gb, at });
    if let Some(file) = &f.damaged {
        return h("fixup", file.clone(), 0);
    }
    if let Some(d) = f.fivem_drive.as_ref().filter(|d| d.total > 0 && d.free < LOW_DISK_BYTES) {
        return h("low_disk", d.root.trim_end_matches('\\').to_string(), d.free / (1024 * 1024 * 1024));
    }
    f.crash_at?;
    if let (Some(gta), false) = (&f.gta_dir, f.other_gta.is_empty()) {
        return h("two_gta", gta.display().to_string(), 0);
    }
    if f.onedrive {
        return h("onedrive", String::new(), 0);
    }
    if f.gta_drive.as_ref().map_or(false, |d| matches!(d.kind, "hdd" | "removable")) {
        return h("slow_disk", String::new(), 0);
    }
    h("crash", String::new(), 0)
}

/// Everything the hint and the report need, from the real PC.
pub fn gather(fivem_exe: Option<&Path>, now: SystemTime) -> Facts {
    let mut f = Facts::default();
    let Some(exe) = fivem_exe else { return f };
    let app = app_dir(exe);
    f.fivem_dir = exe.parent().map(Path::to_path_buf);
    let since = unix(now).saturating_sub(HINT_FOR.as_secs());
    let dump = files(&app.join("crashes"), "", ".dmp").into_iter().find(|d| d.at >= since);
    f.crash_at = dump.as_ref().map(|d| d.at);
    if let Some(log) = files(&app.join("logs"), "CitizenFX_log_", ".log").into_iter().find(|l| l.at >= since) {
        f.damaged = damaged_file(&read_tail(&log.path, LOG_TAIL));
    }
    if f.damaged.is_none() {
        if let Some(d) = &dump {
            let gamelog = PathBuf::from(format!("{}.gamelog", d.path.display()));
            f.damaged = damaged_file(&read_tail(&gamelog, LOG_TAIL));
        }
    }
    f.gta_dir = fivem_gta_dir(&app);
    if let Some(gta) = &f.gta_dir {
        f.other_gta = other_copies(gta, &known_gta_dirs());
        f.gta_drive = drive_of(gta);
    }
    let roots = onedrive_roots();
    f.onedrive = f.gta_dir.iter().chain(f.fivem_dir.iter()).any(|p| in_onedrive(p, &roots));
    f.fivem_drive = drive_of(exe);
    f
}

// ---------------------------------------------------------------- cleaning texts

/// What to hide in the texts of the report.
#[derive(Clone, Debug, Default)]
pub struct Scrub {
    /// The Windows user name(s) and the PC name: `<user>`, `<pc>`.
    pub user: Vec<String>,
    pub pc: Vec<String>,
    /// IPv4 addresses that may stay: our server, the reserve routes and relays.
    pub keep_ips: Vec<String>,
}

impl Scrub {
    pub fn of_this_pc(keep_ips: Vec<String>) -> Scrub {
        let mut user: Vec<String> = Vec::new();
        for v in [std::env::var("USERNAME").ok(), std::env::var_os("USERPROFILE").and_then(|p| PathBuf::from(p).file_name().map(|n| n.to_string_lossy().into_owned()))] {
            if let Some(v) = v.filter(|v| !v.trim().is_empty()) {
                if !user.iter().any(|u| u.eq_ignore_ascii_case(&v)) {
                    user.push(v);
                }
            }
        }
        let pc = std::env::var("COMPUTERNAME").ok().filter(|v| !v.trim().is_empty()).into_iter().collect();
        Scrub { user, pc, keep_ips }
    }
}

/// The lower-case copy keeps every letter at the same byte offset, so an offset found in it can cut the text. 21.11:
/// equal total lengths were not enough («ẞ» gets shorter, «İ» longer: a text with both had offsets inside a letter,
/// a panic, and the launcher is built with `panic = "abort"`).
fn same_layout(text: &str, lower: &str) -> bool {
    lower.len() == text.len() && text.chars().all(|c| c.to_lowercase().map(char::len_utf8).sum::<usize>() == c.len_utf8())
}

/// Case-insensitive replace of `needle` where it stands as a whole word (not inside a longer name).
fn replace_word(text: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return text.to_string();
    }
    let lower = text.to_lowercase();
    let n = needle.to_lowercase();
    // to_lowercase may change lengths for some letters: then only the exact spelling is replaced
    if !same_layout(text, &lower) {
        return text.replace(needle, with);
    }
    let word = |c: Option<char>| c.map_or(false, |c| c.is_alphanumeric() || c == '_');
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(pos) = lower[i..].find(&n) {
        let start = i + pos;
        let end = start + n.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        out.push_str(&text[i..start]);
        if word(before) || word(after) {
            out.push_str(&text[start..end]);
        } else {
            out.push_str(with);
        }
        i = end;
    }
    out.push_str(&text[i..]);
    out
}

/// `a.b.c.d` with each part 0..255, not inside a longer number or version.
fn ipv4_at(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    for part in 0..4 {
        let s = i;
        while i < b.len() && b[i].is_ascii_digit() && i - s < 3 {
            i += 1;
        }
        if i == s || std::str::from_utf8(&b[s..i]).ok()?.parse::<u16>().ok()? > 255 {
            return None;
        }
        if part < 3 {
            if b.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
    }
    if b.get(i).map_or(false, |c| c.is_ascii_digit() || (*c == b'.' && b.get(i + 1).map_or(false, u8::is_ascii_digit))) {
        return None;
    }
    Some(i)
}

fn scrub_ips(text: &str, keep: &[String]) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        let boundary = i == 0 || !(b[i - 1].is_ascii_digit() || b[i - 1] == b'.');
        if boundary && b[i].is_ascii_digit() {
            if let Some(end) = ipv4_at(b, i) {
                let ip = &text[i..end];
                if !(ip.starts_with("127.") || ip == "0.0.0.0" || keep.iter().any(|k| k == ip)) {
                    out.push_str(&text[copied..i]);
                    out.push_str("<ip>");
                    copied = end;
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[copied..]);
    out
}

fn scrub_emails(text: &str) -> String {
    let b = text.as_bytes();
    let local = |c: u8| c.is_ascii_alphanumeric() || b"._%+-".contains(&c);
    let domain = |c: u8| c.is_ascii_alphanumeric() || c == b'.' || c == b'-';
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (at, _) in text.match_indices('@') {
        if at < copied {
            continue;
        }
        let mut s = at;
        while s > copied && local(b[s - 1]) {
            s -= 1;
        }
        let mut e = at + 1;
        while e < b.len() && domain(b[e]) {
            e += 1;
        }
        let dom = text[at + 1..e].trim_end_matches('.');
        if s < at && dom.contains('.') && !dom.starts_with('.') {
            out.push_str(&text[copied..s]);
            out.push_str("<email>");
            copied = at + 1 + dom.len();
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// The Rockstar account number FiveM writes («Account id 264241266»).
fn scrub_account(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("Account id ") {
        let (head, tail) = rest.split_at(pos + "Account id ".len());
        out.push_str(head);
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            out.push_str("<id>");
        }
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out
}

/// A text of the report with the private parts hidden (module doc).
pub fn scrub(text: &str, s: &Scrub) -> String {
    let mut t = text.to_string();
    // e-mail addresses first: a user name inside one must not break it apart
    t = scrub_emails(&t);
    for u in &s.user {
        for (sep, esc) in [("\\", "\\"), ("\\\\", "\\\\"), ("/", "/")] {
            let from = format!("{sep}Users{esc}{u}");
            let to = format!("{sep}Users{esc}<user>");
            // case-insensitive for the path form too
            let lower = t.to_lowercase();
            if same_layout(&t, &lower) {
                let f = from.to_lowercase();
                let mut out = String::new();
                let mut i = 0;
                while let Some(p) = lower[i..].find(&f) {
                    let end = i + p + f.len();
                    // «Users\Ivan», not «Users\Ivanov»
                    let longer = t[end..].chars().next().map_or(false, |c| c.is_alphanumeric() || c == '_' || c == '.');
                    out.push_str(&t[i..i + p]);
                    out.push_str(if longer { &t[i + p..end] } else { &to });
                    i = end;
                }
                out.push_str(&t[i..]);
                t = out;
            } else {
                t = t.replace(&from, &to);
            }
        }
        if u.chars().count() >= 3 {
            t = replace_word(&t, u, "<user>");
        }
    }
    for pc in &s.pc {
        if pc.chars().count() >= 3 {
            t = replace_word(&t, pc, "<pc>");
        }
    }
    scrub_account(&scrub_ips(&t, &s.keep_ips))
}

// ---------------------------------------------------------------- zip (stored, no extra crate)

pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *v = c;
        }
        t
    });
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// A zip with the files as they are («stored»: logs are small, a crash dump is big but the player sends it once).
/// Names are UTF-8 (flag bit 11), so Windows Explorer opens them right.
pub fn zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let put16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let put32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    // a fixed DOS date (1.01.2026 00:00): the real dates are in system.txt
    let (time, date) = (0u16, ((2026 - 1980) << 9 | 1 << 5 | 1) as u16);
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let n = name.as_bytes();
        put32(&mut out, 0x0403_4b50);
        put16(&mut out, 20);
        put16(&mut out, 0x0800);
        put16(&mut out, 0);
        put16(&mut out, time);
        put16(&mut out, date);
        put32(&mut out, crc);
        put32(&mut out, data.len() as u32);
        put32(&mut out, data.len() as u32);
        put16(&mut out, n.len() as u16);
        put16(&mut out, 0);
        out.extend_from_slice(n);
        out.extend_from_slice(data);

        put32(&mut central, 0x0201_4b50);
        put16(&mut central, 20);
        put16(&mut central, 20);
        put16(&mut central, 0x0800);
        put16(&mut central, 0);
        put16(&mut central, time);
        put16(&mut central, date);
        put32(&mut central, crc);
        put32(&mut central, data.len() as u32);
        put32(&mut central, data.len() as u32);
        put16(&mut central, n.len() as u16);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put32(&mut central, 0);
        put32(&mut central, offset);
        central.extend_from_slice(n);
    }
    let cd_offset = out.len() as u32;
    let cd_len = central.len() as u32;
    out.extend_from_slice(&central);
    put32(&mut out, 0x0605_4b50);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, entries.len() as u16);
    put16(&mut out, entries.len() as u16);
    put32(&mut out, cd_len);
    put32(&mut out, cd_offset);
    put16(&mut out, 0);
    out
}

// ---------------------------------------------------------------- the report

/// What goes into the zip, shown to the player before it is made.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub files: Vec<FileInfo>,
    /// The newest crash dump of the last day: only with the tick.
    pub dump: Option<FileInfo>,
    /// Crash dumps of the last day (listed in system.txt by name, size and time).
    pub dumps_today: usize,
}

/// Launcher logs, the newest FiveM log, the game log of the newest crash; `system.txt` is made at the end.
pub fn plan(launcher_logs: &[PathBuf], fivem_exe: Option<&Path>, now: SystemTime) -> (Plan, Vec<FileInfo>) {
    let mut p = Plan::default();
    for l in launcher_logs {
        if let Some(i) = info(l.clone()) {
            p.files.push(i);
        }
    }
    let mut dumps_today = Vec::new();
    if let Some(exe) = fivem_exe {
        let app = app_dir(exe);
        if let Some(log) = files(&app.join("logs"), "CitizenFX_log_", ".log").into_iter().next() {
            p.files.push(log);
        }
        let since = unix(now).saturating_sub(REPORT_DUMPS_FOR.as_secs());
        dumps_today = files(&app.join("crashes"), "", ".dmp").into_iter().filter(|d| d.at >= since).collect::<Vec<_>>();
        if let Some(d) = dumps_today.first() {
            if let Some(g) = info(PathBuf::from(format!("{}.gamelog", d.path.display()))) {
                p.files.push(g);
            }
            p.dump = Some(d.clone());
        }
        // FiveM's own crash packs, when the player saved one from FiveM's crash window
        if let Some(desk) = desktop_dir() {
            dumps_today.extend(files(&desk, "CfxCrashDump", ".zip").into_iter().filter(|d| d.at >= since));
        }
    }
    p.dumps_today = dumps_today.len();
    p.files.push(FileInfo { path: PathBuf::new(), name: "system.txt".into(), size: 0, at: unix(now) });
    (p, dumps_today)
}

/// Is this file a text (cleaned before it goes in)?
fn is_text(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".log") || n.ends_with(".txt") || n.ends_with(".gamelog") || n.ends_with(".ini") || n.ends_with(".json")
}

/// The zip bytes: every planned file (texts cleaned), `system.txt`, the dump only with `with_dump`.
pub fn build(plan: &Plan, system: &str, with_dump: bool, s: &Scrub) -> Vec<u8> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for f in plan.files.iter().filter(|f| !f.path.as_os_str().is_empty()) {
        let data = if is_text(&f.name) {
            scrub(&read_tail(&f.path, 8 * 1024 * 1024), s).into_bytes()
        } else {
            std::fs::read(&f.path).unwrap_or_default()
        };
        entries.push((f.name.clone(), data));
    }
    entries.push(("system.txt".into(), scrub(system, s).into_bytes()));
    if with_dump {
        if let Some(d) = &plan.dump {
            if let Ok(data) = std::fs::read(&d.path) {
                entries.push((d.name.clone(), data));
            }
        }
    }
    zip(&entries)
}

/// Windows version for system.txt: «Windows 11 24H2 (сборка 26100.2314)».
#[cfg(windows)]
pub fn windows_version() -> String {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") else {
        return "Windows (неизвестно)".into();
    };
    let s = |v: &str| k.get_value::<String, _>(v).unwrap_or_default();
    let build = s("CurrentBuild");
    let ubr = k.get_value::<u32, _>("UBR").map(|u| format!(".{u}")).unwrap_or_default();
    let mut product = s("ProductName");
    // Windows 11 still says «Windows 10» in ProductName
    if build.parse::<u32>().map_or(false, |b| b >= 22000) {
        product = product.replace("Windows 10", "Windows 11");
    }
    format!("{product} {} (сборка {build}{ubr})", s("DisplayVersion")).replace("  ", " ")
}

#[cfg(not(windows))]
pub fn windows_version() -> String {
    std::env::consts::OS.to_string()
}

fn gb(b: u64) -> String {
    format!("{:.1} ГБ", b as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// `system.txt`: what support asks first.
pub fn system_text(launcher_version: &str, facts: &Facts, hint: Option<&Hint>, dumps: &[FileInfo], extra: &[String]) -> String {
    let mut t = String::new();
    let stamp = |s: u64| crate::fivem::utc_stamp(s);
    t.push_str(&format!("Отчёт лаунчера COLDZ {launcher_version}, {}\r\n", stamp(unix(SystemTime::now()))));
    t.push_str(&format!("Windows: {}\r\n", windows_version()));
    t.push_str(&format!(
        "FiveM: {}\r\n",
        facts.fivem_dir.as_ref().map_or("не найден".to_string(), |d| d.display().to_string())
    ));
    t.push_str(&format!(
        "GTA 5 (папка у FiveM, CitizenFX.ini IVPath): {}\r\n",
        facts.gta_dir.as_ref().map_or("не указана".to_string(), |d| d.display().to_string())
    ));
    for o in &facts.other_gta {
        t.push_str(&format!("Другая копия GTA 5: {}\r\n", o.display()));
    }
    for (what, d) in [("FiveM", &facts.fivem_drive), ("GTA 5", &facts.gta_drive)] {
        if let Some(d) = d {
            t.push_str(&format!("Диск {what}: {} {}, свободно {} из {}\r\n", d.root, d.kind, gb(d.free), gb(d.total)));
        }
    }
    t.push_str(&format!("OneDrive: {}\r\n", if facts.onedrive { "да" } else { "нет" }));
    if let Some(f) = &facts.damaged {
        t.push_str(&format!("В журнале FiveM «Invalid fixup»: {}\r\n", if f.is_empty() { "файл не назван" } else { f }));
    }
    t.push_str(&format!("Подсказка лаунчера: {}\r\n", hint.map_or("нет", |h| h.key)));
    t.push_str(&format!("Вылеты за сутки: {}\r\n", dumps.len()));
    for d in dumps {
        t.push_str(&format!("    {} {} байт {}\r\n", d.name, d.size, stamp(d.at)));
    }
    for e in extra {
        t.push_str(e);
        t.push_str("\r\n");
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fivem_ini_and_rockstar_log() {
        let ini = "[Game]\r\nIVPath=E:\\SteamLibrary\\steamapps\\common\\Grand Theft Auto V\r\nSavedBuildNumber=3258\r\n";
        assert_eq!(ini_value(ini, "IVPath").as_deref(), Some("E:\\SteamLibrary\\steamapps\\common\\Grand Theft Auto V"));
        assert_eq!(ini_value(ini, "ivpath").is_some(), true);
        assert_eq!(ini_value(ini, "Nope"), None);

        // the «Installed titles» table of the Rockstar Games Launcher
        let log = "[2026-11-18 08:44:06.947] [DISPLAY] [Main ] [titlestorage] Installed titles:\r\n\
            [2026-11-18 08:44:06.947] [DISPLAY] [Main ] [titlestorage]  Title       Progress   Version     Location\r\n\
            [2026-11-18 08:44:06.947] [DISPLAY] [Main ] [titlestorage]  gta5        Installed  1.0.3258.0  D:\\Games\\Grand Theft Auto V\r\n\
            [2026-11-18 08:44:06.947] [DISPLAY] [Main ] [titlestorage]  rdr2        Installed  1.0.1491.0  D:\\Games\\RDR2\r\n\
            [2026-11-18 08:44:07.648] [DISPLAY] [Main ] [folderfix] Requesting fix of non-admin folder locations\r\n";
        assert_eq!(rockstar_locations(log), vec![PathBuf::from("D:\\Games\\Grand Theft Auto V")]);
        assert!(rockstar_locations("").is_empty());
        assert!(same_dir(Path::new("D:\\Games\\GTA\\"), Path::new("d:/games/gta")));
    }

    #[test]
    fn finds_damaged_game_files_in_the_log() {
        let log = "[  9000] [ GTAProcess] MainThrd/ loading x\n\
            [ 12000] [ GTAProcess] MainThrd/ Invalid fixup, address is neither virtual nor physical (in update:/x64/levels/gta5/props/prop_tree.ydr)\n";
        assert_eq!(damaged_file(log).as_deref(), Some("prop_tree.ydr"));
        assert_eq!(damaged_file("Invalid fixup in something").as_deref(), Some(""));
        assert_eq!(damaged_file("all good\nno problems"), None);
    }

    fn drive(free_gb: u64, kind: &'static str) -> Drive {
        Drive { root: "E:\\".into(), free: free_gb * 1024 * 1024 * 1024, total: 1000 * 1024 * 1024 * 1024, kind }
    }

    #[test]
    fn one_hint_most_likely_cause_first() {
        let crash = Facts { crash_at: Some(100), fivem_drive: Some(drive(50, "ssd")), gta_drive: Some(drive(50, "ssd")), ..Default::default() };
        // a crash with nothing else known: «FiveM вылетал, соберите отчёт»
        assert_eq!(hint(&crash).map(|h| h.key), Some("crash"));
        // no crash, nothing to say
        assert_eq!(hint(&Facts { crash_at: None, ..crash.clone() }), None);
        // slow disk < OneDrive < two copies < little space < damaged file
        let mut f = Facts { gta_drive: Some(drive(50, "hdd")), ..crash.clone() };
        assert_eq!(hint(&f).map(|h| h.key), Some("slow_disk"));
        f.onedrive = true;
        assert_eq!(hint(&f).map(|h| h.key), Some("onedrive"));
        f.gta_dir = Some(PathBuf::from("E:\\Steam\\GTA V"));
        f.other_gta = vec![PathBuf::from("D:\\Games\\GTA V")];
        let h = hint(&f).unwrap();
        assert_eq!((h.key, h.detail.as_str(), h.at), ("two_gta", "E:\\Steam\\GTA V", 100));
        f.fivem_drive = Some(drive(3, "ssd"));
        let h = hint(&f).unwrap();
        assert_eq!((h.key, h.detail.as_str(), h.gb), ("low_disk", "E:", 3));
        f.damaged = Some("prop_tree.ydr".into());
        assert_eq!(hint(&f).map(|h| (h.key, h.detail)), Some(("fixup", "prop_tree.ydr".to_string())));
        // little space is said without a crash too
        let low = Facts { fivem_drive: Some(drive(1, "ssd")), ..Default::default() };
        assert_eq!(hint(&low).map(|h| (h.key, h.at)), Some(("low_disk", 0)));
    }

    #[test]
    fn onedrive_paths() {
        let roots = vec![PathBuf::from("C:\\Users\\Ivan\\OneDrive")];
        assert!(in_onedrive(Path::new("C:\\Users\\Ivan\\OneDrive\\Games\\GTA V"), &roots));
        assert!(in_onedrive(Path::new("C:\\Users\\Ivan\\OneDrive - Work\\x"), &[]));
        assert!(!in_onedrive(Path::new("D:\\Games\\GTA V"), &roots));
    }

    #[test]
    fn hides_private_parts() {
        let s = Scrub { user: vec!["Ivan".into()], pc: vec!["DESKTOP-ABC123".into()], keep_ips: vec!["92.118.11.28".into()] };
        let text = "hello from \"C:\\Users\\ivan\\AppData\\Local\\FiveM\\FiveM.exe\" \"fivem://connect/92.118.11.28:28683\"\n\
            json \"C:\\\\Users\\\\Ivan\\\\Documents\" machine DESKTOP-ABC123, user Ivan; Ivanov stays\n\
            Account id 264241266, mail ivan.petrov+x@mail.ru. peer 85.140.1.2:443, local 127.0.0.1, build 1.0.3258.0, v 2.4.0.302";
        let out = scrub(text, &s);
        assert!(out.contains("C:\\Users\\<user>\\AppData"), "{out}");
        assert!(out.contains("C:\\\\Users\\\\<user>\\\\Documents"), "{out}");
        assert!(out.contains("machine <pc>, user <user>; Ivanov stays"), "{out}");
        assert!(out.contains("Account id <id>"), "{out}");
        assert!(out.contains("mail <email>."), "{out}");
        assert!(out.contains("92.118.11.28:28683"), "{out}");
        assert!(out.contains("peer <ip>:443"), "{out}");
        assert!(out.contains("local 127.0.0.1"), "{out}");
        assert!(out.contains("build 1.0.3258.0"), "{out}");
        assert!(!out.to_lowercase().contains("\\ivan\\"), "{out}");
    }

    // 21.11: letters whose lower case is shorter («ẞ») and longer («İ») in one text: offsets stay inside letters
    // only with the old «equal total length» check; now no panic, the exact spelling is still hidden
    #[test]
    fn scrub_survives_letters_that_change_length() {
        let s = Scrub { user: vec!["Ivan".into()], pc: vec!["DESKTOP-ABC".into()], keep_ips: vec![] };
        for text in ["ẞIvanİ C:\\Users\\Ivan\\x", "İẞ C:\\Users\\Ivan\\FiveM DESKTOP-ABC", "ẞ\\Users\\Ivan İ", "İ", "ẞ"] {
            let out = scrub(text, &s);
            assert!(!out.contains("\\Users\\Ivan\\") && !out.contains("DESKTOP-ABC"), "{text} -> {out}");
        }
        assert!(same_layout("Привет, Иван", &"Привет, Иван".to_lowercase()));
        assert!(!same_layout("ẞİ", &"ẞİ".to_lowercase()));
    }

    #[test]
    fn zip_is_readable() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let z = zip(&[("a.txt".into(), b"hello".to_vec()), ("b.bin".into(), vec![1, 2, 3])]);
        assert_eq!(&z[..4], &[0x50, 0x4b, 0x03, 0x04]);
        let eocd = z.len() - 22;
        assert_eq!(&z[eocd..eocd + 4], &[0x50, 0x4b, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([z[eocd + 10], z[eocd + 11]]), 2);
        // Windows' own zip reader, when there is one: the files come back as they were
        #[cfg(windows)]
        {
            let dir = std::env::temp_dir().join(format!("coldz-zip-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("t.zip");
            std::fs::write(&file, &z).unwrap();
            let out = dir.join("out");
            let _ = std::fs::write(std::env::temp_dir().join("coldz-zip-test.zip"), &z);
            let ok = std::process::Command::new("tar").arg("-xf").arg(&file).arg("-C").arg({
                std::fs::create_dir_all(&out).unwrap();
                &out
            })
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
            if ok {
                assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"hello");
                assert_eq!(std::fs::read(out.join("b.bin")).unwrap(), vec![1, 2, 3]);
            } else {
                eprintln!("tar not available: zip not unpacked");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn plan_and_build_from_a_fake_fivem() {
        let d = std::env::temp_dir().join(format!("coldz-support-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let app = d.join("FiveM").join("FiveM.app");
        std::fs::create_dir_all(app.join("logs")).unwrap();
        std::fs::create_dir_all(app.join("crashes")).unwrap();
        std::fs::write(d.join("FiveM").join("FiveM.exe"), b"MZ").unwrap();
        std::fs::write(app.join("logs").join("CitizenFX_log_2026-11-18T134401.log"), "C:\\Users\\Ivan\\x Invalid fixup (x.ydr)").unwrap();
        std::fs::write(app.join("crashes").join("aa.dmp"), vec![0u8; 100]).unwrap();
        std::fs::write(app.join("crashes").join("aa.dmp.gamelog"), "game log").unwrap();
        std::fs::write(app.join("CitizenFX.ini"), "[Game]\nIVPath=Z:\\nope\\GTA\n").unwrap();
        let net = d.join("net-check.log");
        std::fs::write(&net, "проверка связи").unwrap();
        let exe = d.join("FiveM").join("FiveM.exe");

        let facts = gather(Some(&exe), SystemTime::now());
        assert!(facts.crash_at.is_some());
        assert_eq!(facts.damaged.as_deref(), Some("x.ydr"));
        assert_eq!(facts.gta_dir, Some(PathBuf::from("Z:\\nope\\GTA")));
        assert_eq!(hint(&facts).map(|h| h.key), Some("fixup"));

        let (p, dumps) = plan(&[net, d.join("missing.log")], Some(&exe), SystemTime::now());
        let names: Vec<&str> = p.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["net-check.log", "CitizenFX_log_2026-11-18T134401.log", "aa.dmp.gamelog", "system.txt"]);
        assert_eq!(p.dump.as_ref().map(|x| x.name.as_str()), Some("aa.dmp"));
        assert!(!dumps.is_empty());
        let s = Scrub { user: vec!["Ivan".into()], ..Default::default() };
        let without = build(&p, "sys", false, &s);
        let with = build(&p, "sys", true, &s);
        assert!(with.len() >= without.len() + 100);
        let text = String::from_utf8_lossy(&without);
        assert!(text.contains("C:\\Users\\<user>\\x"), "{text}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn looks_at_the_real_pc_without_errors() {
        let (found, _) = crate::fivem::search(&crate::fivem::System { manual: None });
        let facts = gather(found.as_ref().map(|f| f.path.as_path()), SystemTime::now());
        eprintln!("{facts:?}");
        eprintln!("hint: {:?}", hint(&facts));
        eprintln!("windows: {}", windows_version());
        if let Some(d) = drive_of(Path::new("C:\\")) {
            assert!(d.total > 0);
            eprintln!("C: {d:?}");
        }
    }
}
