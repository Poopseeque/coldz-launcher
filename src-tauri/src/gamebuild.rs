//! 0.5.4: FiveM starts on the game build COLDZ needs (README 7м).
//!
//! What was (20.11.2026, five crash dumps of one player, `F:\COLDZ-work\crash_2011_sanych\разбор.md`): FiveM that
//! joins a server with another game build either says «FiveM needs to restart» or downloads the build's game files
//! INSIDE the running game («Updating game storage … 1423 MB»), and the game is closed about 96 s after it started:
//! with a slow connection the download never fits. Started on the right build, FiveM downloads the build data in its
//! own start window, before the game («FiveM needs to update the local game data … download X GB» → Continue),
//! where there is no timer.
//!
//! How FiveM decides (citizenfx/fivem 5412ed86, 11.11.2026):
//!   * `code/components/net/src/NetLibrary.cpp`: on every join FiveM reads `vars` of the server's `/info.json`:
//!     `sv_enforceGameBuild` (the content build; 3717 is remapped to 3751), `sv_pureLevel` (absent = 0),
//!     `sv_poolSizesIncrease` (a JSON object, absent = none), `sv_defaultGameBuild` (the exe build, used when it is not
//!     below the client's own default 3258; without it, `sv_replaceExeToSwitchBuilds` != "false" means the exe of the
//!     enforced build). Any of them different from what FiveM runs with: FiveM restarts into it
//!     (`ConnectToNative.cpp RestartGameToOtherBuild`) and writes `PoolSizesIncrease`, `DefaultBuild`, `PureLevel`
//!     (and `SavedBuildNumber` only for the default build) to `FiveM.app\CitizenFX.ini`, section `[Game]`.
//!   * `code/client/launcher/CrossBuildLaunch.cpp`, `code/client/shared/XBRInit.cpp`,
//!     `code/components/pool-sizes-state/src/PoolSizesState.cpp`: a FiveM started without `-b<build>` takes the build
//!     from `SavedBuildNumber` (absent = its default), the pure level from `PureLevel` (1 or 2, else 0), the exe build
//!     from `DefaultBuild` (absent = its default), the pools from `PoolSizesIncrease` (Win32 profile API: the first
//!     `[Game]` section, the first key of a name, names in any case, values trimmed).
//!
//! So before «Играть» (lib.rs `game_build`, `play`) the launcher reads `vars` of the chosen route's `/info.json` and,
//! when FiveM is NOT running (a running FiveM writes the file itself when it exits), sets the same four keys FiveM
//! would set after a restart. Only those lines change; every other line (IVPath and the rest), the line ends and
//! the encoding (ANSI bytes as they are; UTF-16 with a BOM stays UTF-16) stay. A file that already matches is not
//! written. No file: a new one with `[Game]` (FiveM asks for the GTA folder as on its first start). No `FiveM.app`
//! folder: nothing is created. Any error: logged (fivem-search.log), FiveM starts as before.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const INI: &str = "CitizenFX.ini";
const SECTION: &str = "Game";
/// FiveM's own default GTA 5 build (`CrossBuildRuntime.h GetDefaultGTA5Build`, 11.11.2026): a server default exe
/// build below it is ignored by FiveM.
pub const CLIENT_DEFAULT_BUILD: u32 = 3258;

pub const KEY_BUILD: &str = "SavedBuildNumber";
pub const KEY_DEFAULT: &str = "DefaultBuild";
pub const KEY_PURE: &str = "PureLevel";
pub const KEY_POOLS: &str = "PoolSizesIncrease";

/// What the server asks for. `None`: the launcher does not touch that key.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    /// `sv_enforceGameBuild` → `SavedBuildNumber`.
    pub build: u32,
    /// `sv_defaultGameBuild` (or the enforced build on old servers) → `DefaultBuild`.
    pub default_build: Option<u32>,
    /// `sv_pureLevel` → `PureLevel`.
    pub pure: Option<u32>,
    /// `sv_poolSizesIncrease` → `PoolSizesIncrease`; an empty map = no increase.
    pub pools: Option<BTreeMap<String, u32>>,
}

impl Wanted {
    /// One line for the log: «сборка 3258, exe 3889, pure 1, пулы {...}».
    pub fn describe(&self) -> String {
        let mut parts = vec![format!("сборка {}", self.build)];
        if let Some(d) = self.default_build {
            parts.push(format!("exe {d}"));
        }
        if let Some(p) = self.pure {
            parts.push(format!("pure {p}"));
        }
        if let Some(m) = &self.pools {
            parts.push(format!("пулы {}", if m.is_empty() { "нет".to_string() } else { pools_text(m) }));
        }
        parts.join(", ")
    }
}

