//! 0.5.4: the game file FiveM would otherwise download INSIDE the running game (README 7н).
//!
//! What FiveM does (citizenfx/fivem 5412ed86, `code/client/launcher/GameCache.cpp`, `components/glue/src/
//! CrossBuildSwitch.cpp`): FiveM runs the game exe of the "default" build (`DefaultBuild`, the server's
//! `sv_defaultGameBuild`, 3889 for COLDZ) and, when the server's content build is OLDER (`sv_enforceGameBuild` 3258),
//! mounts that build's `update.rpf` on top: the entry `override/update/update.rpf` with the checksum, address and size
//! of the older build's `update.rpf` from its built-in table (`g_entriesToLoadPerBuild[3258].at("update.rpf")`:
//! sha1 abf3a580…, `https://cfx-downloads.akamaized.net/prod/019febd6-…/update.rpf`, 1 423 288 320 bytes). The file
//! lives in `FiveM.app\data\game-storage\override+update+update.rpf_<sha1>`; a file of that name and exactly that size
//! counts as present (`CompareCacheDifferences`: no hash check). Missing, FiveM downloads it whole (deltas apply only to
//! a local file of the same name) with its own downloader, which starts over on every run (`Download.cpp`: `.tmp`
//! opened with "wb"). When FiveM is already in the game and has to switch builds, it does that download inside the game
//! (`PerformStateSwitch` → `UpdateGameCache`), and the game is closed ~96 s after it started: on a slow connection the
//! 1.4 GB never fit (20.11.2026, 5 crash dumps).
//!
//! With the right `CitizenFX.ini` (gamebuild.rs) FiveM downloads it in its start window instead. The launcher
//! additionally downloads it BEFORE FiveM starts, from the same official address, so a slow or broken connection does
//! not lose the progress: the part goes to `<name>.coldz-part` next to it (FiveM ignores the name: no `_<sha1>` at the
//! end), continues with an HTTP Range after a break or a restart of the launcher, and becomes `<name>` only after its
//! SHA-1 matched. The table (build → sha1, address, size) is FiveM's own: built into launcher.config.json
//! `gameStorage`, `gameStorage` in news.json wins; before downloading the launcher checks that the installed FiveM.exe
//! carries the same sha1 and address (FiveM changed its table: nothing is downloaded). Only addresses on Cfx.re's
//! download host are taken.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The only host the files are taken from (FiveM's own download host for game files).
pub const HOST_PREFIX: &str = "https://cfx-downloads.akamaized.net/";
/// FiveM's cache folder inside FiveM.app.
pub const STORAGE_DIR: [&str; 2] = ["data", "game-storage"];
/// Suffix of the part being downloaded (FiveM's own downloader uses ".tmp" and truncates it, so not that).
pub const PART: &str = ".coldz-part";
/// Free space kept on the disk beyond the file.
const SPACE_MARGIN: u64 = 512 * 1024 * 1024;
/// A response that sends nothing this long is a break (then the launcher asks again from where it stopped).
const STALL: Duration = Duration::from_secs(30);
/// Tries in a row without a single new byte before giving up.
const TRIES: u32 = 5;

/// One file of FiveM's table.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// The content build this `update.rpf` belongs to.
    pub build: u32,
    /// SHA-1, 40 hex characters (as in FiveM's table: also the end of the file name).
    pub sha1: String,
    pub url: String,
    pub size: u64,
}

impl Entry {
    /// Well-formed and on Cfx.re's download host.
    pub fn valid(&self) -> bool {
        self.build > 1
            && self.sha1.len() == 40
            && self.sha1.bytes().all(|b| b.is_ascii_hexdigit())
            && self.url.starts_with(HOST_PREFIX)
            && self.url.len() < 300
            && !self.url.contains(['"', ' ', '\\', '?', '#'])
            && (1 << 20..=8u64 << 30).contains(&self.size)
    }

    /// `override+update+update.rpf_<sha1>`: the name FiveM gives the override entry (`GetCacheFileName`).
    pub fn cache_name(&self) -> String {
        format!("override+update+update.rpf_{}", self.sha1.to_ascii_lowercase())
    }

