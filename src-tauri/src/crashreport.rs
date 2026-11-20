//! 0.5.6: reports about game crashes to the developers. Alexander 21.11: «Давай, хорошая идея. Только смотри, лаунчер
//! не загаси — качественно сделай. Будем с тех вылетов собирать обратную связь и чинить.»
//!
//! What happens (README 7п):
//!  * `scan` (only reads): FiveM's crash dumps `FiveM.app\crashes\<id>.dmp` and FiveM's own crash packs
//!    `CfxCrashDump_*.zip` (the player saved one from FiveM's crash window: in `crashes`, on the desktop, in Downloads)
//!    of the last 3 days, newer than the crash the player last answered about (settings.json `crashReport.seen`).
//!  * the window asks under «Играть» (which stays enabled): «Игра закрылась с ошибкой <hash> (<дата>). Отправить отчёт
//!    разработчикам?» [Отправить] [Не отправлять] [Не спрашивать]. «Отправить» opens a card with what goes; only its
//!    own «Отправить» sends.
//!  * `prepare` / `pack`: a zip with `report.json` (launcher, Windows, video cards and drivers, the crash),
//!    `crashometry.json` (only from FiveM's crash pack), `CitizenFX_log_tail.txt` (up to 300 lines of the FiveM log of
//!    that session, ending with the crash), `modules.txt` (the exception, modules with versions, a scan of the crashed
//!    thread's stack: read from the dump by the launcher, no memory contents). The dump itself only with its own tick.
//!    Texts are cleaned (`clean`): the Windows user name → `%USER%`, the PC name → `%PC%`, IPv4/IPv6 addresses (but our
//!    server and relays), e-mails, tokens and keys, account ids (Steam, Discord, license...); the Rockstar ID becomes
//!    `rid:<hash>` with a salt that never leaves the PC. At most 2 MB without the dump, 20 MB with it.
//!  * `send`: one POST to our receiver (winter-server `tools/crashreports`) with a fixed client key (no secret: it only
//!    keeps random traffic out). A report that did not go out waits in `crash-outbox\` and goes at the next starts
//!    (3 tries, 7 days); no resuming of a broken upload. Errors only go to `crash-report.log`.
//!  * switches: `launcher.config.json → crashReport` (`enabled`, `url`, `key`, `dump`); `crashReport` in the signed
//!    news.json wins: `{"enabled": false}` switches everything off without a release, `{"dump": false}` only the dump.
//!
//! The launcher is built with `panic = "abort"`: a panic closes it. So everything read from files (dumps, zips, logs)
//! goes through `get`, checked arithmetic and limits, never `unwrap` or indexing (tests feed broken and cut files).

use crate::support::{self, FileInfo};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A crash this fresh is asked about (the 0.5.0 crash hint uses the same 3 days).
pub const FRESH_SECS: u64 = 3 * 24 * 3600;
/// A dump younger than this may still be written by FiveM: the next look takes it.
pub const SETTLE_SECS: u64 = 20;
/// Lines of the FiveM log in the report.
pub const LOG_LINES: usize = 300;
/// The FiveM log is read from its end, at most this much (logs reach tens of MB).
pub const LOG_READ: u64 = 4 * 1024 * 1024;
/// A longer log line is cut (NUI messages can be huge).
pub const LINE_MAX: usize = 1000;
/// The whole report without the dump.
pub const LIMIT_SMALL: usize = 2 * 1024 * 1024;
/// The whole report with the dump.
pub const LIMIT_DUMP: usize = 20 * 1024 * 1024;
/// A dump bigger than this is not read at all (FiveM's are 2-20 MB).
pub const DUMP_READ_MAX: u64 = 512 * 1024 * 1024;
/// A crash pack bigger than this is not opened.
pub const PACK_READ_MAX: u64 = 256 * 1024 * 1024;
/// Reports waiting in the outbox at most; older ones go first.
pub const OUTBOX_MAX: usize = 3;
pub const OUTBOX_SECS: u64 = 7 * 24 * 3600;
pub const OUTBOX_TRIES: u32 = 3;
/// Entries of the stack scan in modules.txt.
pub const STACK_MAX: usize = 40;
/// `report.json` → `format`; the receiver checks it.
pub const FORMAT: u32 = 1;
/// Names inside the report zip (the receiver accepts only these and one `<id>.dmp`).
pub const NAME_REPORT: &str = "report.json";
pub const NAME_CRASHOMETRY: &str = "crashometry.json";
pub const NAME_LOG: &str = "CitizenFX_log_tail.txt";
pub const NAME_MODULES: &str = "modules.txt";

// ---------------------------------------------------------------- switches: news.json wins

/// `launcher.config.json → crashReport`. Missing: off.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Switch {
    pub enabled: bool,
    /// The receiver: `https://…`.
    pub url: String,
    /// The client key (not a secret: it is in the launcher), header `X-Coldz-Key`.
    pub key: String,
    /// The dump may go with its tick.
    pub dump: bool,
}

/// `crashReport` of news.json (lib.rs `apply_feed`): each key on its own; a bad value = launcher.config.json.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feed {
    pub enabled: Option<bool>,
    pub url: Option<String>,
    pub key: Option<String>,
    pub dump: Option<bool>,
}

impl Feed {
    pub fn of(v: &serde_json::Value) -> Feed {
        let Some(c) = v.get("crashReport").filter(|c| c.is_object()) else {
            return Feed::default();
        };
        Feed {
            enabled: c.get("enabled").and_then(|x| x.as_bool()),
            url: c.get("url").and_then(|u| u.as_str()).map(str::trim).filter(|u| valid_url(u)).map(str::to_string),
            key: c.get("key").and_then(|k| k.as_str()).map(str::trim).filter(|k| valid_key(k)).map(str::to_string),
            dump: c.get("dump").and_then(|x| x.as_bool()),
        }
    }
}

pub fn valid_url(u: &str) -> bool {
    crate::firstjoin::valid_url(u)
}

pub fn valid_key(k: &str) -> bool {
    (8..=128).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// What is in use.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Setup {
    pub enabled: bool,
    pub url: String,
    pub key: String,
    pub dump: bool,
}

/// news.json wins over launcher.config.json; on only with a good https address and key.
pub fn setup(feed: &Feed, built_in: &Switch) -> Setup {
    let url = feed.url.clone().unwrap_or_else(|| built_in.url.trim().to_string());
    let key = feed.key.clone().unwrap_or_else(|| built_in.key.trim().to_string());
    let enabled = feed.enabled.unwrap_or(built_in.enabled) && valid_url(&url) && valid_key(&key);
    Setup { enabled, url, key, dump: feed.dump.unwrap_or(built_in.dump) }
}

// ---------------------------------------------------------------- what the launcher remembers (settings.json)

/// settings.json → `crashReport`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    /// Unix seconds of the newest crash the player answered about (sent or «Не отправлять»): it and older ones are not
    /// asked about again.
    pub seen: u64,
    /// «Не спрашивать»: no question any more (the settings card turns it back on).
    pub never: bool,
    /// Random, made once; never leaves the PC. The Rockstar ID and the reporter id are hashed with it.
    pub salt: String,
    /// Reports that did not go out yet (files in `crash-outbox\`).
    pub outbox: Vec<Pending>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Pending {
    /// The file name in `crash-outbox\`.
    pub file: String,
    /// Unix seconds when it was made.
    pub at: u64,
    pub tries: u32,
    pub hash: String,
    pub dump: bool,
}

/// What to do with the outbox now: send these (oldest first), forget these (too old, too many tries, too many files).
pub fn outbox_plan(state: &State, now: u64) -> (Vec<Pending>, Vec<Pending>) {
    let mut list = state.outbox.clone();
    list.sort_by_key(|p| p.at);
    let mut send = Vec::new();
    let mut drop = Vec::new();
    let keep_from = list.len().saturating_sub(OUTBOX_MAX);
    for (i, p) in list.into_iter().enumerate() {
        let stale = now.saturating_sub(p.at) > OUTBOX_SECS || p.tries >= OUTBOX_TRIES || !safe_file_name(&p.file);
        if i < keep_from || stale {
            drop.push(p);
        } else {
            send.push(p);
        }
    }
    (send, drop)
}

/// Only our own names (`crash-<…>.zip`): nothing from settings.json can point outside the outbox.
pub fn safe_file_name(n: &str) -> bool {
    n.len() <= 120 && n.starts_with("crash-") && n.ends_with(".zip") && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

// ---------------------------------------------------------------- finding crashes

/// One crash of FiveM.
#[derive(Clone, Debug, PartialEq)]
pub struct Crash {
    /// The dump's name `<id>.dmp` (also when only FiveM's crash pack holds it).
    pub id: String,
    /// Unix seconds (the dump's or the pack's time).
    pub at: u64,
    /// `FiveM.app\crashes\<id>`.
    pub dump: Option<FileInfo>,
    /// `CfxCrashDump_*.zip` holding `<id>`.
    pub pack: Option<FileInfo>,
}

/// Crashes of the last `FRESH_SECS`, newest first. `crashes`: `FiveM.app\crashes`; `pack_dirs`: the desktop, Downloads.
pub fn scan(crashes: Option<&Path>, pack_dirs: &[PathBuf], now: u64) -> Vec<Crash> {
    let since = now.saturating_sub(FRESH_SECS);
    let settled = |f: &FileInfo| f.size > 0 && f.at >= since && f.at.saturating_add(SETTLE_SECS) <= now;
    let mut out: Vec<Crash> = Vec::new();
    if let Some(dir) = crashes {
        for d in support::files(dir, "", ".dmp").into_iter().filter(|f| settled(f) && dump_name(&f.name)) {
            out.push(Crash { id: d.name.clone(), at: d.at, dump: Some(d), pack: None });
        }
    }
    let mut dirs: Vec<&Path> = Vec::new();
    for d in crashes.into_iter().chain(pack_dirs.iter().map(PathBuf::as_path)) {
        if !dirs.iter().any(|x| support::same_dir(x, d)) {
            dirs.push(d);
        }
    }
    for dir in dirs {
        for p in support::files(dir, "CfxCrashDump", ".zip").into_iter().filter(|f| settled(f) && f.size <= PACK_READ_MAX).take(20) {
            let Some(entries) = FileAt::open(&p.path).and_then(|mut f| f.entries()) else { continue };
            let Some(id) = entries.iter().find_map(|e| Some(base_name(&e.name)).filter(|n| dump_name(n))) else { continue };
            match out.iter_mut().find(|c| c.id.eq_ignore_ascii_case(&id)) {
                Some(c) => {
                    if c.pack.is_none() {
                        c.pack = Some(p);
                    }
                }
                None => out.push(Crash { id, at: p.at, dump: None, pack: Some(p) }),
            }
        }
    }
    out.sort_by(|a, b| b.at.cmp(&a.at).then(b.id.cmp(&a.id)));
    out
}

/// `<letters, digits, -_.>.dmp`, short: FiveM's names are `<uuid>.dmp`.
pub fn dump_name(n: &str) -> bool {
    n.len() <= 84
        && n.len() > 4
        && n.to_ascii_lowercase().ends_with(".dmp")
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn base_name(n: &str) -> String {
    n.rsplit(['/', '\\']).next().unwrap_or(n).to_string()
}

/// The newest crash the player has not answered about, and how many such there are.
pub fn pending(crashes: &[Crash], state: &State) -> Option<(Crash, usize)> {
    if state.never {
        return None;
    }
    let fresh: Vec<&Crash> = crashes.iter().filter(|c| c.at > state.seen).collect();
    fresh.first().map(|c| ((*c).clone(), fresh.len()))
}

/// What the window asks about.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Ask {
    pub id: String,
    pub at: u64,
    /// FiveM's crash place (`libcef.dll+E0D01`), "" when unknown.
    pub hash: String,
    /// FiveM's three words (`lamp-india-queen`), only from FiveM's crash pack.
    pub key: String,
    /// New crashes in all.
    pub count: usize,
}

// ---------------------------------------------------------------- reading zips (FiveM's crash packs)

/// One entry of a zip's central directory.
#[derive(Clone, Debug, PartialEq)]
pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    pub flags: u16,
    pub crc: u32,
    pub csize: u64,
    pub usize: u64,
    pub offset: u64,
}

fn rd16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn rd32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn rd64(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}

fn at32(b: &[u8], at: usize) -> Option<usize> {
    rd32(b, at).map(|v| v as usize)
}

/// Reads parts of a file (a zip is opened from its end, the big dump inside is not read for the list).
pub struct FileAt {
    f: std::fs::File,
    pub len: u64,
}

impl FileAt {
    pub fn open(p: &Path) -> Option<FileAt> {
        let f = std::fs::File::open(p).ok()?;
        let len = f.metadata().ok()?.len();
        Some(FileAt { f, len })
    }

    pub fn read(&mut self, at: u64, n: usize) -> Option<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        if at.checked_add(n as u64)? > self.len || n as u64 > PACK_READ_MAX {
            return None;
        }
        self.f.seek(SeekFrom::Start(at)).ok()?;
        let mut buf = vec![0u8; n];
        self.f.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    pub fn entries(&mut self) -> Option<Vec<ZipEntry>> {
        let len = self.len;
        zip_entries(len, &mut |at, n| self.read(at, n))
    }