/// A var as FiveM sends it (a string; a number is taken too): digits only.
fn var_number(vars: &serde_json::Value, key: &str) -> Option<Option<u32>> {
    match vars.get(key) {
        None | Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::Number(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()).map(Some),
        Some(serde_json::Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                Some(None)
            } else if s.bytes().all(|b| b.is_ascii_digit()) && s.len() <= 9 {
                s.parse().ok().map(Some)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn var_text<'a>(vars: &'a serde_json::Value, key: &str) -> &'a str {
    vars.get(key).and_then(|v| v.as_str()).unwrap_or("").trim()
}

/// `{"InteriorProxy":450,"PortalInst":100}`: compact, keys sorted (as FiveM writes it).
pub fn pools_text(m: &BTreeMap<String, u32>) -> String {
    serde_json::to_string(m).unwrap_or_default()
}

/// A pool JSON object; "" = none. Not an object of names to whole numbers: None.
pub fn parse_pools(raw: &str) -> Option<BTreeMap<String, u32>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(BTreeMap::new());
    }
    serde_json::from_str(raw).ok()
}

/// What `/info.json` asks for. Err: no `vars` object, or no usable `sv_enforceGameBuild` (then nothing is touched).
pub fn wanted(info: &serde_json::Value) -> Result<Wanted, &'static str> {
    let vars = info.get("vars").filter(|v| v.is_object()).ok_or("нет vars")?;
    let raw_build = var_text(vars, "sv_enforceGameBuild");
    let build = match var_number(vars, "sv_enforceGameBuild") {
        Some(Some(b)) if b > 0 => b,
        _ => return Err("нет sv_enforceGameBuild"),
    };
    // NetLibrary.cpp: «remap old build numbers»
    let build = if build == 3717 { 3751 } else { build };
    let default_build = match var_number(vars, "sv_defaultGameBuild") {
        Some(Some(d)) if d >= CLIENT_DEFAULT_BUILD => Some(d),
        // no (usable) server default: old servers replace the exe with the enforced build's one unless they say no
        _ if var_text(vars, "sv_replaceExeToSwitchBuilds") != "false" && raw_build != "1" => Some(build),
        // FiveM's own default: not written (it is FiveM's, it may change)
        _ => None,
    };
    let pure = match var_number(vars, "sv_pureLevel") {
        Some(None) => Some(0),
        Some(Some(p)) if p <= 2 => Some(p),
        _ => None,
    };
    let pools = parse_pools(match vars.get("sv_poolSizesIncrease") {
        Some(serde_json::Value::String(s)) => s.as_str(),
        None | Some(serde_json::Value::Null) => "",
        // an object instead of a string: take it as it is
        Some(other) => return Ok(Wanted { build, default_build, pure, pools: parse_pools(&other.to_string()) }),
    });
    Ok(Wanted { build, default_build, pure, pools })
}

// ---------------------------------------------------------------- the ini file (bytes)

/// Lines of the file: (start, end of the text, end with the line break).
fn lines(b: &[u8]) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < b.len() {
        let nl = b[start..].iter().position(|&c| c == b'\n').map(|i| start + i);
        let full = nl.map_or(b.len(), |i| i + 1);
        let mut end = nl.unwrap_or(b.len());
        if end > start && b[end - 1] == b'\r' {
            end -= 1;
        }
        out.push((start, end, full));
        start = full;
    }
    out
}

fn trim(s: &[u8]) -> &[u8] {
    let a = s.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(s.len());
    let z = s.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(a, |i| i + 1);
    &s[a..z]
}

/// A UTF-8 BOM at the very start belongs to no line.
fn strip_bom(s: &[u8]) -> &[u8] {
    s.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(s)
}

/// `[Name]` → Name.
fn section_of(text: &[u8]) -> Option<&[u8]> {
    let t = trim(strip_bom(text));
    let rest = t.strip_prefix(b"[")?;
    let close = rest.iter().position(|&c| c == b']')?;
    Some(trim(&rest[..close]))
}