    /// The same file from times FiveM ran this build's own exe: `update+update.rpf_<sha1>` (same bytes).
    pub fn plain_name(&self) -> String {
        format!("update+update.rpf_{}", self.sha1.to_ascii_lowercase())
    }
}

/// Usable entries only (from news.json or the built-in list).
pub fn clean(list: &[Entry]) -> Vec<Entry> {
    list.iter().filter(|e| e.valid()).take(16).cloned().collect()
}

/// The file FiveM will need for this server, if any: the content build is older than the exe build FiveM runs
/// (`GameCache.cpp`: `GetTargetGameBuild() < GetDefaultBuild()`, build 1 has no override). `ini` is CitizenFX.ini as it
/// will be (gamebuild.rs writes the server's `DefaultBuild`); without a server default its own `DefaultBuild` counts.
pub fn needed(w: &crate::gamebuild::Wanted, ini: &[u8], table: &[Entry]) -> Option<Entry> {
    let target = w.build;
    if target <= 1 {
        return None;
    }
    let persisted = w
        .default_build
        .or_else(|| crate::gamebuild::value(ini, crate::gamebuild::KEY_DEFAULT).and_then(|v| v.trim().parse().ok()).filter(|&d: &u32| d > 0))
        .unwrap_or(crate::gamebuild::CLIENT_DEFAULT_BUILD);
    let exe = target.max(persisted);
    if target >= exe {
        return None;
    }
    table.iter().find(|e| e.build == target && e.valid()).cloned()
}

/// Does the installed FiveM know this file (its table has the same sha1 and address)? Reads FiveM.exe (~5 MB).
pub fn fivem_knows(fivem_exe_bytes: &[u8], e: &Entry) -> bool {
    let has = |needle: &[u8]| !needle.is_empty() && fivem_exe_bytes.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle));
    has(e.sha1.as_bytes()) && has(e.url.as_bytes())
}

/// Where the files are.
#[derive(Debug, Clone, PartialEq)]
pub struct Paths {
    pub dir: PathBuf,
    pub done: PathBuf,
    pub part: PathBuf,
    pub plain: PathBuf,
}

pub fn paths(app_dir: &Path, e: &Entry) -> Paths {
    let dir = app_dir.join(STORAGE_DIR[0]).join(STORAGE_DIR[1]);
    Paths {
        done: dir.join(e.cache_name()),
        part: dir.join(format!("{}{PART}", e.cache_name())),
        plain: dir.join(e.plain_name()),
        dir,
    }
}

fn len_of(p: &Path) -> Option<u64> {
    std::fs::metadata(p).ok().filter(|m| m.is_file()).map(|m| m.len())
}

/// What is on the disk: None — the file is there (exactly its size, as FiveM checks); Some(bytes) — that much of it
/// downloaded so far (a part longer than the file counts as nothing).
pub fn missing(p: &Paths, e: &Entry) -> Option<u64> {
    if len_of(&p.done) == Some(e.size) {
        return None;
    }
    Some(len_of(&p.part).filter(|&n| n <= e.size).unwrap_or(0))
}

/// Why the download stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum Fail {
    /// The player pressed «Отменить» (the part stays).
    Cancelled,
    /// No connection, a break, a server error (the part stays).
    Network(String),
    /// The server sends another file than FiveM's table says (size): nothing more is downloaded.
    Remote(String),
    /// The whole file did not match its SHA-1: the part is deleted.
    Bad,
    /// Not enough free space: bytes needed.
    Space(u64),
    /// Could not write the file.
    Disk(String),
}

impl Fail {
    /// The key for the window (ui/i18n.js `storage_*`).
    pub fn key(&self) -> &'static str {
        match self {
            Fail::Cancelled => "cancelled",
            Fail::Network(_) => "network",
            Fail::Remote(_) => "remote",
            Fail::Bad => "bad",
            Fail::Space(_) => "space",
            Fail::Disk(_) => "disk",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Fail::Cancelled => "отменено".into(),
            Fail::Network(w) => format!("связь: {w}"),
            Fail::Remote(w) => format!("сервер отдаёт другой файл: {w}"),
            Fail::Bad => "SHA-1 не совпал, часть удалена".into(),
            Fail::Space(n) => format!("мало места: нужно {} МБ", n / 1_000_000),
            Fail::Disk(w) => format!("запись: {w}"),
        }
    }
}