    /// The stored bytes of an entry (compressed as in the zip).
    pub fn raw(&mut self, e: &ZipEntry) -> Option<Vec<u8>> {
        let head = self.read(e.offset, 30)?;
        if rd32(&head, 0)? != 0x0403_4b50 {
            return None;
        }
        let skip = 30u64.checked_add(rd16(&head, 26)? as u64)?.checked_add(rd16(&head, 28)? as u64)?;
        self.read(e.offset.checked_add(skip)?, usize::try_from(e.csize).ok()?)
    }

    /// An entry unpacked (stored or deflate), at most `limit` bytes, CRC checked.
    pub fn unpack(&mut self, e: &ZipEntry, limit: usize) -> Option<Vec<u8>> {
        if e.usize > limit as u64 {
            return None;
        }
        let raw = self.raw(e)?;
        unpack(e, &raw, limit)
    }
}

/// The central directory: `read_at(offset, len)` reads the file. No zip64, at most 1000 entries.
pub fn zip_entries(len: u64, read_at: &mut dyn FnMut(u64, usize) -> Option<Vec<u8>>) -> Option<Vec<ZipEntry>> {
    let tail_len = len.min(65_557);
    let tail = read_at(len.checked_sub(tail_len)?, tail_len as usize)?;
    if tail.len() < 22 {
        return None;
    }
    let e = (0..=tail.len() - 22).rev().find(|&i| tail.get(i..i + 4) == Some(&[0x50, 0x4b, 0x05, 0x06][..]))?;
    let count = rd16(&tail, e + 10)? as usize;
    let cd_size = rd32(&tail, e + 12)? as u64;
    let cd_off = rd32(&tail, e + 16)? as u64;
    if count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF || count > 1000 || cd_size > 4 * 1024 * 1024 {
        return None;
    }
    if cd_off.checked_add(cd_size)? > len {
        return None;
    }
    let cd = read_at(cd_off, cd_size as usize)?;
    let mut out = Vec::new();
    let mut p = 0usize;
    for _ in 0..count {
        if rd32(&cd, p)? != 0x0201_4b50 {
            return None;
        }
        let nlen = rd16(&cd, p + 28)? as usize;
        let xlen = rd16(&cd, p + 30)? as usize;
        let clen = rd16(&cd, p + 32)? as usize;
        let name = cd.get(p + 46..(p + 46).checked_add(nlen)?)?;
        out.push(ZipEntry {
            name: String::from_utf8_lossy(name).into_owned(),
            flags: rd16(&cd, p + 8)?,
            method: rd16(&cd, p + 10)?,
            crc: rd32(&cd, p + 16)?,
            csize: rd32(&cd, p + 20)? as u64,
            usize: rd32(&cd, p + 24)? as u64,
            offset: rd32(&cd, p + 42)? as u64,
        });
        p = p.checked_add(46)?.checked_add(nlen)?.checked_add(xlen)?.checked_add(clen)?;
    }
    Some(out)
}

/// Stored or deflate bytes of an entry → the file, at most `limit` bytes, the CRC must match.
pub fn unpack(e: &ZipEntry, raw: &[u8], limit: usize) -> Option<Vec<u8>> {
    if e.flags & 1 != 0 || e.usize > limit as u64 {
        return None;
    }
    let data = match e.method {
        0 => raw.to_vec(),
        8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, limit).ok()?,
        _ => return None,
    };
    (data.len() as u64 == e.usize && support::crc32(&data) == e.crc).then_some(data)
}

// ---------------------------------------------------------------- writing the report zip

/// One file of the report zip: deflated when that is smaller.
#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub method: u16,
    pub crc: u32,
    pub raw_len: u64,
    pub data: Vec<u8>,
}

impl Part {
    pub fn pack(name: &str, raw: &[u8]) -> Part {
        let crc = support::crc32(raw);
        let z = miniz_oxide::deflate::compress_to_vec(raw, 6);
        if z.len() < raw.len() {
            Part { name: name.to_string(), method: 8, crc, raw_len: raw.len() as u64, data: z }
        } else {
            Part { name: name.to_string(), method: 0, crc, raw_len: raw.len() as u64, data: raw.to_vec() }
        }
    }

    pub fn len(&self) -> usize {
        self.data.len() + self.name.len() * 2 + 30 + 46
    }
}

/// The zip bytes (names in UTF-8, a fixed date: the real times are in report.json).
pub fn zip(parts: &[Part]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let put16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let put32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    let (time, date) = (0u16, ((2026 - 1980) << 9 | 1 << 5 | 1) as u16);
    for p in parts {
        let offset = out.len() as u32;
        let n = p.name.as_bytes();
        put32(&mut out, 0x0403_4b50);
        put16(&mut out, 20);
        put16(&mut out, 0x0800);
        put16(&mut out, p.method);
        put16(&mut out, time);
        put16(&mut out, date);
        put32(&mut out, p.crc);
        put32(&mut out, p.data.len() as u32);
        put32(&mut out, p.raw_len as u32);
        put16(&mut out, n.len() as u16);
        put16(&mut out, 0);
        out.extend_from_slice(n);
        out.extend_from_slice(&p.data);

        put32(&mut central, 0x0201_4b50);
        put16(&mut central, 20);
        put16(&mut central, 20);
        put16(&mut central, 0x0800);
        put16(&mut central, p.method);
        put16(&mut central, time);
        put16(&mut central, date);
        put32(&mut central, p.crc);
        put32(&mut central, p.data.len() as u32);
        put32(&mut central, p.raw_len as u32);
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
    put16(&mut out, parts.len() as u16);
    put16(&mut out, parts.len() as u16);
    put32(&mut out, cd_len);
    put32(&mut out, cd_offset);
    put16(&mut out, 0);
    out
}

// ---------------------------------------------------------------- the dump: modules, exception, stack (no memory)

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Module {
    pub base: u64,
    pub size: u32,
    /// The full path as Windows loaded it (cleaned before it goes anywhere).
    pub path: String,
    pub version: String,
    /// The PE time stamp.
    pub stamp: u32,
}

impl Module {
    pub fn name(&self) -> &str {
        self.path.rsplit(['\\', '/']).next().unwrap_or(&self.path)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DumpInfo {
    /// The dump's time (unix seconds).
    pub time: u32,
    /// In the dump's order: the first one is the process's exe.
    pub modules: Vec<Module>,
    pub thread: Option<u32>,
    pub code: Option<u32>,
    pub address: Option<u64>,
    /// `module+OFFSET` words from the crashed thread's stack (a scan: return addresses and other pointers into code).
    pub stack: Vec<String>,
    /// «Windows 10.0.26100».
    pub windows: Option<String>,
    pub cpus: Option<u8>,
    /// Seconds the process ran before the crash.
    pub uptime: Option<u32>,
}

/// `name+OFFSET` the way FiveM writes crash places.
pub fn place(modules: &[Module], addr: u64) -> Option<String> {
    let m = modules.iter().find(|m| addr >= m.base && addr < m.base.saturating_add(m.size as u64))?;
    Some(format!("{}+{:X}", m.name(), addr - m.base))
}

fn md_string(d: &[u8], rva: usize) -> Option<String> {
    let len = at32(d, rva)?.min(4096);
    let bytes = d.get(rva.checked_add(4)?..rva.checked_add(4)?.checked_add(len)?)?;
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(String::from_utf16_lossy(&units))
}

/// Reads a minidump. None: not a minidump. Anything broken inside is skipped, never a panic.
pub fn read_dump(d: &[u8]) -> Option<DumpInfo> {
    if rd32(d, 0)? != 0x504d_444d {
        return None;
    }
    let mut info = DumpInfo { time: rd32(d, 20).unwrap_or(0), ..Default::default() };
    let n = rd32(d, 8)?.min(512) as usize;
    let dir = at32(d, 12)?;
    let mut streams: Vec<(u32, usize, usize)> = Vec::new();
    for i in 0..n {
        let Some(at) = i.checked_mul(12).and_then(|o| dir.checked_add(o)) else { break };
        let (Some(t), Some(size), Some(rva)) = (rd32(d, at), at32(d, at + 4), at32(d, at + 8)) else { break };
        if !streams.iter().any(|s| s.0 == t) {
            streams.push((t, size, rva));
        }
    }
    let stream = |t: u32| streams.iter().find(|s| s.0 == t).map(|s| s.2);
    // modules (ModuleListStream = 4)
    if let Some(rva) = stream(4) {
        let count = rd32(d, rva).unwrap_or(0).min(4096) as usize;
        for j in 0..count {
            let Some(o) = j.checked_mul(108).and_then(|x| x.checked_add(rva)).and_then(|x| x.checked_add(4)) else { break };
            let read = || -> Option<Module> {
                let ms = rd32(d, o + 24 + 8)?;
                let ls = rd32(d, o + 24 + 12)?;
                Some(Module {
                    base: rd64(d, o)?,
                    size: rd32(d, o + 8)?,
                    stamp: rd32(d, o + 16)?,
                    path: md_string(d, at32(d, o + 20)?)?,
                    version: format!("{}.{}.{}.{}", ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff),
                })
            };
            match read() {
                Some(m) => info.modules.push(m),
                None => break,
            }
        }
    }
    let mut sorted: Vec<Module> = info.modules.clone();
    sorted.sort_by_key(|m| m.base);
    let lookup = |v: u64| -> Option<String> {
        let i = sorted.partition_point(|m| m.base <= v);
        let m = sorted.get(i.checked_sub(1)?)?;
        (v < m.base.saturating_add(m.size as u64) && v > m.base).then(|| format!("{}+{:X}", m.name(), v - m.base))
    };
    // the exception (ExceptionStream = 6) and the crashed thread's registers
    let mut rsp: Option<u64> = None;
    if let Some(rva) = stream(6) {
        info.thread = rd32(d, rva);
        info.code = rd32(d, rva + 8);
        info.address = rd64(d, rva + 24);
        if let (Some(size), Some(ctx)) = (rd32(d, rva + 160), at32(d, rva + 164)) {
            if size >= 0x100 {
                rsp = rd64(d, ctx + 0x98);
            }
        }
    }
    // a scan of the crashed thread's stack (ThreadListStream = 3), like tools md_stack.py
    if let (Some(rva), Some(tid)) = (stream(3), info.thread) {
        if let Some(a) = info.address.and_then(|a| lookup(a)) {
            info.stack.push(a);
        }
        let count = rd32(d, rva).unwrap_or(0).min(20_000) as usize;
        for k in 0..count {
            let Some(o) = k.checked_mul(48).and_then(|x| x.checked_add(rva)).and_then(|x| x.checked_add(4)) else { break };
            if rd32(d, o) != Some(tid) {
                continue;
            }
            let (Some(start), Some(size), Some(srva)) = (rd64(d, o + 24), at32(d, o + 32), at32(d, o + 36)) else { break };
            let Some(mem) = d.get(srva..srva.saturating_add(size)) else { break };
            let lo = rsp.filter(|r| *r >= start).map_or(0, |r| r - start);
            let lo = usize::try_from(lo).unwrap_or(0);
            let lo = if lo < mem.len() { lo - lo % 8 } else { 0 };
            let end = mem.len().min(lo.saturating_add(512 * 1024));
            let mut q = lo;
            while q + 8 <= end && info.stack.len() < STACK_MAX {
                if let Some(p) = rd64(mem, q).and_then(|v| lookup(v)) {
                    if info.stack.last() != Some(&p) {
                        info.stack.push(p);
                    }
                }
                q += 8;
            }
            break;
        }
    }
    // SystemInfoStream = 7
    if let Some(rva) = stream(7) {
        info.cpus = d.get(rva + 6).copied();
        if let (Some(ma), Some(mi), Some(b)) = (rd32(d, rva + 8), rd32(d, rva + 12), rd32(d, rva + 16)) {
            info.windows = Some(format!("Windows {ma}.{mi}.{b}"));
        }
    }
    // MiscInfoStream = 15: the process start (flag MINIDUMP_MISC1_PROCESS_TIMES)
    if let Some(rva) = stream(15) {
        if rd32(d, rva).unwrap_or(0) >= 24 && rd32(d, rva + 4).unwrap_or(0) & 2 != 0 {
            info.uptime = rd32(d, rva + 12).and_then(|c| info.time.checked_sub(c));
        }
    }
    Some(info)
}

/// «access violation» for the codes seen most.
pub fn code_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "access violation",
        0xC000_0409 => "stack buffer overrun (fail fast)",
        0xC000_00FD => "stack overflow",
        0xC000_0374 => "heap corruption",
        0xC000_001D => "illegal instruction",
        0xC000_0094 => "integer divide by zero",
        0x8000_0003 => "breakpoint",
        0xE06D_7363 => "C++ exception",
        _ => "",
    }
}

/// The game build from the process exe (`FiveM_b3889_GTAProcess.exe`, `GTA5_b3258.exe`).
pub fn game_build(exe: &str) -> Option<u32> {
    let low = exe.to_ascii_lowercase();
    let i = low.find("_b")?;
    let digits: String = low.get(i + 2..)?.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|b| *b >= 1000)
}