/// `key=value` → (key, value), both trimmed (Win32 profile API).
fn key_of(text: &[u8]) -> Option<(&[u8], &[u8])> {
    let eq = text.iter().position(|&c| c == b'=')?;
    Some((trim(&text[..eq]), trim(&text[eq + 1..])))
}

/// The line range of the first `[Game]` section: (header line, first line after it). None: no such section.
fn game_section(ls: &[(usize, usize, usize)], b: &[u8]) -> Option<(usize, usize)> {
    let head = ls.iter().position(|&(s, e, _)| section_of(&b[s..e]).is_some_and(|n| n.eq_ignore_ascii_case(SECTION.as_bytes())))?;
    let end = ls
        .iter()
        .enumerate()
        .skip(head + 1)
        .find(|(_, l)| section_of(&b[l.0..l.1]).is_some())
        .map_or(ls.len(), |(i, _)| i);
    Some((head, end))
}

/// The line index of `key` inside the section (the first one, as Win32 reads it).
fn find_key(ls: &[(usize, usize, usize)], b: &[u8], range: (usize, usize), key: &str) -> Option<usize> {
    (range.0 + 1..range.1).find(|&i| {
        let (s, e, _) = ls[i];
        key_of(&b[s..e]).is_some_and(|(k, _)| k.eq_ignore_ascii_case(key.as_bytes()))
    })
}

/// The value of `key` in `[Game]` as FiveM reads it (quotes around it dropped, like Win32). None: no such key.
pub fn value(b: &[u8], key: &str) -> Option<String> {
    let ls = lines(b);
    let range = game_section(&ls, b)?;
    let i = find_key(&ls, b, range, key)?;
    let (s, e, _) = ls[i];
    let (_, v) = key_of(&b[s..e])?;
    let v = match v {
        [q, inner @ .., z] if (*q == b'"' || *q == b'\'') && q == z => inner,
        v => v,
    };
    Some(String::from_utf8_lossy(v).into_owned())
}

/// `GetPrivateProfileInt`: the leading digits of the value. None: no key or no digits.
fn profile_int(v: Option<&str>) -> Option<u32> {
    let v = v?.trim();
    let digits: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// The line break of the file: CRLF unless it only has LF (an empty file: CRLF, as Windows writes it).
fn newline(b: &[u8]) -> &'static [u8] {
    if b.windows(2).any(|w| w == b"\r\n") || !b.contains(&b'\n') {
        b"\r\n"
    } else {
        b"\n"
    }
}

/// Sets `key=value` in the first `[Game]` section: the key's line is replaced (its line break kept), a missing key goes
/// after the last non-empty line of the section, a missing section goes to the end. Nothing else changes.
pub fn set_values(b: &[u8], pairs: &[(&str, String)]) -> Vec<u8> {
    let nl = newline(b);
    let mut out = b.to_vec();
    for (key, val) in pairs {
        let ls = lines(&out);
        let line = format!("{key}={val}").into_bytes();
        let Some(range) = game_section(&ls, &out) else {
            if !out.is_empty() && !out.ends_with(b"\n") {
                out.extend_from_slice(nl);
            }
            out.extend_from_slice(b"[Game]");
            out.extend_from_slice(nl);
            out.extend_from_slice(&line);
            out.extend_from_slice(nl);
            continue;
        };
        if let Some(i) = find_key(&ls, &out, range, key) {
            let (s, e, _) = ls[i];
            drop(out.splice(s..e, line));
            continue;
        }
        // after the last non-empty line of the section (the header at least)
        let last = (range.0..range.1).rev().find(|&i| !trim(&out[ls[i].0..ls[i].1]).is_empty()).unwrap_or(range.0);
        let (_, e, full) = ls[last];
        let mut insert = Vec::new();
        if full == e {
            // the last line of the file without a line break
            insert.extend_from_slice(nl);
        }
        insert.extend_from_slice(&line);
        insert.extend_from_slice(nl);
        drop(out.splice(full..full, insert));
    }
    out
}

/// One key to change: what the file had (None: no key) and what goes there.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub key: &'static str,
    pub was: Option<String>,
    pub now: String,
}