/// Progress for the window: bytes so far, all bytes, "download" | "check".
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub phase: &'static str,
}

/// The player's «Отменить»: a flag and a wake-up for a download waiting on the network.
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    wake: tokio::sync::Notify,
}

impl Cancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// `Content-Range: bytes 100-199/1000` → 100.
fn range_start(v: &str) -> Option<u64> {
    v.trim().strip_prefix("bytes ")?.split('-').next()?.trim().parse().ok()
}

/// One request from `from` to the end, appended to the part. Ok(bytes written).
async fn fetch_once(
    client: &reqwest::Client,
    e: &Entry,
    part: &Path,
    from: u64,
    cancel: &Cancel,
    on_bytes: &mut (dyn FnMut(u64) + Send),
) -> Result<u64, Fail> {
    let mut req = client.get(&e.url);
    if from > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={from}-"));
    }
    let mut resp = tokio::select! {
        r = req.send() => r.map_err(|err| Fail::Network(short(&err)))?,
        _ = cancel.wake.notified() => return Err(Fail::Cancelled),
    };
    let status = resp.status().as_u16();
    let mut at = from;
    match status {
        206 => {
            let start = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(range_start);
            if start != Some(from) {
                return Err(Fail::Network(format!("Content-Range {start:?}, ждали {from}")));
            }
        }
        // the whole file (no Range asked, or the server ignored it): start the part over
        200 => at = 0,
        // the part is not a prefix of this file any more
        416 => {
            let _ = std::fs::remove_file(part);
            return Err(Fail::Network("HTTP 416".into()));
        }
        _ => return Err(Fail::Network(format!("HTTP {status}"))),
    }
    if let Some(len) = resp.content_length() {
        if at + len != e.size {
            return Err(Fail::Remote(format!("{} байт вместо {}", at + len, e.size)));
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(at == 0)
        .append(at > 0)
        .open(part)
        .map_err(|err| Fail::Disk(err.to_string()))?;
    let mut written = 0u64;
    loop {
        if cancel.is_set() {
            return Err(Fail::Cancelled);
        }
        let chunk = tokio::select! {
            c = resp.chunk() => c.map_err(|err| Fail::Network(short(&err)))?,
            _ = tokio::time::sleep(STALL) => return Err(Fail::Network(format!("нет данных {} с", STALL.as_secs()))),
            _ = cancel.wake.notified() => return Err(Fail::Cancelled),
        };
        let Some(chunk) = chunk else { break };
        if at + written + chunk.len() as u64 > e.size {
            return Err(Fail::Remote("больше, чем в таблице FiveM".into()));
        }
        file.write_all(&chunk).map_err(|err| Fail::Disk(err.to_string()))?;
        written += chunk.len() as u64;
        on_bytes(at + written);
    }
    file.flush().map_err(|err| Fail::Disk(err.to_string()))?;
    Ok(written)
}

fn short(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "таймаут".into()
    } else if err.is_connect() {
        "нет соединения".into()
    } else {
        "обрыв".into()
    }
}