/// User-mode video drivers: their version is the driver's version.
pub const DRIVER_DLLS: [&str; 11] = [
    "nvwgf2umx.dll",
    "nvldumdx.dll",
    "nvoglv64.dll",
    "amdxx64.dll",
    "atidxx64.dll",
    "amdxc64.dll",
    "aticfx64.dll",
    "igd10iumd64.dll",
    "igdumdim64.dll",
    "igd12umd64.dll",
    "igc64.dll",
];

/// `modules.txt`: what the dump says, without the memory. `fix` cleans the names and paths of the modules; versions,
/// offsets and dates are left as they are (a version like `31.0.21.1` looks like an IPv4 address to `clean`).
pub fn modules_text(info: &DumpInfo, dump: &str, size: u64, fix: &dyn Fn(&str) -> String) -> String {
    let mut t = String::new();
    t.push_str("COLDZ crash modules v1\n");
    t.push_str(&format!("dump: {dump}, {size} bytes, {}\n", crate::fivem::utc_stamp(info.time as u64)));
    if let Some(code) = info.code {
        let at = info.address.and_then(|a| place(&info.modules, a)).unwrap_or_else(|| info.address.map_or("?".into(), |a| format!("0x{a:X}")));
        t.push_str(&format!("exception: code 0x{code:08X} {}, at {at}, thread {}\n", code_name(code), info.thread.unwrap_or(0)));
    }
    if let Some(m) = info.modules.first() {
        let build = game_build(m.name()).map_or(String::new(), |b| format!(", game build {b}"));
        let up = info.uptime.map_or(String::new(), |u| format!(", ran {u} s"));
        t.push_str(&format!("process: {} {}{build}{up}\n", m.name(), m.version));
    }
    if let Some(w) = &info.windows {
        t.push_str(&format!("system: {w}, {} CPU\n", info.cpus.unwrap_or(0)));
    }
    for m in info.modules.iter().filter(|m| DRIVER_DLLS.contains(&m.name().to_ascii_lowercase().as_str())) {
        t.push_str(&format!("video driver: {} {}\n", m.name(), m.version));
    }
    t.push_str(&format!("stack (scan of the crashed thread, not exact): {}\n", info.stack.len()));
    for s in &info.stack {
        t.push_str(&format!("  {s}\n"));
    }
    t.push_str(&format!("modules: {}\n", info.modules.len()));
    for m in &info.modules {
        let date = if m.stamp > 0 { crate::fivem::utc_stamp(m.stamp as u64).chars().take(10).collect::<String>() } else { "-".into() };
        t.push_str(&format!("  {:016X} {:08X} {:<16} {} {:<36} {}\n", m.base, m.size, m.version, date, fix(m.name()), fix(&m.path)));
    }
    t
}

// ---------------------------------------------------------------- the FiveM log of the crashed session

/// `CitizenFX_log_2026-11-20T175337.log` → its start (UTC, unix seconds).
pub fn log_time(name: &str) -> Option<u64> {
    let rest = name.get("CitizenFX_log_".len()..)?;
    if !name.get(.."CitizenFX_log_".len())?.eq_ignore_ascii_case("CitizenFX_log_") {
        return None;
    }
    let b = rest.as_bytes();
    let num = |a: usize, n: usize| -> Option<i64> { std::str::from_utf8(b.get(a..a + n)?).ok()?.parse::<i64>().ok() };
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || b.get(10) != Some(&b'T') {
        return None;
    }
    let (y, m, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hh, mm, ss) = (num(11, 2)?, num(13, 2)?, num(15, 2)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // days from the civil date (Howard Hinnant), the inverse of fivem::utc_stamp
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

/// The log of the session that crashed: started before the crash, still written near it; the newest such. (FiveM
/// restarted right after a crash starts a new log within a minute: it must not be taken, 20.11 on the owner's PC.)
pub fn find_log(logs: &Path, crash_at: u64) -> Option<FileInfo> {
    support::files(logs, "CitizenFX_log_", ".log")
        .into_iter()
        .filter_map(|f| log_time(&f.name).map(|t| (t, f)))
        .filter(|(t, f)| *t <= crash_at && f.at.saturating_add(120) >= crash_at)
        .max_by_key(|(t, _)| *t)
        .map(|(_, f)| f)
}

/// The end of the log for the report: lines up to the crash, FiveM's crash place and its stack lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogPart {
    pub text: String,
    pub lines: usize,
    pub hash: Option<String>,
    pub stack: Vec<String>,
}

/// «[   11159359] [    DumpServer]                27660/ text» → «text».
fn message(line: &str) -> &str {
    if line.starts_with('[') {
        if let Some(i) = line.find("/ ") {
            return line.get(i + 2..).unwrap_or(line);
        }
    }
    line
}

/// At most `max` bytes, cut at a letter.
pub fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    s.get(..i).unwrap_or("")
}

/// `text`: the end of a FiveM log. The last «Crash dialog content:» block (DumpServer) ends the part.
pub fn log_part(text: &str, max_lines: usize) -> LogPart {
    let lines: Vec<&str> = text.lines().collect();
    let mut end = lines.len();
    let mut hash = None;
    let mut stack = Vec::new();
    if let Some(i) = lines.iter().rposition(|l| l.contains("Crash dialog content:")) {
        hash = lines.get(i + 1).map(|l| message(l).trim()).filter(|h| !h.is_empty() && h.len() <= 160).map(str::to_string);
        end = (i + 3).min(lines.len());
        let limit = lines.len().min(i + 12);
        if let Some(s) = (i..limit).find(|&k| lines.get(k).map_or(false, |l| message(l).trim() == "Stack trace:")) {
            let mut k = s + 1;
            while let Some(l) = lines.get(k) {
                let m = message(l).trim();
                if m.is_empty() || m.contains(' ') || !m.contains('+') || stack.len() >= 30 {
                    break;
                }
                stack.push(m.to_string());
                k += 1;
            }
            end = k.min(lines.len());
        }
    }
    let start = end.saturating_sub(max_lines);
    let part: Vec<&str> = lines.get(start..end).unwrap_or(&[]).iter().map(|l| cut(l, LINE_MAX)).collect();
    LogPart { text: part.join("\n"), lines: part.len(), hash, stack }
}

/// The end of a log file (the first, maybe cut line dropped when the file is longer).
pub fn log_tail(path: &Path) -> String {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let text = support::read_tail(path, LOG_READ);
    if len > LOG_READ {
        return text.split_once('\n').map_or(String::new(), |(_, rest)| rest.to_string());
    }
    text
}

// ---------------------------------------------------------------- cleaning the texts

/// What `clean` hides and how.
#[derive(Clone, Debug, Default)]
pub struct Clean {
    /// The user name, the PC name, our addresses (support.rs).
    pub scrub: support::Scrub,
    /// Never leaves the PC.
    pub salt: String,
    /// Rockstar IDs known from crashometry: hashed wherever they stand.
    pub rockstar: Vec<String>,
}

/// 12 hex characters of SHA-256 (salted): the same id gives the same word, the id itself does not come back.
pub fn hash_id(salt: &str, id: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, format!("coldz-crash-v1:{salt}:{id}").as_bytes());
    d.as_ref().iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Replaces `spans` (start, end, with) of `text`, sorted and not overlapping; a span that is not on letter borders
/// is left as it is.
fn rebuild(text: &str, spans: &[(usize, usize, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    for (s, e, with) in spans {
        if *s < at || *e < *s {
            continue;
        }
        let (Some(head), Some(_)) = (text.get(at..*s), text.get(*s..*e)) else { continue };
        out.push_str(head);
        out.push_str(with);
        at = *e;
    }
    out.push_str(text.get(at..).unwrap_or(""));
    out
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whole numbers equal to one of `ids` → `rid:<hash>`; «Account id 123» (FiveM's Rockstar account) too.
fn hash_rockstar(text: &str, ids: &[String], salt: &str) -> String {
    let b = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_digit()) {
            let mut e = i;
            while e < b.len() && b[e].is_ascii_digit() {
                e += 1;
            }
            let num = text.get(i..e).unwrap_or("");
            let after_account = text.get(..i).map_or(false, |h| h.ends_with("Account id ") || h.ends_with("RockstarId=") || h.ends_with("rockstarId\":"));
            if num.len() >= 5 && (ids.iter().any(|x| x == num) || after_account) {
                spans.push((i, e, format!("rid:{}", hash_id(salt, num))));
            }
            i = e;
            continue;
        }
        i += 1;
    }
    rebuild(text, &spans)
}

const SECRET_PARTS: [&str; 17] = [
    "token", "ticket", "secret", "password", "passwd", "apikey", "api_key", "api-key", "cdnkey", "licensekey",
    "license_key", "sessionid", "session_id", "cookie", "authorization", "credential", "private_key",
];
const SECRET_WORDS: [&str; 6] = ["auth", "pwd", "sig", "signature", "key", "bearer"];

/// `token=…`, `"password": "…"`, `Authorization: Bearer …`: the value → `<hidden>`.
fn hide_secrets(text: &str) -> String {
    let b = text.as_bytes();
    let stop = |c: u8| c.is_ascii_whitespace() || b"\"',;&)}]<>".contains(&c);
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !(b[i].is_ascii_alphabetic() || b[i] == b'_') || (i > 0 && (is_word(b[i - 1]) || b[i - 1] == b'-')) {
            i += 1;
            continue;
        }
        let mut e = i;
        while e < b.len() && (is_word(b[e]) || b[e] == b'-') {
            e += 1;
        }
        let word = text.get(i..e).unwrap_or("").to_ascii_lowercase();
        let secret = SECRET_PARTS.iter().any(|p| word.contains(p)) || SECRET_WORDS.contains(&word.as_str());
        let mut j = e;
        if secret {
            if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                j += 1;
            }
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            let bearer = word == "bearer" && j > e;
            if bearer || (j < b.len() && (b[j] == b'=' || b[j] == b':')) {
                if !bearer {
                    j += 1;
                }
                while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                    j += 1;
                }
                if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                    j += 1;
                }
                let mut v = j;
                while v < b.len() && !stop(b[v]) {
                    v += 1;
                }
                let value = text.get(j..v).unwrap_or("");
                // «Bearer xyz», «Basic xyz»: the word after it is the secret
                if matches!(value.to_ascii_lowercase().as_str(), "bearer" | "basic") {
                    let mut k = v;
                    while k < b.len() && b[k] == b' ' {
                        k += 1;
                    }
                    let mut w = k;
                    while w < b.len() && !stop(b[w]) {
                        w += 1;
                    }
                    if w > k {
                        spans.push((k, w, "<hidden>".into()));
                    }
                    i = w.max(e);
                    continue;
                }
                if value.len() >= 6 && !value.starts_with('<') {
                    spans.push((j, v, "<hidden>".into()));
                    i = v;
                    continue;
                }
            }
        }
        i = e;
    }
    rebuild(text, &spans)
}

const ID_PREFIXES: [&str; 7] = ["steam:", "license:", "license2:", "discord:", "xbl:", "live:", "fivem:"];

/// FiveM's player identifiers (`steam:110000…`, `license:…`, `discord:…`): the number → `<id>`.
fn hide_ids(text: &str) -> String {
    let b = text.as_bytes();
    let low = text.to_ascii_lowercase();
    let mut spans = Vec::new();
    for p in ID_PREFIXES {
        let mut from = 0;
        while let Some(pos) = low.get(from..).and_then(|s| s.find(p)) {
            let s = from + pos;
            let v = s + p.len();
            from = v;
            if s > 0 && is_word(b[s - 1]) {
                continue;
            }
            let mut e = v;
            while e < b.len() && b[e].is_ascii_hexdigit() {
                e += 1;
            }
            if e - v >= 6 && !(e < b.len() && is_word(b[e])) {
                spans.push((v, e, "<id>".to_string()));
            }
        }
    }
    spans.sort_by_key(|s| s.0);
    rebuild(text, &spans)
}

/// JWT (`eyJ…`) and long random-looking runs (upper and lower case and digits, 40+): `<hidden>`.
fn hide_blobs(text: &str) -> String {
    let b = text.as_bytes();
    let run = |c: u8| c.is_ascii_alphanumeric() || b"+=_-.".contains(&c);
    let mut spans = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !run(b[i]) || (i > 0 && run(b[i - 1])) {
            i += 1;
            continue;
        }
        let mut e = i;
        while e < b.len() && run(b[e]) {
            e += 1;
        }
        let token = text.get(i..e).unwrap_or("");
        let jwt = token.starts_with("eyJ") && token.matches('.').count() >= 2 && token.len() >= 30;
        let opaque = token.split('.').any(|piece| {
            piece.len() >= 40
                && piece.bytes().any(|c| c.is_ascii_uppercase())
                && piece.bytes().any(|c| c.is_ascii_lowercase())
                && piece.bytes().any(|c| c.is_ascii_digit())
        });
        if jwt || opaque {
            spans.push((i, e, "<hidden>".to_string()));
        }
        i = e;
    }
    rebuild(text, &spans)
}