/// Which keys differ from what the server asks for, compared the way FiveM compares them.
pub fn plan(b: &[u8], w: &Wanted) -> Vec<Change> {
    let mut out = Vec::new();
    let mut add = |key: &'static str, now: String| out.push(Change { key, was: value(b, key), now });
    // the build and the exe build: an absent key means FiveM's own default, which is FiveM's to change: write it
    if profile_int(value(b, KEY_BUILD).as_deref()) != Some(w.build) {
        add(KEY_BUILD, w.build.to_string());
    }
    if let Some(d) = w.default_build {
        if profile_int(value(b, KEY_DEFAULT).as_deref()) != Some(d) {
            add(KEY_DEFAULT, d.to_string());
        }
    }
    // the pure level: FiveM uses 1 or 2 from the file, anything else is 0
    if let Some(p) = w.pure {
        let now = profile_int(value(b, KEY_PURE).as_deref()).filter(|n| (1..=2).contains(n)).unwrap_or(0);
        if now != p {
            add(KEY_PURE, p.to_string());
        }
    }
    // the pools: compared as maps (FiveM does); a broken value FiveM clears itself (= none)
    if let Some(m) = &w.pools {
        let now = value(b, KEY_POOLS).and_then(|v| parse_pools(&v)).unwrap_or_default();
        if &now != m {
            add(KEY_POOLS, if m.is_empty() { String::new() } else { pools_text(m) });
        }
    }
    out
}

/// The file in its encoding → bytes to edit (UTF-8 for UTF-16 files), and back.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Encoding {
    /// ANSI or UTF-8 as they are (only ASCII lines are written).
    Bytes,
    /// UTF-16 LE with a BOM (the Win32 profile API keeps such a file Unicode).
    Utf16,
}

fn decode(raw: &[u8]) -> Result<(Vec<u8>, Encoding), &'static str> {
    if let Some(rest) = raw.strip_prefix(b"\xFF\xFE") {
        if rest.len() % 2 != 0 {
            return Err("испорченный UTF-16");
        }
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16(&units).map_err(|_| "испорченный UTF-16")?;
        return Ok((text.into_bytes(), Encoding::Utf16));
    }
    if raw.starts_with(b"\xFE\xFF") {
        return Err("UTF-16 BE");
    }
    Ok((raw.to_vec(), Encoding::Bytes))
}

fn encode(b: Vec<u8>, enc: Encoding) -> Vec<u8> {
    match enc {
        Encoding::Bytes => b,
        Encoding::Utf16 => {
            let text = String::from_utf8_lossy(&b);
            let mut out = vec![0xFF, 0xFE];
            for u in text.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            out
        }
    }
}

/// The new file content for the changes (in the file's own encoding).
pub fn rewrite(raw: &[u8], changes: &[Change]) -> Result<Vec<u8>, &'static str> {
    let (b, enc) = decode(raw)?;
    let pairs: Vec<(&str, String)> = changes.iter().map(|c| (c.key, c.now.clone())).collect();
    Ok(encode(set_values(&b, &pairs), enc))
}

/// What happened to the file.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Already as the server asks: not written.
    Same,
    /// Written (or created: `created`).
    Written { created: bool, changes: Vec<Change> },
    /// No `FiveM.app` next to FiveM.exe: nothing created.
    NoAppDir,
    /// Could not read or write it (FiveM starts as before).
    Failed { why: String, changes: Vec<Change> },
}

/// `FiveM.app\CitizenFX.ini` next to FiveM.exe.
pub fn ini_path(fivem_exe: &Path) -> PathBuf {
    crate::support::app_dir(fivem_exe).join(INI)
}

fn io_why(e: &std::io::Error, path: &Path) -> String {
    let read_only = std::fs::metadata(path).map(|m| m.permissions().readonly()).unwrap_or(false);
    match e.kind() {
        _ if read_only => format!("файл только для чтения ({e})"),
        std::io::ErrorKind::PermissionDenied => format!("нет доступа ({e})"),
        _ => e.to_string(),
    }
}

/// Writes through a temporary file next to it and a rename: a half-written CitizenFX.ini would lose IVPath.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // a read-only CitizenFX.ini stays as it is (FiveM's own write fails on it too)
    if std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read-only"));
    }
    let tmp = path.with_extension("ini.coldz-tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Brings `FiveM.app\CitizenFX.ini` to what the server asks for. Call only while FiveM is not running.
pub fn sync(app_dir: &Path, w: &Wanted) -> Outcome {
    if !app_dir.is_dir() {
        return Outcome::NoAppDir;
    }
    let path = app_dir.join(INI);
    let (raw, created) = match std::fs::read(&path) {
        Ok(b) => (b, false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), true),
        Err(e) => return Outcome::Failed { why: format!("не прочитать: {}", io_why(&e, &path)), changes: Vec::new() },
    };
    let (text, _) = match decode(&raw) {
        Ok(d) => d,
        Err(why) => return Outcome::Failed { why: why.to_string(), changes: Vec::new() },
    };
    let changes = plan(&text, w);
    if changes.is_empty() {
        return Outcome::Same;
    }
    let new = match rewrite(&raw, &changes) {
        Ok(n) => n,
        Err(why) => return Outcome::Failed { why: why.to_string(), changes },
    };
    match write_whole(&path, &new) {
        Ok(()) => Outcome::Written { created, changes },
        Err(e) => Outcome::Failed { why: format!("не записать: {}", io_why(&e, &path)), changes },
    }
}