/// SHA-1 of a file, reporting the bytes read.
pub fn sha1_file(path: &Path, mut on_bytes: impl FnMut(u64)) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
        total += n as u64;
        on_bytes(total);
    }
    Ok(ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads (or continues) the file into FiveM's cache: part, SHA-1, rename. `free_space` — free bytes on that disk
/// (None: unknown, not checked). Progress at most ~5 times a second.
pub async fn run(
    client: reqwest::Client,
    app_dir: PathBuf,
    e: Entry,
    free_space: Option<u64>,
    cancel: Arc<Cancel>,
    progress: impl Fn(Progress) + Send + Sync + 'static,
) -> Result<(), Fail> {
    let p = paths(&app_dir, &e);
    std::fs::create_dir_all(&p.dir).map_err(|err| Fail::Disk(err.to_string()))?;
    let Some(mut have) = missing(&p, &e) else { return Ok(()) };
    // FiveM once ran this build's own exe: the same bytes are already there under the other name (checked below)
    if have == 0 && len_of(&p.plain) == Some(e.size) {
        let _ = std::fs::remove_file(&p.part);
        if std::fs::hard_link(&p.plain, &p.part).is_ok() {
            have = e.size;
        }
    }
    if let Some(free) = free_space {
        let need = e.size - have + SPACE_MARGIN;
        if have < e.size && free < need {
            return Err(Fail::Space(need));
        }
    }
    let progress = Arc::new(progress);
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut report = {
        let progress = progress.clone();
        let total = e.size;
        move |done: u64, phase: &'static str, force: bool| {
            if force || last.elapsed() >= Duration::from_millis(200) {
                last = Instant::now();
                progress(Progress { done, total, phase });
            }
        }
    };
    report(have, "download", true);
    let mut fails = 0;
    while have < e.size {
        if cancel.is_set() {
            return Err(Fail::Cancelled);
        }
        let mut on_bytes = |n: u64| report(n, "download", false);
        match fetch_once(&client, &e, &p.part, have, &cancel, &mut on_bytes).await {
            Ok(_) => {}
            Err(Fail::Network(why)) => {
                let now = len_of(&p.part).unwrap_or(0);
                let moved = now > have;
                fails = if moved { 1 } else { fails + 1 };
                if fails >= TRIES {
                    return Err(Fail::Network(why));
                }
                // a break after new bytes: ask again at once; nothing came: wait a little
                let pause = if moved { Duration::from_millis(300) } else { Duration::from_secs(2) };
                tokio::select! {
                    _ = tokio::time::sleep(pause) => {}
                    _ = cancel.wake.notified() => return Err(Fail::Cancelled),
                }
            }
            Err(other) => return Err(other),
        }
        have = len_of(&p.part).filter(|&n| n <= e.size).unwrap_or(0);
    }
    report(0, "check", true);
    let part = p.part.clone();
    let sum = tokio::task::spawn_blocking(move || sha1_file(&part, |n| report(n, "check", false)))
        .await
        .map_err(|err| Fail::Disk(err.to_string()))?
        .map_err(|err| Fail::Disk(err.to_string()))?;
    if !sum.eq_ignore_ascii_case(&e.sha1) {
        let _ = std::fs::remove_file(&p.part);
        return Err(Fail::Bad);
    }
    // FiveM got it meanwhile (it ran by itself): keep its file
    if len_of(&p.done) == Some(e.size) {
        let _ = std::fs::remove_file(&p.part);
        return Ok(());
    }
    std::fs::rename(&p.part, &p.done).map_err(|err| Fail::Disk(err.to_string()))?;
    progress(Progress { done: e.size, total: e.size, phase: "check" });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamebuild::Wanted;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// FiveM's table, 5412ed86 (GameCache.cpp, build 3258, "update.rpf").
    fn coldz_entry() -> Entry {
        Entry {
            build: 3258,
            sha1: "abf3a580ddfc4cb372b5a4ce48ed7b2ea31e5270".into(),
            url: "https://cfx-downloads.akamaized.net/prod/019febd6-45b3-748d-a84c-baa43c75160a/update.rpf".into(),
            size: 1_423_288_320,
        }
    }

    fn wanted(build: u32, default_build: Option<u32>) -> Wanted {
        Wanted { build, default_build, pure: Some(1), pools: None }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("coldz-storage-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn built_in_table_is_fivems() {
        let cfg: serde_json::Value = serde_json::from_str(include_str!("../../launcher.config.json")).unwrap();
        let list: Vec<Entry> = serde_json::from_value(cfg["gameStorage"].clone()).unwrap();
        assert_eq!(clean(&list), [coldz_entry()]);
    }

    #[test]
    fn only_cfx_download_host_and_sane_entries() {
        assert!(coldz_entry().valid());
        let bad = |f: &dyn Fn(&mut Entry)| {
            let mut e = coldz_entry();
            f(&mut e);
            !e.valid()
        };
        assert!(bad(&|e| e.url = "https://example.com/update.rpf".into()));
        assert!(bad(&|e| e.url = "http://cfx-downloads.akamaized.net/prod/x/update.rpf".into()));
        assert!(bad(&|e| e.url = "https://cfx-downloads.akamaized.net.evil.ru/update.rpf".into()));
        assert!(bad(&|e| e.sha1 = "abf3".into()));
        assert!(bad(&|e| e.sha1 = "zbf3a580ddfc4cb372b5a4ce48ed7b2ea31e5270".into()));
        assert!(bad(&|e| e.size = 0));
        assert!(bad(&|e| e.build = 1));
        assert_eq!(coldz_entry().cache_name(), "override+update+update.rpf_abf3a580ddfc4cb372b5a4ce48ed7b2ea31e5270");
        assert_eq!(coldz_entry().plain_name(), "update+update.rpf_abf3a580ddfc4cb372b5a4ce48ed7b2ea31e5270");
    }

    #[test]
    fn needed_only_when_the_content_build_is_older_than_the_exe() {
        let t = [coldz_entry()];
        // COLDZ now: content 3258, exe 3889 → the override file
        assert_eq!(needed(&wanted(3258, Some(3889)), b"", &t), Some(coldz_entry()));
        // the server on 3889: nothing
        assert_eq!(needed(&wanted(3889, Some(3889)), b"", &t), None);
        // 3258 with the exe of 3258 (no server default, the file has none): FiveM copies the game's own update.rpf
        assert_eq!(needed(&wanted(3258, None), b"[Game]\r\n", &t), None);
        // no server default, but FiveM's file says exe 3889
        assert_eq!(needed(&wanted(3258, None), b"[Game]\r\nDefaultBuild=3889\r\n", &t), Some(coldz_entry()));
        // a build the table does not have
        assert_eq!(needed(&wanted(3095, Some(3889)), b"", &t), None);
        assert_eq!(needed(&wanted(1, Some(3889)), b"", &t), None);
    }

    #[test]
    fn installed_fivem_must_know_the_file() {
        let e = coldz_entry();
        let exe = format!("MZ....{}....\0\0{}\0", e.url, e.sha1.to_uppercase());
        assert!(fivem_knows(exe.as_bytes(), &e));
        assert!(!fivem_knows(format!("MZ {}", e.url).as_bytes(), &e));
        assert!(!fivem_knows(format!("MZ {}", e.sha1).as_bytes(), &e));
    }

    #[test]
    fn what_is_on_the_disk() {
        let d = tmp("disk");
        let e = Entry { size: 10, ..coldz_entry() };
        let p = paths(&d.join("FiveM.app"), &e);
        assert_eq!(p.dir, d.join("FiveM.app").join("data").join("game-storage"));
        assert!(p.part.to_string_lossy().ends_with("override+update+update.rpf_abf3a580ddfc4cb372b5a4ce48ed7b2ea31e5270.coldz-part"));
        assert_eq!(missing(&p, &e), Some(0));
        std::fs::create_dir_all(&p.dir).unwrap();
        std::fs::write(&p.part, b"1234").unwrap();
        assert_eq!(missing(&p, &e), Some(4));
        std::fs::write(&p.part, b"123456789012").unwrap();
        assert_eq!(missing(&p, &e), Some(0), "longer than the file: start over");
        // the done file counts only at its exact size (FiveM's rule)
        std::fs::write(&p.done, b"123").unwrap();
        assert_eq!(missing(&p, &e), Some(0));
        std::fs::write(&p.done, b"0123456789").unwrap();
        assert_eq!(missing(&p, &e), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn content_range_start() {
        assert_eq!(range_start("bytes 100-199/1000"), Some(100));
        assert_eq!(range_start("bytes 0-15/1423288320"), Some(0));
        assert_eq!(range_start("bytes */1000"), None);
        assert_eq!(range_start("items 1-2/3"), None);
    }

    #[test]
    fn sha1_of_a_file() {
        let d = tmp("sha");
        let f = d.join("x");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(sha1_file(&f, |_| {}).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let _ = std::fs::remove_dir_all(&d);
    }

    // ------------------------------------------------------------ the download against a fake CDN on 127.0.0.1

    #[derive(Clone, Copy, PartialEq)]
    enum Cdn {
        /// Answers Range with 206.
        Good,
        /// Ignores Range: always 200 with the whole file.
        NoRange,
        /// Sends `cut` bytes of each answer and closes the connection.
        Cuts(usize),
        /// Sends the headers and a little, then nothing.
        Stalls,
    }

    /// Returns the base url and the list of `Range` headers it got.
    async fn fake_cdn(body: Vec<u8>, kind: Cdn) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let body = Arc::new(body);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let body = body.clone();
                let seen = seen2.clone();
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 2048];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        let Ok(n) = sock.read(&mut buf).await else { return };
                        if n == 0 {
                            return;
                        }
                        req.extend_from_slice(&buf[..n]);
                    }
                    let text = String::from_utf8_lossy(&req).to_string();
                    let range = text
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(|r| r.trim_end_matches('-').parse::<usize>().unwrap()));
                    seen.lock().unwrap().push(range.map_or("-".into(), |r| r.to_string()));
                    let from = if kind == Cdn::NoRange { 0 } else { range.unwrap_or(0) };
                    let rest = &body[from.min(body.len())..];
                    let head = if from > 0 {
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len() - 1,
                            body.len(),
                            rest.len()
                        )
                    } else {
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", rest.len())
                    };
                    let _ = sock.write_all(head.as_bytes()).await;
                    match kind {
                        Cdn::Good | Cdn::NoRange => {
                            let _ = sock.write_all(rest).await;
                        }
                        Cdn::Cuts(cut) => {
                            let _ = sock.write_all(&rest[..cut.min(rest.len())]).await;
                        }
                        Cdn::Stalls => {
                            let _ = sock.write_all(&rest[..rest.len().min(3)]).await;
                            tokio::time::sleep(Duration::from_secs(60)).await;
                        }
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}/prod/x/update.rpf"), seen)
    }

    fn body(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn sha1_hex(b: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, b).as_ref().iter().map(|x| format!("{x:02x}")).collect()
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
    }

    fn client() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder().connect_timeout(Duration::from_secs(3)).build().unwrap()
    }

    /// An entry for the fake CDN (the url is not on Cfx.re's host: `run` does not check, the caller does).
    fn entry_for(url: &str, b: &[u8]) -> Entry {
        Entry { build: 3258, sha1: sha1_hex(b), url: url.into(), size: b.len() as u64 }
    }

    #[test]
    fn downloads_continues_and_checks() {
        rt().block_on(async {
            let b = body(300_000);
            // a CDN that breaks every answer after 70 000 bytes: the launcher continues with Range until done
            let (url, seen) = fake_cdn(b.clone(), Cdn::Cuts(70_000)).await;
            let d = tmp("cuts");
            let e = entry_for(&url, &b);
            let steps = Arc::new(Mutex::new(Vec::new()));
            let s2 = steps.clone();
            let r = run(client(), d.clone(), e.clone(), Some(u64::MAX), Arc::new(Cancel::default()), move |p| s2.lock().unwrap().push(p)).await;
            assert_eq!(r, Ok(()));
            let p = paths(&d, &e);
            assert_eq!(std::fs::read(&p.done).unwrap(), b);
            assert!(!p.part.exists());
            assert_eq!(*seen.lock().unwrap(), ["-", "70000", "140000", "210000", "280000"]);
            let steps = steps.lock().unwrap();
            assert_eq!(steps.first().unwrap(), &Progress { done: 0, total: 300_000, phase: "download" });
            assert_eq!(steps.last().unwrap(), &Progress { done: 300_000, total: 300_000, phase: "check" });
            // already there: nothing asked
            let r = run(client(), d.clone(), e.clone(), None, Arc::new(Cancel::default()), |_| {}).await;
            assert_eq!(r, Ok(()));
            assert_eq!(seen.lock().unwrap().len(), 5);
            let _ = std::fs::remove_dir_all(&d);
        });
    }

    #[test]
    fn a_part_from_last_time_is_continued() {
        rt().block_on(async {
            let b = body(100_000);
            let (url, seen) = fake_cdn(b.clone(), Cdn::Good).await;
            let d = tmp("resume");
            let e = entry_for(&url, &b);
            let p = paths(&d, &e);
            std::fs::create_dir_all(&p.dir).unwrap();
            std::fs::write(&p.part, &b[..40_000]).unwrap();
            assert_eq!(run(client(), d.clone(), e.clone(), None, Arc::new(Cancel::default()), |_| {}).await, Ok(()));
            assert_eq!(*seen.lock().unwrap(), ["40000"]);
            assert_eq!(std::fs::read(&p.done).unwrap(), b);
            let _ = std::fs::remove_dir_all(&d);
        });
    }

    #[test]
    fn a_server_without_range_starts_over() {
        rt().block_on(async {
            let b = body(50_000);
            let (url, _) = fake_cdn(b.clone(), Cdn::NoRange).await;
            let d = tmp("norange");
            let e = entry_for(&url, &b);
            let p = paths(&d, &e);
            std::fs::create_dir_all(&p.dir).unwrap();
            std::fs::write(&p.part, &b[..10_000]).unwrap();
            assert_eq!(run(client(), d.clone(), e.clone(), None, Arc::new(Cancel::default()), |_| {}).await, Ok(()));
            assert_eq!(std::fs::read(&p.done).unwrap(), b);
            let _ = std::fs::remove_dir_all(&d);
        });
    }

    #[test]
    fn wrong_bytes_are_thrown_away_and_wrong_size_stops() {
        rt().block_on(async {
            let b = body(20_000);
            let (url, _) = fake_cdn(b.clone(), Cdn::Good).await;
            let d = tmp("bad");
            // the table says another sha1
            let e = Entry { sha1: "0".repeat(40), ..entry_for(&url, &b) };
            assert_eq!(run(client(), d.clone(), e.clone(), None, Arc::new(Cancel::default()), |_| {}).await, Err(Fail::Bad));
            let p = paths(&d, &e);
            assert!(!p.part.exists() && !p.done.exists());
            // the table says another size: stop at once, nothing kept
            let e = Entry { size: 30_000, ..entry_for(&url, &b) };
            let r = run(client(), d.clone(), e.clone(), None, Arc::new(Cancel::default()), |_| {}).await;
            assert!(matches!(r, Err(Fail::Remote(_))), "{r:?}");
            assert!(!paths(&d, &e).done.exists());
            let _ = std::fs::remove_dir_all(&d);
        });
    }

    #[test]
    fn cancel_keeps_the_part_and_space_is_checked() {
        rt().block_on(async {
            let b = body(10_000);
            let (url, _) = fake_cdn(b.clone(), Cdn::Stalls).await;
            let d = tmp("cancel");
            let e = entry_for(&url, &b);
            let cancel = Arc::new(Cancel::default());
            let c2 = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                c2.cancel();
            });
            let t = Instant::now();
            let r = run(client(), d.clone(), e.clone(), None, cancel, |_| {}).await;
            assert_eq!(r, Err(Fail::Cancelled));
            assert!(t.elapsed() < Duration::from_secs(5));
            assert_eq!(missing(&paths(&d, &e), &e), Some(3), "the 3 bytes that came stay");
            // not enough space: nothing asked
            let r = run(client(), d.clone(), e.clone(), Some(1000), Arc::new(Cancel::default()), |_| {}).await;
            assert_eq!(r, Err(Fail::Space(10_000 - 3 + SPACE_MARGIN)));
            let _ = std::fs::remove_dir_all(&d);
        });
    }

    #[test]
    fn the_same_bytes_under_the_other_name_are_used() {
        rt().block_on(async {
            let b = body(30_000);
            // nobody listens there: nothing may be downloaded
            let d = tmp("plain");
            let e = entry_for("http://127.0.0.1:9/none", &b);
            let p = paths(&d, &e);
            std::fs::create_dir_all(&p.dir).unwrap();
            std::fs::write(&p.plain, &b).unwrap();
            assert_eq!(run(client(), d.clone(), e.clone(), None, Arc::new(Cancel::default()), |_| {}).await, Ok(()));
            assert_eq!(std::fs::read(&p.done).unwrap(), b);
            assert_eq!(std::fs::read(&p.plain).unwrap(), b, "the other file stays");
            let _ = std::fs::remove_dir_all(&d);
        });
    }

    #[test]
    fn no_connection_gives_up_after_a_few_tries() {
        rt().block_on(async {
            let b = body(1000);
            let d = tmp("down");
            let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
            let e = entry_for(&format!("http://{free}/x"), &b);
            let t = Instant::now();
            let r = run(client(), d.clone(), e, None, Arc::new(Cancel::default()), |_| {}).await;
            assert!(matches!(r, Err(Fail::Network(_))), "{r:?}");
            // Windows answers a closed local port after ~2 s: 5 tries and 4 pauses of 2 s
            assert!(t.elapsed() < Duration::from_secs(40));
            let _ = std::fs::remove_dir_all(&d);
        });
    }
}