/// IPv6 addresses (full, or with `::`) → `<ip6>`; times like 10:22:33 and MAC-like words stay.
fn hide_ipv6(text: &str) -> String {
    let b = text.as_bytes();
    let part = |c: u8| c.is_ascii_hexdigit() || c == b':';
    let mut spans = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !part(b[i]) || (i > 0 && (part(b[i - 1]) || b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'.')) {
            i += 1;
            continue;
        }
        let mut e = i;
        while e < b.len() && part(b[e]) {
            e += 1;
        }
        let token = text.get(i..e).unwrap_or("");
        let next_ok = !(e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'.'));
        let groups: Vec<&str> = token.split(':').collect();
        let filled = groups.iter().filter(|g| !g.is_empty()).count();
        let ok = next_ok
            && (3..=8).contains(&groups.len())
            && groups.iter().all(|g| g.len() <= 4)
            && filled >= 2
            && (token.contains("::") || groups.len() == 8)
            && groups.iter().any(|g| g.len() >= 3 || g.bytes().any(|c| c.is_ascii_alphabetic()));
        if ok {
            spans.push((i, e, "<ip6>".to_string()));
        }
        i = e;
    }
    rebuild(text, &spans)
}

/// Windows account SIDs (`S-1-5-21-…`).
fn hide_sids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("S-1-5-21-") {
        let (head, tail) = rest.split_at(pos + "S-1-5-21-".len());
        out.push_str(head);
        let n = tail.bytes().take_while(|c| c.is_ascii_digit() || *c == b'-').count();
        if n > 0 {
            out.push_str("<sid>");
        }
        rest = tail.get(n..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// Profile folders that are nobody's name.
const SHARED_PROFILES: [&str; 6] = ["public", "default", "default user", "all users", "%user%", "<user>"];

/// Any `…\Users\<name>\…` (also `/`, `\\` in JSON) → `…\Users\%USER%\…`, whatever the name: a dump or a crash pack may
/// come from another Windows account than the one the launcher runs in (20.11: a player's pack had «C:\Users\Юрка»).
fn hide_user_dirs(text: &str) -> String {
    let b = text.as_bytes();
    let sep = |c: u8| c == b'\\' || c == b'/';
    let mut spans = Vec::new();
    let mut i = 0;
    while i + 7 <= b.len() {
        if !(sep(b[i]) && b.get(i + 1..i + 6).map_or(false, |w| w.eq_ignore_ascii_case(b"users")) && sep(b[i + 6])) {
            i += 1;
            continue;
        }
        let mut s = i + 7;
        while s < b.len() && sep(b[s]) {
            s += 1;
        }
        // the name: up to the next separator, a quote or the line end, at most 64 bytes
        let rest = text.get(s..).unwrap_or("");
        let end = rest
            .char_indices()
            .find(|(_, c)| matches!(c, '\\' | '/' | '"' | '\'' | '\n' | '\r' | '\t' | '|' | '<' | '>' | ':' | '*' | '?'))
            .map_or(rest.len(), |(k, _)| k);
        let name = rest.get(..end).unwrap_or("");
        if !name.trim().is_empty() && end <= 64 && !SHARED_PROFILES.contains(&name.to_lowercase().as_str()) {
            spans.push((s, s + end, "%USER%".to_string()));
        }
        i = s + end.max(1);
    }
    rebuild(text, &spans)
}

/// A text of the report with the private parts hidden (module doc).
pub fn clean(text: &str, c: &Clean) -> String {
    let t = hash_rockstar(text, &c.rockstar, &c.salt);
    let t = support::scrub(&t, &c.scrub).replace("<user>", "%USER%").replace("<pc>", "%PC%");
    let t = hide_user_dirs(&t);
    let t = hide_secrets(&t);
    let t = hide_ids(&t);
    let t = hide_blobs(&t);
    let t = hide_ipv6(&t);
    hide_sids(&t)
}

/// FiveM's `crashometry.json` from its crash pack, cleaned: the Rockstar ID hashed, every other value through `clean`.
/// Also returns the Rockstar IDs (to hash them in the log too), the crash place and FiveM's three words.
pub fn crashometry_from(raw: &[u8], c: &Clean) -> Option<(String, Vec<String>, String, String)> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let obj = v.as_object()?;
    let mut ids = Vec::new();
    for (k, val) in obj {
        if k.to_ascii_lowercase().contains("rockstarid") {
            if let Some(s) = val.as_str().map(str::to_string).or_else(|| val.as_u64().map(|n| n.to_string())) {
                if !s.is_empty() {
                    ids.push(s);
                }
            }
        }
    }
    let c2 = Clean { rockstar: ids.clone(), ..c.clone() };
    let mut out = serde_json::Map::new();
    for (k, val) in obj.iter().take(200) {
        let key = clean(cut(k, 100), &c2);
        let value = if k.to_ascii_lowercase().contains("rockstarid") {
            serde_json::Value::String(format!("rid:{}", hash_id(&c.salt, val.as_str().map(str::to_string).unwrap_or_else(|| val.to_string()).as_str())))
        } else {
            match val {
                serde_json::Value::String(s) => serde_json::Value::String(clean(cut(s, 4000), &c2)),
                serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Null => val.clone(),
                other => serde_json::Value::String(clean(cut(&other.to_string(), 4000), &c2)),
            }
        };
        out.insert(key, value);
    }
    let hash = obj.get("crash_hash").and_then(|h| h.as_str()).map(|h| cut(h, 160).to_string()).unwrap_or_default();
    let key = obj.get("crash_hash_key").and_then(|h| h.as_str()).map(|h| cut(h, 80).to_string()).unwrap_or_default();
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(out)).ok()?;
    Some((text, ids, hash, key))
}

// ---------------------------------------------------------------- the report

/// A video card as Windows lists it (registry, class «Display»).
#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct Gpu {
    pub name: String,
    pub driver: String,
    pub date: String,
}

/// What the report knows about this PC and the launcher.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub launcher: String,
    /// ru | en | es | pt-BR | "".
    pub lang: String,
    pub clean: Clean,
    pub windows: String,
    pub gpus: Vec<Gpu>,
    pub ram_gb: Option<u64>,
    /// `FiveM.app\logs`.
    pub logs: Option<PathBuf>,
    pub new_count: usize,
}

/// Where the dump comes from when the player ticks it.
#[derive(Clone, Debug, PartialEq)]
pub enum DumpSrc {
    Raw { path: PathBuf, size: u64 },
    Packed { pack: PathBuf, entry: ZipEntry },
}

impl DumpSrc {
    pub fn size(&self) -> u64 {
        match self {
            DumpSrc::Raw { size, .. } => *size,
            DumpSrc::Packed { entry, .. } => entry.usize,
        }
    }
}

/// One line of the card: what goes.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// crashometry | log | modules | info
    pub key: &'static str,
    pub name: String,
    pub size: u64,
}

/// Everything but the zip itself: the texts are made (and cleaned) once, shown in the card, then packed.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub crash: Crash,
    pub hash: String,
    pub key: String,
    pub hash_from: &'static str,
    /// (name in the zip, cleaned text), without report.json (made in `pack`, it says whether the dump went).
    pub texts: Vec<(String, String)>,
    pub dump: Option<DumpSrc>,
    pub info: Option<DumpInfo>,
    pub log_name: String,
    pub log_lines: usize,
    pub log_from: &'static str,
    pub ctx: Ctx,
}

/// The card's list.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub id: String,
    pub at: u64,
    pub hash: String,
    pub key: String,
    pub items: Vec<Item>,
    /// The dump's size (unpacked) when the player may tick it.
    pub dump: Option<u64>,
    /// The report may be at most this big with the dump (MB).
    pub limit_mb: u64,
}

/// The crash place, FiveM's three words and where they came from, cheaply (for the question under «Играть»): the
/// crash pack, then the log, then the dump.
pub fn quick_hash(c: &Crash, logs: Option<&Path>) -> (String, String) {
    if let Some(p) = &c.pack {
        if let Some(mut f) = FileAt::open(&p.path) {
            if let Some(e) = f.entries().and_then(|es| es.into_iter().find(|e| base_name(&e.name).eq_ignore_ascii_case(NAME_CRASHOMETRY))) {
                if let Some((_, _, h, k)) = f.unpack(&e, 64 * 1024).and_then(|raw| crashometry_from(&raw, &Clean::default())) {
                    if !h.is_empty() {
                        return (h, k);
                    }
                }
            }
        }
    }
    if let Some(h) = logs.and_then(|l| find_log(l, c.at)).and_then(|f| log_part(&log_tail(&f.path), 50).hash) {
        return (h, String::new());
    }
    if let Some(d) = c.dump.as_ref().filter(|d| d.size <= DUMP_READ_MAX) {
        if let Some(info) = std::fs::read(&d.path).ok().and_then(|b| read_dump(&b)) {
            if let Some(p) = info.address.and_then(|a| place(&info.modules, a)) {
                return (p, String::new());
            }
        }
    }
    (String::new(), String::new())
}

/// Reads what the report needs and cleans it. Never fails: what is missing is just not there.
pub fn prepare(crash: &Crash, ctx: &Ctx) -> Prepared {
    let mut clean_ctx = ctx.clean.clone();
    let mut texts: Vec<(String, String)> = Vec::new();
    let (mut hash, mut key, mut hash_from) = (String::new(), String::new(), "");
    let mut dump: Option<DumpSrc> = None;
    let mut info: Option<DumpInfo> = None;
    let mut pack_log: Option<String> = None;
    // FiveM's crash pack: crashometry.json, and the dump and the log when FiveM.app no longer has them
    if let Some(p) = &crash.pack {
        if let Some(mut f) = FileAt::open(&p.path) {
            let entries = f.entries().unwrap_or_default();
            let find = |n: &str| entries.iter().find(|e| base_name(&e.name).eq_ignore_ascii_case(n)).cloned();
            if let Some(e) = find(NAME_CRASHOMETRY) {
                if let Some(raw) = f.unpack(&e, 64 * 1024) {
                    if let Some((text, ids, h, k)) = crashometry_from(&raw, &clean_ctx) {
                        clean_ctx.rockstar.extend(ids);
                        texts.push((NAME_CRASHOMETRY.to_string(), text));
                        if !h.is_empty() {
                            (hash, key, hash_from) = (h, k, "crashometry");
                        }
                    }
                }
            }
            if crash.dump.is_none() {
                if let Some(e) = find(&crash.id).filter(|e| e.usize <= DUMP_READ_MAX && e.csize <= PACK_READ_MAX) {
                    info = f.unpack(&e, DUMP_READ_MAX as usize).and_then(|raw| read_dump(&raw));
                    dump = Some(DumpSrc::Packed { pack: p.path.clone(), entry: e });
                }
            }
            if let Some(e) = entries.iter().find(|e| {
                let n = base_name(&e.name).to_ascii_lowercase();
                n.starts_with("citizenfx_log") && n.ends_with(".log")
            }) {
                pack_log = f.unpack(e, 64 * 1024 * 1024).map(|raw| {
                    let text = String::from_utf8_lossy(&raw).into_owned();
                    let from = text.len().saturating_sub(LOG_READ as usize);
                    let mut from_ok = from;
                    while from_ok < text.len() && !text.is_char_boundary(from_ok) {
                        from_ok += 1;
                    }
                    text.get(from_ok..).unwrap_or("").to_string()
                });
            }
        }
    }
    // the raw dump in FiveM.app\crashes
    if let Some(d) = crash.dump.as_ref().filter(|d| d.size <= DUMP_READ_MAX) {
        info = std::fs::read(&d.path).ok().and_then(|b| read_dump(&b));
        dump = Some(DumpSrc::Raw { path: d.path.clone(), size: d.size });
    }
    // the log of that session (FiveM.app\logs), else the one in the pack
    let (mut log_name, mut log_from) = (String::new(), "");
    let log_text = match ctx.logs.as_deref().and_then(|l| find_log(l, crash.at)) {
        Some(f) => {
            log_name = f.name.clone();
            log_from = "logs";
            Some(log_tail(&f.path))
        }
        None => pack_log.inspect(|_| {
            log_from = "pack";
            log_name = "CitizenFX_log (crash pack)".into();
        }),
    };
    let mut log_lines = 0;
    if let Some(text) = log_text {
        let part = log_part(&text, LOG_LINES);
        if hash.is_empty() {
            if let Some(h) = &part.hash {
                (hash, hash_from) = (h.clone(), "log");
            }
        }
        log_lines = part.lines;
        texts.push((NAME_LOG.to_string(), clean(&part.text, &clean_ctx)));
    }
    if let Some(i) = &info {
        if hash.is_empty() {
            if let Some(p) = i.address.and_then(|a| place(&i.modules, a)) {
                (hash, hash_from) = (p, "dump");
            }
        }
        let size = dump.as_ref().map_or(0, DumpSrc::size);
        texts.push((NAME_MODULES.to_string(), modules_text(i, &crash.id, size, &|s| clean(s, &clean_ctx))));
    }
    let ctx = Ctx { clean: clean_ctx, ..ctx.clone() };
    Prepared {
        crash: crash.clone(),
        hash: clean(&hash, &ctx.clean),
        key: clean(&key, &ctx.clean),
        hash_from,
        texts,
        dump,
        info,
        log_name,
        log_lines,
        log_from,
        ctx,
    }
}