/// Log lines for the outcome (fivem-search.log).
pub fn log_lines(o: &Outcome) -> Vec<String> {
    let change = |c: &Change| format!("{}: {} → {}", c.key, c.was.as_deref().unwrap_or("(нет)"), if c.now.is_empty() { "(пусто)" } else { &c.now });
    match o {
        Outcome::Same => vec!["совпадает с сервером, файл не тронут".into()],
        Outcome::NoAppDir => vec!["нет папки FiveM.app рядом с FiveM.exe: файл не создаём".into()],
        Outcome::Written { created, changes } => {
            let mut v = vec![if *created { "файла не было: создан с [Game]".to_string() } else { "записано".to_string() }];
            v.extend(changes.iter().map(change));
            v
        }
        Outcome::Failed { why, changes } => {
            let mut v = vec![format!("ошибка: {why}; FiveM запускается как раньше")];
            v.extend(changes.iter().map(|c| format!("не записано {}", change(c))));
            v
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// What the server sends (20.11.2026) and what FiveM wrote at the owner's PC after joining.
    fn coldz_info() -> serde_json::Value {
        json!({ "vars": {
            "sv_enforceGameBuild": "3258",
            "sv_defaultGameBuild": "3889",
            "sv_pureLevel": "1",
            "sv_poolSizesIncrease": "{\"InteriorProxy\":450,\"PortalInst\":100}",
            "sv_replaceExeToSwitchBuilds": "false",
            "sv_projectName": "COLDZ"
        }})
    }
    const OWNER_INI: &[u8] = b"[Game]\r\nIVPath=E:\\SteamLibrary\\steamapps\\common\\Grand Theft Auto V\r\nSavedBuildNumber=3258\r\nPoolSizesIncrease={\"InteriorProxy\":450,\"PortalInst\":100}\r\nDefaultBuild=3889\r\nPureLevel=1\r\n";

    fn pools(p: &[(&str, u32)]) -> BTreeMap<String, u32> {
        p.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn coldz() -> Wanted {
        wanted(&coldz_info()).unwrap()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("coldz-gamebuild-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_what_the_server_asks_for() {
        let w = coldz();
        assert_eq!(w.build, 3258);
        assert_eq!(w.default_build, Some(3889));
        assert_eq!(w.pure, Some(1));
        assert_eq!(w.pools, Some(pools(&[("InteriorProxy", 450), ("PortalInst", 100)])));
        assert_eq!(w.describe(), "сборка 3258, exe 3889, pure 1, пулы {\"InteriorProxy\":450,\"PortalInst\":100}");
        // soon 3889
        let w = wanted(&json!({ "vars": { "sv_enforceGameBuild": "3889", "sv_defaultGameBuild": "3889" } })).unwrap();
        assert_eq!((w.build, w.default_build, w.pure, w.pools), (3889, Some(3889), Some(0), Some(BTreeMap::new())));
        // the old number FiveM remaps
        assert_eq!(wanted(&json!({ "vars": { "sv_enforceGameBuild": "3717" } })).unwrap().build, 3751);
        // numbers instead of strings, spaces
        let w = wanted(&json!({ "vars": { "sv_enforceGameBuild": 3258, "sv_pureLevel": " 2 " } })).unwrap();
        assert_eq!((w.build, w.pure), (3258, Some(2)));
    }

    #[test]
    fn exe_build_like_fivem() {
        let w = |vars: serde_json::Value| wanted(&json!({ "vars": vars })).unwrap().default_build;
        assert_eq!(w(json!({ "sv_enforceGameBuild": "3258", "sv_defaultGameBuild": "3889" })), Some(3889));
        // below FiveM's own default: FiveM ignores it; an old server replaces the exe unless it says "false"
        assert_eq!(w(json!({ "sv_enforceGameBuild": "3570", "sv_defaultGameBuild": "2944" })), Some(3570));
        assert_eq!(w(json!({ "sv_enforceGameBuild": "3570" })), Some(3570));
        assert_eq!(w(json!({ "sv_enforceGameBuild": "3570", "sv_replaceExeToSwitchBuilds": "false" })), None);
        assert_eq!(w(json!({ "sv_enforceGameBuild": "1" })), None);
    }

    #[test]
    fn bad_or_missing_vars_touch_nothing() {
        assert!(wanted(&json!({})).is_err());
        assert!(wanted(&json!({ "vars": [] })).is_err());
        assert!(wanted(&json!({ "vars": {} })).is_err());
        assert!(wanted(&json!({ "vars": { "sv_enforceGameBuild": "" } })).is_err());
        assert!(wanted(&json!({ "vars": { "sv_enforceGameBuild": "mpchristmas3" } })).is_err());
        assert!(wanted(&json!({ "vars": { "sv_enforceGameBuild": "0" } })).is_err());
        // a strange pure level or pools: that key alone is left alone
        let w = wanted(&json!({ "vars": { "sv_enforceGameBuild": "3258", "sv_pureLevel": "7", "sv_poolSizesIncrease": "{oops" } })).unwrap();
        assert_eq!((w.pure, w.pools), (None, None));
        let w = wanted(&json!({ "vars": { "sv_enforceGameBuild": "3258", "sv_poolSizesIncrease": "{\"A\":-1}" } })).unwrap();
        assert_eq!(w.pools, None);
        // pools as an object
        let w = wanted(&json!({ "vars": { "sv_enforceGameBuild": "3258", "sv_poolSizesIncrease": { "PortalInst": 100 } } })).unwrap();
        assert_eq!(w.pools, Some(pools(&[("PortalInst", 100)])));
    }

    #[test]
    fn owner_file_already_matches() {
        assert!(plan(OWNER_INI, &coldz()).is_empty());
        // the same with other spellings FiveM reads the same way
        let ini = b"[game]\r\nivpath=D:\\GTA\r\nsavedbuildnumber = 3258\r\npoolsizesincrease={\"PortalInst\":100,\"InteriorProxy\":450}\r\nDefaultBuild=3889\r\nPureLevel=1\r\n";
        assert!(plan(ini, &coldz()).is_empty());
    }

    #[test]
    fn other_build_is_rewritten_and_the_rest_stays() {
        // the player of 20.11 after `FiveM.exe -b3889`
        let ini = b"[Game]\r\nIVPath=E:\\SteamLibrary\\steamapps\\common\\Grand Theft Auto V\r\nSavedBuildNumber=3889\r\nUpdateChannel=production\r\n";
        let ch = plan(ini, &coldz());
        let keys: Vec<_> = ch.iter().map(|c| (c.key, c.was.clone(), c.now.as_str())).collect();
        assert_eq!(
            keys,
            [
                (KEY_BUILD, Some("3889".to_string()), "3258"),
                (KEY_DEFAULT, None, "3889"),
                (KEY_PURE, None, "1"),
                (KEY_POOLS, None, "{\"InteriorProxy\":450,\"PortalInst\":100}"),
            ]
        );
        let out = rewrite(ini, &ch).unwrap();
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            "[Game]\r\nIVPath=E:\\SteamLibrary\\steamapps\\common\\Grand Theft Auto V\r\nSavedBuildNumber=3258\r\nUpdateChannel=production\r\nDefaultBuild=3889\r\nPureLevel=1\r\nPoolSizesIncrease={\"InteriorProxy\":450,\"PortalInst\":100}\r\n"
        );
        // and then it matches
        assert!(plan(&out, &coldz()).is_empty());
    }

    #[test]
    fn ivpath_bytes_in_the_ansi_code_page_stay() {
        // «C:\Игры\GTA V» in cp1251 is not UTF-8: the bytes must come back the same
        let mut ini = b"[Game]\r\nIVPath=C:\\".to_vec();
        ini.extend_from_slice(&[0xC8, 0xE3, 0xF0, 0xFB]);
        ini.extend_from_slice(b"\\GTA V\r\nSavedBuildNumber=3889\r\n");
        let out = rewrite(&ini, &plan(&ini, &coldz())).unwrap();
        assert!(out.starts_with(&ini[..ini.len() - b"SavedBuildNumber=3889\r\n".len()]));
        assert!(out.windows(4).any(|w| w == [0xC8, 0xE3, 0xF0, 0xFB]));
        assert_eq!(value(&out, KEY_BUILD).as_deref(), Some("3258"));
    }

    #[test]
    fn line_ends_follow_the_file() {
        // LF only: new lines with LF
        let ini = b"[Game]\nIVPath=D:\\GTA\nSavedBuildNumber=3889\n";
        let out = String::from_utf8(rewrite(ini, &plan(ini, &coldz())).unwrap()).unwrap();
        assert!(!out.contains('\r'));
        assert!(out.starts_with("[Game]\nIVPath=D:\\GTA\nSavedBuildNumber=3258\nDefaultBuild=3889\n"));
        // no line break at the end of the last line
        let ini = b"[Game]\r\nIVPath=D:\\GTA";
        let out = String::from_utf8(rewrite(ini, &plan(ini, &coldz())).unwrap()).unwrap();
        assert!(out.starts_with("[Game]\r\nIVPath=D:\\GTA\r\nSavedBuildNumber=3258\r\n"), "{out:?}");
        assert!(!out.contains("\n\n") && !out.contains("\r\r"));
    }

    #[test]
    fn other_sections_and_blank_lines() {
        // [Game] is not the only section; keys of the same name elsewhere are not ours; a blank line closes [Game]
        let ini = b"; FiveM\r\n[Game]\r\nIVPath=D:\\GTA\r\n\r\n[Other]\r\nSavedBuildNumber=1604\r\n";
        let out = String::from_utf8(rewrite(ini, &plan(ini, &coldz())).unwrap()).unwrap();
        assert_eq!(
            out,
            "; FiveM\r\n[Game]\r\nIVPath=D:\\GTA\r\nSavedBuildNumber=3258\r\nDefaultBuild=3889\r\nPureLevel=1\r\nPoolSizesIncrease={\"InteriorProxy\":450,\"PortalInst\":100}\r\n\r\n[Other]\r\nSavedBuildNumber=1604\r\n"
        );
        // no [Game] at all: appended
        let ini = b"[Other]\r\nA=1";
        let out = String::from_utf8(rewrite(ini, &plan(ini, &coldz())).unwrap()).unwrap();
        assert!(out.starts_with("[Other]\r\nA=1\r\n[Game]\r\nSavedBuildNumber=3258\r\n"), "{out:?}");
        assert!(plan(out.as_bytes(), &coldz()).is_empty());
        // an empty file
        let out = String::from_utf8(rewrite(b"", &plan(b"", &coldz())).unwrap()).unwrap();
        assert_eq!(out, "[Game]\r\nSavedBuildNumber=3258\r\nDefaultBuild=3889\r\nPureLevel=1\r\nPoolSizesIncrease={\"InteriorProxy\":450,\"PortalInst\":100}\r\n");
    }

    #[test]
    fn pure_and_pools_like_fivem() {
        let w = Wanted { build: 3258, default_build: None, pure: Some(0), pools: Some(BTreeMap::new()) };
        // no keys = pure 0 and no pools: nothing but the build
        assert_eq!(plan(b"[Game]\r\nSavedBuildNumber=3258\r\n", &w), []);
        // pure 3 is not used by FiveM (= 0); a broken pools value FiveM clears itself (= none)
        assert_eq!(plan(b"[Game]\r\nSavedBuildNumber=3258\r\nPureLevel=3\r\nPoolSizesIncrease={oops\r\n", &w), []);
        // pure 1 for a server without pure mode, pools from another server: cleared
        let ch = plan(b"[Game]\r\nSavedBuildNumber=3258\r\nPureLevel=1\r\nPoolSizesIncrease={\"A\":1}\r\n", &w);
        assert_eq!(ch.iter().map(|c| (c.key, c.now.as_str())).collect::<Vec<_>>(), [(KEY_PURE, "0"), (KEY_POOLS, "")]);
        let out = rewrite(b"[Game]\r\nSavedBuildNumber=3258\r\nPureLevel=1\r\nPoolSizesIncrease={\"A\":1}\r\n", &ch).unwrap();
        assert_eq!(out, b"[Game]\r\nSavedBuildNumber=3258\r\nPureLevel=0\r\nPoolSizesIncrease=\r\n");
        // a missing build key is written even when it is FiveM's default today
        assert_eq!(plan(b"[Game]\r\nIVPath=D:\\GTA\r\n", &w).len(), 1);
    }

    #[test]
    fn unicode_file_stays_unicode() {
        let text = "[Game]\r\nIVPath=C:\\Users\\Юрка\\GTA V\r\nSavedBuildNumber=3889\r\n";
        let mut raw = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            raw.extend_from_slice(&u.to_le_bytes());
        }
        let (dec, _) = decode(&raw).unwrap();
        let out = rewrite(&raw, &plan(&dec, &coldz())).unwrap();
        assert_eq!(&out[..2], [0xFF, 0xFE]);
        let units: Vec<u16> = out[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let back = String::from_utf16(&units).unwrap();
        assert!(back.starts_with("[Game]\r\nIVPath=C:\\Users\\Юрка\\GTA V\r\nSavedBuildNumber=3258\r\n"), "{back:?}");
        assert!(decode(b"\xFE\xFF\x00[").is_err());
    }

    #[test]
    fn file_on_disk() {
        let w = coldz();
        // no FiveM.app: nothing created
        let d = tmp("noapp");
        assert_eq!(sync(&d.join("FiveM.app"), &w), Outcome::NoAppDir);
        assert!(!d.join("FiveM.app").exists());
        let _ = std::fs::remove_dir_all(&d);

        // FiveM.app without CitizenFX.ini: created
        let d = tmp("create");
        let app = d.join("FiveM.app");
        std::fs::create_dir_all(&app).unwrap();
        let o = sync(&app, &w);
        assert!(matches!(&o, Outcome::Written { created: true, changes } if changes.len() == 4), "{o:?}");
        assert!(plan(&std::fs::read(app.join(INI)).unwrap(), &w).is_empty());
        assert!(log_lines(&o)[0].contains("создан"));
        // the second time: the same, not written
        let before = std::fs::metadata(app.join(INI)).unwrap().modified().unwrap();
        assert_eq!(sync(&app, &w), Outcome::Same);
        assert_eq!(std::fs::metadata(app.join(INI)).unwrap().modified().unwrap(), before);
        assert_eq!(log_lines(&Outcome::Same), ["совпадает с сервером, файл не тронут"]);
        let _ = std::fs::remove_dir_all(&d);

        // the owner's file: not written; another build: only those lines, IVPath stays, no temporary file left
        let d = tmp("rewrite");
        let app = d.join("FiveM.app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join(INI), OWNER_INI).unwrap();
        assert_eq!(sync(&app, &w), Outcome::Same);
        let old = b"[Game]\r\nIVPath=E:\\SteamLibrary\\steamapps\\common\\Grand Theft Auto V\r\nSavedBuildNumber=3889\r\nPoolSizesIncrease={\"InteriorProxy\":450,\"PortalInst\":100}\r\nDefaultBuild=3889\r\nPureLevel=1\r\n";
        std::fs::write(app.join(INI), old).unwrap();
        let o = sync(&app, &w);
        assert_eq!(o, Outcome::Written { created: false, changes: vec![Change { key: KEY_BUILD, was: Some("3889".into()), now: "3258".into() }] });
        assert_eq!(std::fs::read(app.join(INI)).unwrap(), OWNER_INI);
        assert_eq!(std::fs::read_dir(&app).unwrap().count(), 1);
        assert_eq!(log_lines(&o), ["записано", "SavedBuildNumber: 3889 → 3258"]);

        // read-only: left as it is, the error is said
        std::fs::write(app.join(INI), old).unwrap();
        let mut perm = std::fs::metadata(app.join(INI)).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(app.join(INI), perm.clone()).unwrap();
        let o = sync(&app, &w);
        assert!(matches!(&o, Outcome::Failed { why, changes } if why.contains("только для чтения") && changes.len() == 1), "{o:?}");
        assert_eq!(std::fs::read(app.join(INI)).unwrap(), old);
        assert_eq!(std::fs::read_dir(&app).unwrap().count(), 1);
        assert!(log_lines(&o)[0].starts_with("ошибка: не записать: файл только для чтения"));
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(app.join(INI), perm).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ini_next_to_fivem_exe() {
        assert_eq!(
            ini_path(Path::new(r"D:\Games\FiveM\FiveM.exe")),
            Path::new(r"D:\Games\FiveM").join("FiveM.app").join("CitizenFX.ini")
        );
    }
}