/// `report.json`: what the receiver and the triage read first.
pub fn report_json(p: &Prepared, dump_attached: bool, dump_skipped: &str) -> String {
    let i = p.info.clone().unwrap_or_default();
    let exe = i.modules.first();
    let drivers: Vec<serde_json::Value> = i
        .modules
        .iter()
        .filter(|m| DRIVER_DLLS.contains(&m.name().to_ascii_lowercase().as_str()))
        .map(|m| serde_json::json!({ "name": m.name(), "version": m.version }))
        .collect();
    let c = &p.ctx.clean;
    let v = serde_json::json!({
        "format": FORMAT,
        "launcher": p.ctx.launcher,
        "reporter": hash_id(&c.salt, "reporter"),
        "lang": p.ctx.lang,
        "crash": {
            "id": p.crash.id,
            "at": p.crash.at,
            "atUtc": crate::fivem::utc_stamp(p.crash.at),
            "hash": p.hash,
            "key": p.key,
            "hashFrom": p.hash_from,
            "newCount": p.ctx.new_count,
            "pack": p.crash.pack.is_some(),
        },
        "game": {
            "exe": exe.map(|m| m.name().to_string()).unwrap_or_default(),
            "build": exe.and_then(|m| game_build(m.name())),
            "fivem": exe.map(|m| m.version.clone()).unwrap_or_default(),
            "uptimeS": i.uptime,
        },
        "exception": {
            "code": i.code.map(|c| format!("0x{c:08X}")),
            "name": i.code.map(code_name).unwrap_or(""),
            "at": i.address.and_then(|a| place(&i.modules, a)),
        },
        "system": {
            "windows": clean(&p.ctx.windows, c),
            "dumpWindows": i.windows,
            "cpus": i.cpus,
            "ramGb": p.ctx.ram_gb,
        },
        "gpus": p.ctx.gpus.iter().take(4).map(|g| serde_json::json!({
            "name": clean(cut(&g.name, 120), c),
            "driver": cut(&g.driver, 40),
            "date": cut(&g.date, 20),
        })).collect::<Vec<_>>(),
        "drivers": drivers,
        "files": {
            "log": p.log_name,
            "logLines": p.log_lines,
            "logFrom": p.log_from,
        },
        "dump": {
            "attached": dump_attached,
            "size": p.dump.as_ref().map_or(0, DumpSrc::size),
            "skipped": dump_skipped,
        },
    });
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into())
}

/// The card's list (sizes of the cleaned texts; report.json as it will be without the dump).
pub fn preview(p: &Prepared, dump_allowed: bool) -> Preview {
    let mut items: Vec<Item> = Vec::new();
    for (name, text) in &p.texts {
        let key = match name.as_str() {
            NAME_CRASHOMETRY => "crashometry",
            NAME_LOG => "log",
            _ => "modules",
        };
        items.push(Item { key, name: name.clone(), size: text.len() as u64 });
    }
    items.push(Item { key: "info", name: NAME_REPORT.into(), size: report_json(p, false, "").len() as u64 });
    Preview {
        id: p.crash.id.clone(),
        at: p.crash.at,
        hash: p.hash.clone(),
        key: p.key.clone(),
        items,
        dump: p.dump.as_ref().filter(|_| dump_allowed).map(DumpSrc::size),
        limit_mb: (LIMIT_DUMP / (1024 * 1024)) as u64,
    }
}

/// The report zip.
#[derive(Clone, Debug)]
pub struct Packed {
    pub bytes: Vec<u8>,
    pub dump_attached: bool,
    /// "" | "too_big" | "unreadable" | "off"
    pub dump_skipped: &'static str,
}

/// Packs the report: within `LIMIT_SMALL` without the dump (the log gets shorter if it must), the dump only with
/// `with_dump` and only while the whole stays within `LIMIT_DUMP`. Err: even the smallest report does not fit.
pub fn pack(p: &Prepared, with_dump: bool, dump_allowed: bool) -> Result<Packed, String> {
    let mut texts = p.texts.clone();
    let small = |texts: &[(String, String)], attached: bool, skipped: &str| -> Vec<Part> {
        let mut parts: Vec<Part> = texts.iter().map(|(n, t)| Part::pack(n, t.as_bytes())).collect();
        parts.push(Part::pack(NAME_REPORT, report_json(p, attached, skipped).as_bytes()));
        parts
    };
    let size = |parts: &[Part]| parts.iter().map(Part::len).sum::<usize>() + 22;
    // without the dump: within 2 MB, the log halves while it does not fit
    let mut parts = small(&texts, false, "");
    while size(&parts) > LIMIT_SMALL {
        let Some((_, log)) = texts.iter_mut().find(|(n, _)| n == NAME_LOG) else { break };
        let lines: Vec<&str> = log.lines().collect();
        if lines.len() <= 20 {
            break;
        }
        *log = lines.get(lines.len() / 2..).unwrap_or(&[]).join("\n");
        parts = small(&texts, false, "");
    }
    if size(&parts) > LIMIT_SMALL {
        return Err("too_big".into());
    }
    if !with_dump {
        return Ok(Packed { bytes: zip(&parts), dump_attached: false, dump_skipped: "" });
    }
    let skipped = |why: &'static str| -> Result<Packed, String> {
        let parts = small(&texts, false, why);
        Ok(Packed { bytes: zip(&parts), dump_attached: false, dump_skipped: why })
    };
    if !dump_allowed {
        return skipped("off");
    }
    let dump_part = match &p.dump {
        Some(DumpSrc::Raw { path, size }) if *size <= DUMP_READ_MAX => std::fs::read(path).ok().map(|raw| Part::pack(&p.crash.id, &raw)),
        Some(DumpSrc::Packed { pack, entry }) if entry.csize <= LIMIT_DUMP as u64 && (entry.method == 0 || entry.method == 8) && entry.flags & 1 == 0 => {
            FileAt::open(pack).and_then(|mut f| f.raw(entry)).map(|data| Part {
                name: p.crash.id.clone(),
                method: entry.method,
                crc: entry.crc,
                raw_len: entry.usize,
                data,
            })
        }
        _ => None,
    };
    let Some(dump_part) = dump_part else { return skipped("unreadable") };
    let mut with = small(&texts, true, "");
    with.push(dump_part);
    if size(&with) > LIMIT_DUMP {
        return skipped("too_big");
    }
    Ok(Packed { bytes: zip(&with), dump_attached: true, dump_skipped: "" })
}

/// `crash-<YYYYMMDD-HHMMSS>-<id>.zip` for the outbox.
pub fn outbox_name(crash: &Crash, now: u64) -> String {
    let stamp: String = crate::fivem::utc_stamp(now).chars().filter(char::is_ascii_digit).take(14).collect();
    let (day, time) = (stamp.get(..8).unwrap_or(""), stamp.get(8..).unwrap_or(""));
    let id: String = crash.id.trim_end_matches(".dmp").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect();
    format!("crash-{day}-{time}-{id}.zip")
}

/// Writes the report into the outbox: `<name>.part`, then the name (a half-written file is never sent).
pub fn outbox_put(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let done = dir.join(name);
    let part = dir.join(format!("{name}.part"));
    std::fs::write(&part, bytes)?;
    std::fs::rename(&part, &done)?;
    Ok(done)
}

// ---------------------------------------------------------------- sending

/// How a send went.
#[derive(Clone, Debug, PartialEq)]
pub enum Sent {
    /// Taken; the receiver's id of the report.
    Ok(String),
    /// Not now (no connection, the receiver busy or down): again at the next start.
    Retry(String),
    /// The receiver said no (wrong key, too big, not a report): not sent again.
    Refused(String),
}

/// What the receiver's answer means.
pub fn classify(code: u16, body: &str) -> Sent {
    match code {
        200..=299 => {
            let id = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| cut(s, 80).to_string()))
                .unwrap_or_default();
            Sent::Ok(id)
        }
        408 | 425 | 429 | 500..=599 => Sent::Retry(format!("HTTP {code}")),
        _ => Sent::Refused(format!("HTTP {code}: {}", cut(body.trim(), 200))),
    }
}

/// Time for one try: small reports quickly, a report with the dump gets minutes on a slow line.
pub fn timeout_for(len: usize) -> Duration {
    Duration::from_secs(30 + (len / (64 * 1024)) as u64).min(Duration::from_secs(330))
}

/// One POST of the report zip. Never panics, never retries by itself.
pub async fn send(url: &str, key: &str, body: Vec<u8>) -> Sent {
    let timeout = timeout_for(body.len());
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("COLDZ-Launcher/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Sent::Retry(format!("клиент HTTP: {e}")),
    };
    let resp = client
        .post(url)
        .header("Content-Type", "application/zip")
        .header("X-Coldz-Key", key)
        .header("X-Coldz-Format", FORMAT.to_string())
        .body(body)
        .send()
        .await;
    match resp {
        Ok(mut r) => {
            let code = r.status().as_u16();
            // the answer is a short JSON: read at most 4 KB of it
            let mut text = Vec::new();
            while let Ok(Some(chunk)) = r.chunk().await {
                text.extend_from_slice(&chunk);
                if text.len() > 4096 {
                    break;
                }
            }
            classify(code, &String::from_utf8_lossy(&text))
        }
        Err(e) => Sent::Retry(net_why(&e)),
    }
}

fn net_why(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "время вышло"
    } else if e.is_connect() {
        "нет соединения"
    } else {
        "связь"
    };
    format!("{kind}: {}", cut(&e.to_string(), 200))
}

// ---------------------------------------------------------------- this PC

/// Video cards from the registry (class «Display»: name, driver version and date), at most 4.
#[cfg(windows)]
pub fn gpus() -> Vec<Gpu> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;
    let class = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(class, KEY_READ) else { return Vec::new() };
    let mut out = Vec::new();
    for name in k.enum_keys().flatten().take(32) {
        if name.len() != 4 || !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(sub) = k.open_subkey_with_flags(&name, KEY_READ) else { continue };
        let desc: String = sub.get_value("DriverDesc").unwrap_or_default();
        if desc.trim().is_empty() {
            continue;
        }
        let gpu = Gpu {
            name: desc.trim().to_string(),
            driver: sub.get_value::<String, _>("DriverVersion").unwrap_or_default(),
            date: sub.get_value::<String, _>("DriverDate").unwrap_or_default(),
        };
        if !out.contains(&gpu) {
            out.push(gpu);
        }
        if out.len() >= 4 {
            break;
        }
    }
    out
}

#[cfg(not(windows))]
pub fn gpus() -> Vec<Gpu> {
    Vec::new()
}

/// Memory of this PC, GB.
#[cfg(windows)]
pub fn ram_gb() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    unsafe {
        let mut m: MEMORYSTATUSEX = std::mem::zeroed();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        (GlobalMemoryStatusEx(&mut m) != 0).then(|| (m.ullTotalPhys + 512 * 1024 * 1024) / (1024 * 1024 * 1024))
    }
}

#[cfg(not(windows))]
pub fn ram_gb() -> Option<u64> {
    None
}

/// The Downloads folder (FiveM's crash window saves its pack wherever the player says: often there or the desktop).
#[cfg(windows)]
pub fn downloads_dir() -> Option<PathBuf> {
    crate::fivem::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Downloads)
}

#[cfg(not(windows))]
pub fn downloads_dir() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------ a made-up minidump (the layout of minidumpapiset.h)

    struct Fake {
        d: Vec<u8>,
    }

    impl Fake {
        fn put32(&mut self, at: usize, v: u32) {
            self.d[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        fn put64(&mut self, at: usize, v: u64) {
            self.d[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
        fn add(&mut self, bytes: &[u8]) -> usize {
            while self.d.len() % 8 != 0 {
                self.d.push(0);
            }
            let at = self.d.len();
            self.d.extend_from_slice(bytes);
            at
        }
        fn string(&mut self, s: &str) -> usize {
            let u: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
            let mut b = (u.len() as u32).to_le_bytes().to_vec();
            b.extend_from_slice(&u);
            b.extend_from_slice(&[0, 0]);
            self.add(&b)
        }
    }

    pub(crate) const EXE_BASE: u64 = 0x7FF7_0000_0000;
    pub(crate) const CEF_BASE: u64 = 0x7FFB_0000_0000;

    /// A dump of «FiveM_b3889_GTAProcess.exe» that crashed in libcef.dll+E0D01, a user path in a module.
    pub(crate) fn fake_dump() -> Vec<u8> {
        let mut f = Fake { d: vec![0u8; 32 + 6 * 12] };
        f.put32(0, 0x504d_444d);
        f.put32(4, 0xA793);
        f.put32(8, 6);
        f.put32(12, 32);
        f.put32(20, 1_795_000_000);
        let mods = [
            (EXE_BASE, 0x61D_8000u32, r"C:\Users\Ivan\AppData\Local\FiveM\FiveM.app\data\cache\subprocess\FiveM_b3889_GTAProcess.exe", (2u32 << 16, 38410u32)),
            (CEF_BASE, 0x800_0000u32, r"C:\Users\Ivan\AppData\Local\FiveM\FiveM.app\bin\cef\libcef.dll", (1 << 16, 0)),
            (0x7FFC_0000_0000, 0x100_0000u32, r"C:\Windows\System32\DriverStore\FileRepository\nv_dispi.inf_amd64\nvwgf2umx.dll", ((32 << 16) | 0, (15 << 16) | 9144)),
        ];
        let names: Vec<usize> = mods.iter().map(|m| f.string(m.2)).collect();
        let mut list = (mods.len() as u32).to_le_bytes().to_vec();
        for (i, m) in mods.iter().enumerate() {
            let mut e = vec![0u8; 108];
            e[0..8].copy_from_slice(&m.0.to_le_bytes());
            e[8..12].copy_from_slice(&m.1.to_le_bytes());
            e[16..20].copy_from_slice(&1_790_000_000u32.to_le_bytes());
            e[20..24].copy_from_slice(&(names[i] as u32).to_le_bytes());
            e[24 + 8..24 + 12].copy_from_slice(&m.3 .0.to_le_bytes());
            e[24 + 12..24 + 16].copy_from_slice(&m.3 .1.to_le_bytes());
            list.extend_from_slice(&e);
        }
        let mod_rva = f.add(&list);
        // the crashed thread's stack: rsp points 16 bytes in; return addresses into the exe and libcef, data between
        let stack_start = 0x0000_00A0_0000_0000u64;
        let mut stack = Vec::new();
        for v in [0x1111u64, 0x2222, EXE_BASE + 0x92B364, 0x3333, CEF_BASE + 0x72402FC, CEF_BASE + 0x72402FC, 0, EXE_BASE + 0x1028] {
            stack.extend_from_slice(&v.to_le_bytes());
        }
        let stack_rva = f.add(&stack);
        let mut ctx = vec![0u8; 0x4D0];
        ctx[0x98..0xA0].copy_from_slice(&(stack_start + 16).to_le_bytes());
        ctx[0xF8..0x100].copy_from_slice(&(CEF_BASE + 0xE0D01).to_le_bytes());
        let ctx_rva = f.add(&ctx);
        let mut thr = 1u32.to_le_bytes().to_vec();
        let mut t = vec![0u8; 48];
        t[0..4].copy_from_slice(&27660u32.to_le_bytes());
        t[24..32].copy_from_slice(&stack_start.to_le_bytes());
        t[32..36].copy_from_slice(&(stack.len() as u32).to_le_bytes());
        t[36..40].copy_from_slice(&(stack_rva as u32).to_le_bytes());
        thr.extend_from_slice(&t);
        let thr_rva = f.add(&thr);
        let mut ex = vec![0u8; 168];
        ex[0..4].copy_from_slice(&27660u32.to_le_bytes());
        ex[8..12].copy_from_slice(&0xC000_0005u32.to_le_bytes());
        ex[24..32].copy_from_slice(&(CEF_BASE + 0xE0D01).to_le_bytes());
        ex[160..164].copy_from_slice(&0x4D0u32.to_le_bytes());
        ex[164..168].copy_from_slice(&(ctx_rva as u32).to_le_bytes());
        let ex_rva = f.add(&ex);
        let mut sys = vec![0u8; 56];
        sys[0..2].copy_from_slice(&9u16.to_le_bytes());
        sys[6] = 16;
        sys[8..12].copy_from_slice(&10u32.to_le_bytes());
        sys[16..20].copy_from_slice(&26100u32.to_le_bytes());
        let sys_rva = f.add(&sys);
        let mut misc = vec![0u8; 24];
        misc[0..4].copy_from_slice(&24u32.to_le_bytes());
        misc[4..8].copy_from_slice(&2u32.to_le_bytes());
        misc[12..16].copy_from_slice(&(1_795_000_000u32 - 96).to_le_bytes());
        let misc_rva = f.add(&misc);
        for (i, (t, size, rva)) in [(4u32, list.len(), mod_rva), (3, 52, thr_rva), (6, 168, ex_rva), (7, 56, sys_rva), (15, 24, misc_rva), (0, 0, 0)].iter().enumerate() {
            f.put32(32 + i * 12, *t);
            f.put32(32 + i * 12 + 4, *size as u32);
            f.put32(32 + i * 12 + 8, *rva as u32);
        }
        f.put64(24, 0x201120);
        f.d
    }

    #[test]
    fn reads_modules_exception_and_stack_from_a_dump() {
        let d = fake_dump();
        let i = read_dump(&d).unwrap();
        assert_eq!(i.modules.len(), 3);
        assert_eq!(i.modules[0].name(), "FiveM_b3889_GTAProcess.exe");
        assert_eq!(i.modules[0].version, "2.0.0.38410");
        assert_eq!(i.modules[2].version, "32.0.15.9144");
        assert_eq!(game_build(i.modules[0].name()), Some(3889));
        assert_eq!(game_build("GTA5_b3258.exe"), Some(3258));
        assert_eq!(game_build("FiveM.exe"), None);
        assert_eq!((i.thread, i.code), (Some(27660), Some(0xC000_0005)));
        assert_eq!(i.address.and_then(|a| place(&i.modules, a)).as_deref(), Some("libcef.dll+E0D01"));
        // the scan starts at rsp (the two words below it are skipped), repeats once, the exe's base itself is no frame
        assert_eq!(i.stack, ["libcef.dll+E0D01", "FiveM_b3889_GTAProcess.exe+92B364", "libcef.dll+72402FC", "FiveM_b3889_GTAProcess.exe+1028"]);
        assert_eq!((i.windows.as_deref(), i.cpus, i.uptime), (Some("Windows 10.0.26100"), Some(16), Some(96)));
        let t = modules_text(&i, "a.dmp", 100, &|s| s.to_string());
        assert!(t.contains("exception: code 0xC0000005 access violation, at libcef.dll+E0D01, thread 27660"), "{t}");
        assert!(t.contains("process: FiveM_b3889_GTAProcess.exe 2.0.0.38410, game build 3889, ran 96 s"), "{t}");
        assert!(t.contains("video driver: nvwgf2umx.dll 32.0.15.9144"), "{t}");
        assert!(t.contains("C:\\Users\\Ivan\\AppData"), "the caller cleans it: {t}");
        // cleaned: paths lose the name, versions stay (libcef's «1.0.0.0» looks like an IPv4 address)
        let c = modules_text(&i, "a.dmp", 100, &|s| clean(s, &cleaner()));
        assert!(c.contains("C:\\Users\\%USER%\\AppData") && !c.contains("Ivan"), "{c}");
        assert!(c.contains(" 1.0.0.0 ") && !c.contains("<ip>"), "{c}");
    }

    /// Broken and cut dumps: never a panic (the launcher is built with panic = "abort").
    #[test]
    fn broken_dumps_never_panic() {
        let d = fake_dump();
        for n in 0..d.len() {
            let _ = read_dump(&d[..n]);
        }
        let mut seed = 0x2111_2026u64;
        for _ in 0..3000 {
            let mut x = d.clone();
            for _ in 0..4 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let at = (seed >> 33) as usize % x.len();
                x[at] = (seed >> 13) as u8;
            }
            let _ = read_dump(&x);
        }
        // every u32 field set to the extremes
        for at in (0..d.len().min(2048)).step_by(4) {
            for v in [0u32, 0xFFFF_FFFF, 0x8000_0000] {
                let mut x = d.clone();
                x[at..at + 4].copy_from_slice(&v.to_le_bytes());
                let _ = read_dump(&x);
            }
        }
        assert!(read_dump(b"MDMP").is_none() || read_dump(b"MDMP").is_some());
        assert!(read_dump(b"not a dump at all").is_none());
    }

    // ------------------------------------------------ zips

    fn fake_pack(dmp_name: &str, crashometry: &str, log: &str) -> Vec<u8> {
        zip(&[
            Part::pack("CitizenFX_log_2026-11-20T073136.log", log.as_bytes()),
            Part::pack(dmp_name, &fake_dump()),
            Part::pack("crashometry.json", crashometry.as_bytes()),
        ])
    }

    #[test]
    fn zip_round_trip_and_broken_zips() {
        let z = fake_pack("4d50eb14-b5df-49fb-a5de-d2ad4b8e54dc.dmp", r#"{"crash_hash":"x"}"#, "log line\n");
        let mut read = |at: u64, n: usize| z.get(at as usize..at as usize + n).map(<[u8]>::to_vec);
        let entries = zip_entries(z.len() as u64, &mut read).unwrap();
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["CitizenFX_log_2026-11-20T073136.log", "4d50eb14-b5df-49fb-a5de-d2ad4b8e54dc.dmp", "crashometry.json"]);
        let dmp = &entries[1];
        assert_eq!(dmp.method, 8, "the dump is deflated");
        let local = |e: &ZipEntry| {
            let skip = 30 + rd16(&z, e.offset as usize + 26).unwrap() as usize + rd16(&z, e.offset as usize + 28).unwrap() as usize;
            z[e.offset as usize + skip..e.offset as usize + skip + e.csize as usize].to_vec()
        };
        assert_eq!(unpack(dmp, &local(dmp), 10 << 20).unwrap(), fake_dump());
        assert_eq!(unpack(dmp, &local(dmp), 100), None, "over the limit");
        let mut bad = local(dmp);
        bad[10] ^= 0xFF;
        assert_eq!(unpack(dmp, &bad, 10 << 20), None, "a damaged entry");
        // cut and damaged zips: None, no panic
        for n in 0..z.len() {
            let mut r = |at: u64, k: usize| z[..n].get(at as usize..at as usize + k).map(<[u8]>::to_vec);
            let _ = zip_entries(n as u64, &mut r);
        }
        let mut seed = 7u64;
        for _ in 0..2000 {
            let mut x = z.clone();
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let at = (seed >> 33) as usize % x.len();
            x[at] = (seed >> 7) as u8;
            let mut r = |a: u64, k: usize| x.get(a as usize..a as usize + k).map(<[u8]>::to_vec);
            if let Some(es) = zip_entries(x.len() as u64, &mut r) {
                for e in es {
                    let skip = 30usize;
                    if let Some(raw) = x.get(e.offset as usize + skip..(e.offset as usize + skip).saturating_add(e.csize as usize)) {
                        let _ = unpack(&e, raw, 1 << 20);
                    }
                }
            }
        }
        // Windows' own tar reads our zip (deflate and stored)
        #[cfg(windows)]
        {
            let dir = std::env::temp_dir().join(format!("coldz-crashzip-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("out")).unwrap();
            std::fs::write(dir.join("r.zip"), &z).unwrap();
            let ok = std::process::Command::new("tar").arg("-xf").arg(dir.join("r.zip")).arg("-C").arg(dir.join("out")).status().map(|s| s.success()).unwrap_or(false);
            if ok {
                assert_eq!(std::fs::read(dir.join("out").join("crashometry.json")).unwrap(), br#"{"crash_hash":"x"}"#);
                assert_eq!(std::fs::read(dir.join("out").join("4d50eb14-b5df-49fb-a5de-d2ad4b8e54dc.dmp")).unwrap(), fake_dump());
            } else {
                eprintln!("tar not available");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    // ------------------------------------------------ the log

    const LOG: &str = "[       593] [    DumpServer]                24876/ DumpServer is active and waiting.\n\
[       750] [         FiveM]             MainThrd/ hello from \"C:\\Users\\Ivan\\AppData\\Local\\FiveM\\FiveM.exe\" \"fivem://connect/92.118.11.28:28683\"\n\
[      9937] [   ROSLauncher]             MainThrd/ Account id 264241266\n\
[     24500] [b3889_GTAProce]  UV loop: httpClient/ OnConnectionProgress: Requesting authentication ticket...\n\
[     30000] [b3889_GTAProce]             MainThrd/ connecting with token=AbCdEf123456789xyz&x=1 peer 85.140.1.2:443 v6 2a00:1fa0:4216:5e0c:1d2c:9f1:aa22:1\n\
[     31000] [b3889_GTAProce]             MainThrd/ identifiers steam:110000112345678 license:0123456789abcdef0123456789abcdef01234567 discord:123456789012345678\n\
[     32000] [b3889_GTAProce]             MainThrd/ jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U at 10:22:33\n\
[  11159359] [    DumpServer]                27660/ Process crash captured. Crash dialog content:\n\
[  11159359] [    DumpServer]                27660/ libcef.dll+E0D01\n\
[  11159359] [    DumpServer]                27660/ An error at libcef.dll+E0D01 caused FiveM to stop working.\n\
[  11159359] [    DumpServer]                27660/ Stack trace:\n\
[  11159359] [    DumpServer]                27660/   libcef.dll+E0D01\n\
[  11159359] [    DumpServer]                27660/   libcef.dll+72402FC\n\
[  11159359] [    DumpServer]                27660/ \n\
[  11160000] [    DumpServer]                27660/ after the crash\n";

    #[test]
    fn log_part_ends_with_the_crash_and_knows_fivems_place() {
        let p = log_part(LOG, 300);
        assert_eq!(p.hash.as_deref(), Some("libcef.dll+E0D01"));
        assert_eq!(p.stack, ["libcef.dll+E0D01", "libcef.dll+72402FC"]);
        assert!(p.text.ends_with("27660/   libcef.dll+72402FC"), "{}", p.text);
        assert!(!p.text.contains("after the crash"));
        assert_eq!(p.lines, 13);
        // the last 5 lines only
        let p5 = log_part(LOG, 5);
        assert_eq!(p5.lines, 5);
        assert!(p5.text.starts_with("[  11159359] [    DumpServer]                27660/ libcef.dll+E0D01"), "{}", p5.text);
        // no crash block: the end of the log
        let none = log_part("a\nb\nc\n", 2);
        assert_eq!((none.text.as_str(), none.hash), ("b\nc", None));
        // a very long line is cut at a letter
        let long = format!("{}\n", "я".repeat(3000));
        assert!(log_part(&long, 10).text.len() <= LINE_MAX);
        assert_eq!(log_time("CitizenFX_log_2026-11-20T175337.log"), Some(1_795_197_217));
        assert_eq!(log_time("CitizenFX_log_2026-13-20T175337.log"), None);
        assert_eq!(log_time("CitizenFX_log_x.log"), None);
        assert_eq!(log_time("other.log"), None);
        assert_eq!(crate::fivem::utc_stamp(1_795_197_217), "2026-11-20 17:53:37 UTC");
    }

    fn cleaner() -> Clean {
        Clean {
            scrub: support::Scrub { user: vec!["Ivan".into()], pc: vec!["DESKTOP-ABC123".into()], keep_ips: vec!["92.118.11.28".into()] },
            salt: "s4lt".into(),
            rockstar: vec!["89029823".into()],
        }
    }

    #[test]
    fn clean_hides_private_parts_and_keeps_the_rest() {
        let c = cleaner();
        let out = clean(LOG, &c);
        assert!(out.contains("C:\\Users\\%USER%\\AppData\\Local\\FiveM"), "{out}");
        assert!(out.contains("fivem://connect/92.118.11.28:28683"), "our server stays: {out}");
        assert!(out.contains(&format!("Account id rid:{}", hash_id("s4lt", "264241266"))), "{out}");
        assert!(!out.contains("264241266"));
        assert!(out.contains("Requesting authentication ticket..."), "{out}");
        assert!(out.contains("token=<hidden>&x=1"), "{out}");
        assert!(out.contains("peer <ip>:443"), "{out}");
        assert!(out.contains("v6 <ip6>"), "{out}");
        assert!(out.contains("steam:<id> license:<id> discord:<id>"), "{out}");
        assert!(out.contains("jwt <hidden> at 10:22:33"), "times stay: {out}");
        assert!(out.contains("libcef.dll+E0D01") && out.contains("libcef.dll+72402FC"), "{out}");
        for gone in ["Ivan", "AbCdEf123456789xyz", "85.140.1.2", "2a00:1fa0", "110000112345678", "eyJhbGci"] {
            assert!(!out.contains(gone), "{gone}: {out}");
        }
        // the Rockstar ID from crashometry, wherever it stands; the same salt gives the same word
        let t = clean("rid 89029823 x189029823 PC DESKTOP-ABC123", &c);
        assert_eq!(t, format!("rid rid:{} x189029823 PC %PC%", hash_id("s4lt", "89029823")));
        assert_eq!(hash_id("s4lt", "89029823").len(), 12);
        assert_ne!(hash_id("other", "89029823"), hash_id("s4lt", "89029823"));
        // secrets in other forms
        assert_eq!(hide_secrets(r#""password": "hunter22", "sv_licenseKey" "abcdef123456""#), r#""password": "<hidden>", "sv_licenseKey" "abcdef123456""#);
        assert_eq!(hide_secrets("Authorization: Bearer abc.def.ghi1234"), "Authorization: Bearer <hidden>");
        assert_eq!(hide_secrets("set adhesive_cdnKey=0123456789ab"), "set adhesive_cdnKey=<hidden>");
        assert_eq!(hide_secrets("key: F8"), "key: F8", "short values stay");
        assert_eq!(hide_secrets("crash_hash_key lamp-india-queen"), "crash_hash_key lamp-india-queen");
        assert_eq!(hide_sids("C:\\$Recycle.Bin\\S-1-5-21-123-456-789-1001\\x"), "C:\\$Recycle.Bin\\S-1-5-21-<sid>\\x");
        // hashes of files, PDB ids, versions, times: not hidden
        for keep in ["cache:v1:730ca6fbd14b88607ef40c12eeeae8191ad64cfe", "1556A7BB2CBF41078D900E30AF76D1E3E", "1.0.0.38410", "12:34:56", "aa:bb:cc:dd:ee:ff", "std::string", "GTA5_b3889.exe+92B364"] {
            assert_eq!(clean(keep, &c), keep);
        }
        // any profile folder, not only this PC's user (a crash pack from another account), in all spellings
        assert_eq!(
            clean("C:\\Users\\Юрка\\AppData x C:/Users/John Smith/Docs \"C:\\\\Users\\\\Petr\\\\x\" C:\\Users\\Public\\y c:\\users\\Default\\z", &c),
            "C:\\Users\\%USER%\\AppData x C:/Users/%USER%/Docs \"C:\\\\Users\\\\%USER%\\\\x\" C:\\Users\\Public\\y c:\\users\\Default\\z"
        );
        // never a panic on odd texts
        for t in ["", ":", "::", "ẞİ token=", "token=", "Bearer", "S-1-5-21-", "eyJ", "steam:", "Account id ", "я:я:я", "\u{0}\u{1}"] {
            let _ = clean(t, &c);
        }
    }

    #[test]
    fn crashometry_is_cleaned_and_the_rockstar_id_hashed() {
        let raw = br#"{"crash_hash":"fivem.exe+1028","crash_hash_id":2892607512,"crash_hash_key":"blue-pizza-jersey","crashometry_RockstarId":"89029823","crashometry_gpu_name":"NVIDIA GeForce RTX 5070","crashometry_last_server_url":"http://92.118.11.28:28683/","crashometry_other_server":"http://5.6.7.8:30120/","nested":{"a":"C:\\Users\\Ivan\\x"}}"#;
        let (text, ids, hash, key) = crashometry_from(raw, &cleaner()).unwrap();
        assert_eq!((hash.as_str(), key.as_str()), ("fivem.exe+1028", "blue-pizza-jersey"));
        assert_eq!(ids, ["89029823"]);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["crashometry_RockstarId"], format!("rid:{}", hash_id("s4lt", "89029823")));
        assert_eq!(v["crash_hash_id"], 2892607512u64);
        assert_eq!(v["crashometry_gpu_name"], "NVIDIA GeForce RTX 5070");
        assert_eq!(v["crashometry_last_server_url"], "http://92.118.11.28:28683/");
        assert_eq!(v["crashometry_other_server"], "http://<ip>:30120/");
        assert!(!text.contains("89029823") && !text.contains("Ivan"), "{text}");
        assert!(crashometry_from(b"[1,2]", &cleaner()).is_none());
        assert!(crashometry_from(b"{oops", &cleaner()).is_none());
    }

    // ------------------------------------------------ switches

    #[test]
    fn news_json_switches_the_reports() {
        use serde_json::json;
        let built_in = Switch { enabled: true, url: "https://cdn.coldz.vip/crash/v1/report".into(), key: "coldz-crash-test-key".into(), dump: true };
        let on = setup(&Feed::default(), &built_in);
        assert!(on.enabled && on.dump);
        // the kill switch
        let off = setup(&Feed::of(&json!({ "crashReport": { "enabled": false } })), &built_in);
        assert!(!off.enabled);
        // only the dump off; another receiver and key
        let s = setup(&Feed::of(&json!({ "crashReport": { "dump": false, "url": "https://coldz.vip/crash/v1/report", "key": "new-key-2026" } })), &built_in);
        assert!(s.enabled && !s.dump);
        assert_eq!((s.url.as_str(), s.key.as_str()), ("https://coldz.vip/crash/v1/report", "new-key-2026"));
        // bad values: the built-in ones; not an object: as if missing
        for feed in [json!({ "crashReport": { "url": "http://x/report", "key": "a b", "enabled": "no" } }), json!({ "crashReport": true }), json!({})] {
            assert_eq!(setup(&Feed::of(&feed), &built_in), on, "{feed}");
        }
        // switched on by news.json while the built-in is off; no good address: off anyway
        let dark = Switch { enabled: false, ..built_in.clone() };
        assert!(setup(&Feed::of(&json!({ "crashReport": { "enabled": true } })), &dark).enabled);
        let no_url = Switch { url: String::new(), ..built_in.clone() };
        assert!(!setup(&Feed::default(), &no_url).enabled);
        assert!(!setup(&Feed::default(), &Switch::default()).enabled, "missing in launcher.config.json: off");
    }

    // ------------------------------------------------ finding, preparing and packing on a fake FiveM

    fn touch_at(p: &Path, bytes: &[u8], secs_ago: u64) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
        let t = std::time::SystemTime::now() - Duration::from_secs(secs_ago);
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    pub(crate) fn fake_fivem(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("coldz-crashreport-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let app = root.join("FiveM").join("FiveM.app");
        let desk = root.join("Desktop");
        std::fs::create_dir_all(app.join("logs")).unwrap();
        std::fs::create_dir_all(&desk).unwrap();
        (root, app, desk)
    }

    #[test]
    fn finds_new_crashes_and_packs_them_within_the_limits() {
        let (root, app, desk) = fake_fivem("scan");
        let now = crate::netcheck::unix_now();
        let crashes = app.join("crashes");
        // an old one, a fresh one, one still being written, a strange name
        touch_at(&crashes.join("old.dmp"), &fake_dump(), 4 * 24 * 3600);
        touch_at(&crashes.join("aaaa-1111.dmp"), &fake_dump(), 3600);
        touch_at(&crashes.join("bbbb-2222.dmp"), &fake_dump(), 5);
        touch_at(&crashes.join("x y.dmp"), &fake_dump(), 3600);
        touch_at(&crashes.join("aaaa-1111.dmp.gamelog"), b"game log", 3600);
        // FiveM's crash pack on the desktop for another crash (its dump is gone from FiveM.app)
        let cfx_pack = fake_pack("cccc-3333.dmp", r#"{"crash_hash":"GTA5_b3889.exe+92B364","crash_hash_key":"hotel-jersey-london","crashometry_RockstarId":"89029823"}"#, LOG);
        touch_at(&desk.join("CfxCrashDump_2026_11_20_07_34_36.zip"), &cfx_pack, 1800);
        // the session log of the first crash: started 2 hours before it
        let start = crate::fivem::utc_stamp(now - 3600 - 7200);
        let name = format!("CitizenFX_log_{}T{}.log", &start[..10], start[11..19].replace(':', ""));
        touch_at(&app.join("logs").join(&name), LOG.as_bytes(), 3590);
        // FiveM started again 55 s after that crash: its new log must not be taken
        let next = crate::fivem::utc_stamp(now - 3600 + 55);
        let next_name = format!("CitizenFX_log_{}T{}.log", &next[..10], next[11..19].replace(':', ""));
        // (that session ended before the next crash, so the next crash has no log here: it takes the pack's)
        touch_at(&app.join("logs").join(&next_name), b"[  1] [  FiveM]  MainThrd/ a new session\n", 3500);

        let found = scan(Some(&crashes), &[desk.clone()], now);
        assert_eq!(found.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["cccc-3333.dmp", "aaaa-1111.dmp"]);
        assert!(found[0].pack.is_some() && found[0].dump.is_none());
        assert!(found[1].dump.is_some() && found[1].pack.is_none());
        // the question: the newest one, two in all; answered — nothing; «Не спрашивать» — nothing
        let state = State::default();
        let (c, n) = pending(&found, &state).unwrap();
        assert_eq!((c.id.as_str(), n), ("cccc-3333.dmp", 2));
        assert!(pending(&found, &State { seen: c.at, ..Default::default() }).is_none());
        assert_eq!(pending(&found, &State { seen: found[1].at, ..Default::default() }).map(|x| x.1), Some(1));
        assert!(pending(&found, &State { never: true, ..Default::default() }).is_none());
        assert_eq!(quick_hash(&found[0], Some(&app.join("logs"))), ("GTA5_b3889.exe+92B364".into(), "hotel-jersey-london".into()));
        assert_eq!(quick_hash(&found[1], Some(&app.join("logs"))).0, "libcef.dll+E0D01");

        let ctx = Ctx { launcher: "0.5.6".into(), lang: "ru".into(), clean: cleaner(), windows: "Windows 11 Pro".into(), gpus: vec![Gpu { name: "NVIDIA GeForce RTX 5070".into(), driver: "32.0.15.9144".into(), date: "10-20-2026".into() }], ram_gb: Some(32), logs: Some(app.join("logs")), new_count: 2 };
        // the crash from FiveM.app: log + modules, no crashometry
        let p = prepare(&found[1], &ctx);
        assert_eq!((p.hash.as_str(), p.hash_from), ("libcef.dll+E0D01", "log"));
        assert_eq!(p.texts.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), [NAME_LOG, NAME_MODULES]);
        assert_eq!((p.log_name.as_str(), p.log_from), (name.as_str(), "logs"));
        let all: String = p.texts.iter().map(|t| t.1.clone()).collect();
        assert!(!all.contains("Ivan") && !all.contains("264241266"), "{all}");
        let pv = preview(&p, true);
        assert_eq!(pv.items.iter().map(|i| i.key).collect::<Vec<_>>(), ["log", "modules", "info"]);
        assert_eq!(pv.dump, Some(fake_dump().len() as u64));
        assert_eq!(preview(&p, false).dump, None);
        let small = pack(&p, false, true).unwrap();
        assert!(!small.dump_attached && small.bytes.len() < LIMIT_SMALL);
        let with = pack(&p, true, true).unwrap();
        assert!(with.dump_attached && with.bytes.len() > small.bytes.len() && with.bytes.len() <= LIMIT_DUMP);
        let off = pack(&p, true, false).unwrap();
        assert_eq!((off.dump_attached, off.dump_skipped), (false, "off"));
        // what the receiver gets: our names, report.json says what is in
        let mut read = |at: u64, n: usize| with.bytes.get(at as usize..at as usize + n).map(<[u8]>::to_vec);
        let es = zip_entries(with.bytes.len() as u64, &mut read).unwrap();
        assert_eq!(es.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), [NAME_LOG, NAME_MODULES, NAME_REPORT, "aaaa-1111.dmp"]);
        let rep = es.iter().find(|e| e.name == NAME_REPORT).unwrap();
        let skip = 30 + rep.name.len();
        let raw = &with.bytes[rep.offset as usize + skip..rep.offset as usize + skip + rep.csize as usize];
        let v: serde_json::Value = serde_json::from_slice(&unpack(rep, raw, 1 << 20).unwrap()).unwrap();
        assert_eq!(v["format"], 1);
        assert_eq!(v["crash"]["hash"], "libcef.dll+E0D01");
        assert_eq!(v["game"]["build"], 3889);
        assert_eq!(v["dump"]["attached"], true);
        assert_eq!(v["gpus"][0]["name"], "NVIDIA GeForce RTX 5070");
        assert_eq!(v["drivers"][0]["version"], "32.0.15.9144");
        assert_eq!(v["reporter"], hash_id("s4lt", "reporter"));

        // the crash only in FiveM's pack: crashometry, the pack's log and dump
        let q = prepare(&found[0], &ctx);
        assert_eq!((q.hash.as_str(), q.key.as_str(), q.hash_from), ("GTA5_b3889.exe+92B364", "hotel-jersey-london", "crashometry"));
        assert_eq!(q.texts.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), [NAME_CRASHOMETRY, NAME_LOG, NAME_MODULES]);
        assert_eq!(q.log_from, "pack");
        assert!(!q.texts.iter().any(|t| t.1.contains("89029823")));
        let qp = pack(&q, true, true).unwrap();
        assert!(qp.dump_attached, "the pack's deflated dump is copied as it is");
        let mut read = |at: u64, n: usize| qp.bytes.get(at as usize..at as usize + n).map(<[u8]>::to_vec);
        let es = zip_entries(qp.bytes.len() as u64, &mut read).unwrap();
        let dmp = es.iter().find(|e| e.name == "cccc-3333.dmp").unwrap();
        let skip = 30 + dmp.name.len();
        let raw = &qp.bytes[dmp.offset as usize + skip..dmp.offset as usize + skip + dmp.csize as usize];
        assert_eq!(unpack(dmp, raw, 1 << 24).unwrap(), fake_dump());

        // a crash without anything readable: still a small report
        let empty = Crash { id: "zzzz.dmp".into(), at: now - 100, dump: None, pack: None };
        let e = prepare(&empty, &Ctx::default());
        assert!(e.texts.is_empty() && e.hash.is_empty());
        assert!(pack(&e, true, true).unwrap().dump_skipped == "unreadable");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_huge_log_gets_shorter_and_a_huge_dump_stays_home() {
        let (root, app, _desk) = fake_fivem("limits");
        let now = crate::netcheck::unix_now();
        // random bytes do not compress: a dump over 20 MB stays out, the report goes without it
        let mut seed = 99u64;
        let big: Vec<u8> = (0..(21 << 20)).map(|_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u8
        }).collect();
        touch_at(&app.join("crashes").join("big.dmp"), &big, 600);
        let c = scan(Some(&app.join("crashes")), &[], now).remove(0);
        let mut p = prepare(&c, &Ctx::default());
        // and a log part of 3 MB of random text (does not compress either)
        let mut s2 = 5u64;
        let noise: String = (0..3_000_000u32)
            .map(|i| {
                if i % 200 == 199 {
                    return '\n';
                }
                s2 = s2.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (b'!' + ((s2 >> 33) % 90) as u8) as char
            })
            .collect();
        p.texts.push((NAME_LOG.to_string(), noise));
        let r = pack(&p, true, true).unwrap();
        assert_eq!((r.dump_attached, r.dump_skipped), (false, "too_big"));
        assert!(r.bytes.len() <= LIMIT_SMALL, "{}", r.bytes.len());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn outbox_keeps_three_fresh_reports_with_our_names() {
        let now = 2_000_000_000u64;
        let p = |file: &str, at: u64, tries: u32| Pending { file: file.into(), at, tries, ..Default::default() };
        let s = State {
            outbox: vec![
                p("crash-1.zip", now - 100, 0),
                p("crash-2.zip", now - 8 * 24 * 3600, 0),
                p("crash-3.zip", now - 50, 3),
                p("..\\evil.zip", now - 10, 0),
                p("crash-4.zip", now - 40, 1),
                p("crash-5.zip", now - 30, 0),
                p("crash-6.zip", now - 20, 2),
            ],
            ..Default::default()
        };
        let (send, drop) = outbox_plan(&s, now);
        let names = |l: &[Pending]| l.iter().map(|x| x.file.clone()).collect::<Vec<_>>();
        assert_eq!(names(&send), ["crash-5.zip", "crash-6.zip"]);
        assert_eq!(drop.len(), 5);
        assert!(safe_file_name("crash-20261121-101500-4d50eb14-b5df-49fb-a5de-d2ad4b8e54dc.zip"));
        assert!(!safe_file_name("crash-../x.zip") && !safe_file_name("x.zip"));
        let c = Crash { id: "4d50eb14-b5df-49fb-a5de-d2ad4b8e54dc.dmp".into(), at: 0, dump: None, pack: None };
        assert_eq!(outbox_name(&c, 1_795_197_217), "crash-20261120-175337-4d50eb14-b5df-49fb-a5de-d2ad4b8e54dc.zip");
        assert!(safe_file_name(&outbox_name(&c, 1_795_197_217)));
        let dir = std::env::temp_dir().join(format!("coldz-outbox-{}", std::process::id()));
        let f = outbox_put(&dir, "crash-x.zip", b"PK").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"PK");
        assert!(!dir.join("crash-x.zip.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------ sending to a fake receiver

    #[test]
    fn send_to_a_fake_receiver() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            async fn receiver(status: u16, body: &'static str) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = l.local_addr().unwrap().port();
                let t = tokio::spawn(async move {
                    let (mut s, _) = l.accept().await.unwrap();
                    let mut req = Vec::new();
                    let mut buf = vec![0u8; 65536];
                    loop {
                        let n = s.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        req.extend_from_slice(&buf[..n]);
                        let text = String::from_utf8_lossy(&req).to_string();
                        if let Some(h) = text.find("\r\n\r\n") {
                            let len = text[..h].lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                            if req.len() >= h + 4 + len {
                                break;
                            }
                        }
                    }
                    let reply = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = s.write_all(reply.as_bytes()).await;
                    req
                });
                (format!("http://127.0.0.1:{port}/crash/v1/report"), t)
            }
            let (url, t) = receiver(200, r#"{"ok":true,"id":"2026-11-21/101500-ab12cd34"}"#).await;
            assert_eq!(send(&url, "coldz-crash-test-key", b"PK-report".to_vec()).await, Sent::Ok("2026-11-21/101500-ab12cd34".into()));
            let req = String::from_utf8_lossy(&t.await.unwrap()).to_string();
            assert!(req.starts_with("POST /crash/v1/report HTTP/1.1"), "{req}");
            let low = req.to_ascii_lowercase();
            assert!(low.contains("content-type: application/zip") && low.contains("x-coldz-key: coldz-crash-test-key") && low.contains("x-coldz-format: 1"), "{req}");
            assert!(req.ends_with("PK-report"));
            let (url, _t) = receiver(503, "busy").await;
            assert_eq!(send(&url, "k", vec![1]).await, Sent::Retry("HTTP 503".into()));
            let (url, _t) = receiver(429, "slow down").await;
            assert!(matches!(send(&url, "k", vec![1]).await, Sent::Retry(_)));
            let (url, _t) = receiver(413, r#"{"error":"too big"}"#).await;
            assert!(matches!(send(&url, "k", vec![1]).await, Sent::Refused(w) if w.starts_with("HTTP 413")));
            let (url, _t) = receiver(301, "").await;
            assert!(matches!(send(&url, "k", vec![1]).await, Sent::Refused(_)), "redirects are not followed");
            // nobody listens: again later
            let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            assert!(matches!(send(&format!("http://127.0.0.1:{free}/x"), "k", vec![1]).await, Sent::Retry(_)));
        });
        assert_eq!(classify(200, "not json"), Sent::Ok(String::new()));
        assert!(timeout_for(20 << 20) <= Duration::from_secs(330) && timeout_for(1000) == Duration::from_secs(30));
    }

    #[test]
    fn looks_at_this_pc_without_errors() {
        eprintln!("gpus: {:?}", gpus());
        eprintln!("ram: {:?} GB", ram_gb());
        eprintln!("downloads: {:?}", downloads_dir());
        if cfg!(windows) {
            assert!(ram_gb().unwrap_or(0) > 0);
        }
    }

    /// By hand: `cargo test real_crash_report_dry_run -- --ignored --nocapture` (and `COLDZ_PACK_DIR=<folder with a
    /// CfxCrashDump_*.zip>`). Makes the reports of the newest crashes on this PC in the temp folder, as the player would
    /// send them (cleaned, with the dump); sends nothing.
    #[test]
    #[ignore]
    fn real_crash_report_dry_run() {
        let (found, _) = crate::fivem::search(&crate::fivem::System { manual: None });
        let app = found.map(|f| support::app_dir(&f.path));
        let dirs: Vec<PathBuf> = std::env::var_os("COLDZ_PACK_DIR").map(PathBuf::from).into_iter().collect();
        let crashes = scan(app.as_ref().map(|a| a.join("crashes")).as_deref(), &dirs, crate::netcheck::unix_now());
        let out = std::env::temp_dir().join("coldz-crash-dry-run");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let ctx = Ctx {
            launcher: "dry-run".into(),
            lang: "ru".into(),
            clean: Clean { scrub: support::Scrub::of_this_pc(vec!["92.118.11.28".into()]), salt: "dry-run".into(), rockstar: vec![] },
            windows: support::windows_version(),
            gpus: gpus(),
            ram_gb: ram_gb(),
            logs: app.as_ref().map(|a| a.join("logs")),
            new_count: crashes.len(),
        };
        for c in crashes.iter().take(3) {
            let t = std::time::Instant::now();
            let p = prepare(c, &ctx);
            let small = pack(&p, false, true).unwrap();
            let with = pack(&p, true, true).unwrap();
            eprintln!(
                "{} {}: {} ({}), {:?}; без снимка {} Б, со снимком {} Б ({}{}), {:.1} с",
                c.id,
                crate::fivem::utc_stamp(c.at),
                p.hash,
                p.hash_from,
                p.texts.iter().map(|x| (x.0.clone(), x.1.len())).collect::<Vec<_>>(),
                small.bytes.len(),
                with.bytes.len(),
                with.dump_attached,
                with.dump_skipped,
                t.elapsed().as_secs_f64()
            );
            let dir = out.join(c.id.trim_end_matches(".dmp"));
            std::fs::create_dir_all(&dir).unwrap();
            for (name, text) in &p.texts {
                std::fs::write(dir.join(name), text).unwrap();
            }
            std::fs::write(dir.join(NAME_REPORT), report_json(&p, with.dump_attached, with.dump_skipped)).unwrap();
            std::fs::write(dir.join("report.zip"), &with.bytes).unwrap();
        }
        eprintln!("{}", out.display());
    }
}
