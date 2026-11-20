//! 0.5.5: the first join to COLDZ and the speed to our file server (CDN).
//!
//! Alexander 20.11: «сделаем всё, что можем официально и без денег, чтобы улучшить загрузку». A new player downloads
//! about 6 GB of server files from our CDN at the first join. FiveM does it itself, on its loading screen: the launcher
//! never does it instead of FiveM (Cfx rules). The CDN gives 90 Mbit/s in total, players in Russia often have
//! 10-80 Mbit/s. So the launcher says it honestly under «Играть» and measures the speed to the CDN (README 7о):
//!
//!  * `measure`: GET of the test file (`cdnSpeedUrl`: 8 MiB, no token, `Cache-Control: no-store`), the first 8 MiB or
//!    6 s, whatever comes first. The speed counts from the first 256 KiB on (the connection warms up first);
//!    1 MB = 1 000 000 bytes, as FiveM shows sizes. Not more often than once in 10 minutes (a failure without data:
//!    once a minute), never in the 2 minutes after «Играть», never while the game file downloads (storage.rs):
//!    lib.rs `cdn_measure`. Errors only go to net-check.log.
//!  * the first join (`is_first`): the launcher's own mark (settings.json `joinedOnce`, set after «Играть» started
//!    FiveM) and only whether FiveM's folder `FiveM.app\data\server-cache-priv` exists (nothing inside is read): no
//!    folder = the first time for sure.
//!  * the line (`advice`): minutes = size / speed, rounded up (5-minute steps above 10, 10-minute steps above 60);
//!    slower than 2 MB/s: «close torrents and other downloads». A returning player: nothing, or «updates take a minute
//!    or two» when news.json says `updateHint: true`.
//!
//! The address and the size: `launcher.config.json` `cdnSpeedUrl`, `firstJoinGb`; news.json wins (an empty
//! `cdnSpeedUrl` switches the measuring off, `firstJoinGb: 0` the first-join line).

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};

/// How much of the test file is read at most (the file itself: 8 388 608 bytes).
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// The whole measurement, the connection included, is never longer than this.
pub const MAX_TIME: Duration = Duration::from_secs(6);
/// The speed counts after this much has come (TCP slow start would make a fast line look slower).
pub const WARMUP_BYTES: u64 = 256 * 1024;
/// Less than this in the whole time: no usable answer (a throttled connection stops after ~16 KB).
pub const MIN_BYTES: u64 = 64 * 1024;
/// Not more often than this: the test file is 8 MB of our 90 Mbit/s CDN.
pub const EVERY_SECS: u64 = 600;
/// A failure that brought (almost) no data may be asked again sooner: the player may have fixed the network.
pub const RETRY_SECS: u64 = 60;
/// Slower than this (MB/s): the line asks to close torrents and other downloads.
pub const SLOW_MBPS: f64 = 2.0;
/// No measuring this long after «Играть»: FiveM starts, the game file or the server files download.
pub const PLAY_QUIET: Duration = Duration::from_secs(120);
/// `firstJoinGb` above this is a typo: the built-in size is used.
pub const MAX_GB: f64 = 200.0;
/// FiveM's cache of the servers' files, under `FiveM.app\data`. Only its existence is looked at.
pub const CACHE_DIR: &str = "server-cache-priv";

// ---------------------------------------------------------------- settings: news.json wins

/// What news.json says (lib.rs `apply_feed`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feed {
    /// `cdnSpeedUrl`: Some("") = measuring off; None = no key, or not a good https address: the built-in one.
    pub cdn_url: Option<String>,
    /// `firstJoinGb`: Some(0) = no first-join line; None = no key, or not a number from 0 to 200: the built-in one.
    pub first_gb: Option<f64>,
    /// `updateHint: true`: a returning player sees «Обновления докачаются за минуту-другую».
    pub update_hint: bool,
}

impl Feed {
    pub fn of(v: &serde_json::Value) -> Feed {
        let cdn_url = v
            .get("cdnSpeedUrl")
            .and_then(|u| u.as_str())
            .map(str::trim)
            .filter(|u| u.is_empty() || valid_url(u))
            .map(str::to_string);
        let first_gb = v.get("firstJoinGb").and_then(|g| g.as_f64()).filter(|g| valid_gb(*g));
        let update_hint = v.get("updateHint").and_then(|h| h.as_bool()).unwrap_or(false);
        Feed { cdn_url, first_gb, update_hint }
    }
}

/// Only `https://` with a host name (like the other addresses the launcher takes from news.json).
pub fn valid_url(u: &str) -> bool {
    u.len() <= 300
        && u.get(..8).map_or(false, |p| p.eq_ignore_ascii_case("https://"))
        && reqwest::Url::parse(u).ok().and_then(|x| x.host_str().map(|h| !h.is_empty())).unwrap_or(false)
}

fn valid_gb(g: f64) -> bool {
    g.is_finite() && (0.0..=MAX_GB).contains(&g)
}

/// What is in use.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    /// The test file; "" = measuring off.
    pub url: String,
    /// The first-join download in GB; 0 = no first-join line.
    pub gb: f64,
    pub update_hint: bool,
}

/// news.json wins over launcher.config.json; a bad built-in value switches the thing off.
pub fn setup(feed: &Feed, built_in_url: &str, built_in_gb: f64) -> Setup {
    let built_in_url = built_in_url.trim();
    let url = match &feed.cdn_url {
        Some(u) => u.clone(),
        None if valid_url(built_in_url) => built_in_url.to_string(),
        None => String::new(),
    };
    let gb = feed.first_gb.unwrap_or(if valid_gb(built_in_gb) { built_in_gb } else { 0.0 });
    Setup { url, gb, update_hint: feed.update_hint }
}

// ---------------------------------------------------------------- the line under «Играть»

/// About how many minutes `gb` take at `mbps` (1 GB = 1000 MB). None: no size or no speed.
pub fn minutes(gb: f64, mbps: f64) -> Option<u32> {
    if !(gb.is_finite() && mbps.is_finite() && gb > 0.0 && mbps > 0.0) {
        return None;
    }
    let raw = (gb * 1000.0 / mbps / 60.0).ceil().max(1.0);
    let step = if raw <= 10.0 {
        1.0
    } else if raw <= 60.0 {
        5.0
    } else {
        10.0
    };
    Some(((raw / step).ceil() * step).min(1_000_000.0) as u32)
}

pub fn slow(mbps: Option<f64>) -> bool {
    mbps.map_or(false, |s| s < SLOW_MBPS)
}

/// What the window says under «Играть».
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Advice {
    /// "first" (the first join: the size, minutes when the speed is known) | "update" (`updateHint`) | "none"
    pub kind: &'static str,
    pub gb: f64,
    pub minutes: Option<u32>,
    /// The speed is known and below `SLOW_MBPS`: the line adds «close torrents and other downloads».
    pub slow: bool,
}

pub fn advice(first: bool, setup: &Setup, mbps: Option<f64>) -> Advice {
    if first && setup.gb > 0.0 {
        return Advice { kind: "first", gb: setup.gb, minutes: mbps.and_then(|s| minutes(setup.gb, s)), slow: slow(mbps) };
    }
    let kind = if !first && setup.update_hint { "update" } else { "none" };
    Advice { kind, gb: setup.gb, minutes: None, slow: false }
}

// ---------------------------------------------------------------- the first join or not

/// FiveM's cache folder of the servers' files: only whether it is there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cache {
    /// FiveM.app is there, `data\server-cache-priv` is not: FiveM has never downloaded a server's files.
    Missing,
    Present,
    /// FiveM not found (or no FiveM.app): only the launcher's mark counts.
    Unknown,
}

/// `fivem_app`: FiveM.app next to the FiveM.exe the launcher found (support.rs `app_dir`).
pub fn cache_of(fivem_app: Option<&Path>) -> Cache {
    match fivem_app {
        Some(app) if app.is_dir() => {
            if app.join("data").join(CACHE_DIR).is_dir() {
                Cache::Present
            } else {
                Cache::Missing
            }
        }
        _ => Cache::Unknown,
    }
}

/// The player has joined COLDZ through the launcher. `joinedOnce` is new in 0.5.5: until the first «Играть» of
/// 0.5.5 (lib.rs `send_nick` writes it then) a launcher id means «Играть» was pressed in an older version, since
/// 0.3.0 the id is made only there.
pub fn joined_before(joined_once: Option<bool>, launcher_id: &str) -> bool {
    joined_once.unwrap_or(!launcher_id.trim().is_empty())
}

/// No cache folder = the first time for sure; with the folder (it may be another server's files) the mark decides.
pub fn is_first(joined: bool, cache: Cache) -> bool {
    cache == Cache::Missing || !joined
}

// ---------------------------------------------------------------- the speed

/// One measurement as kept in settings.json (`cdnSpeed`): the next start does not download the file again within
/// `EVERY_SECS`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Record {
    pub url: String,
    /// Unix seconds of the attempt.
    pub at: u64,
    /// MB/s; None: no usable answer.
    pub mbps: Option<f64>,
    pub bytes: u64,
    /// "" when measured, else what stopped it (`Measure::problem`).
    pub problem: String,
}

/// Time to measure again: no record for this address, the clock went back, or the record is old enough.
pub fn due(rec: Option<&Record>, url: &str, now: u64) -> bool {
    let Some(r) = rec.filter(|r| r.url == url) else { return true };
    if r.at > now {
        return true;
    }
    let every = if r.mbps.is_some() || r.bytes >= MIN_BYTES { EVERY_SECS } else { RETRY_SECS };
    now - r.at >= every
}

/// One download of the test file.
#[derive(Clone, Debug, PartialEq)]
pub struct Measure {
    /// MB/s; None: no usable answer (`problem`).
    pub mbps: Option<f64>,
    pub bytes: u64,
    pub ms: u32,
    pub status: Option<u16>,
    /// None: measured (also when stopped early with enough data). Else "connect" (no connection), "timeout" (no
    /// answer in time), "http" (an error answer), "read" (the answer broke), "slow" (less than `MIN_BYTES` in the
    /// whole time: what a throttled connection looks like), "short" (the file is too small), "aborted" («Играть»).
    pub problem: Option<&'static str>,
}

impl Measure {
    pub fn record(&self, url: &str, at: u64) -> Record {
        Record { url: url.to_string(), at, mbps: self.mbps, bytes: self.bytes, problem: self.problem.unwrap_or("").to_string() }
    }

    /// One line for net-check.log.
    pub fn log_line(&self, url: &str) -> String {
        let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        let what = match (self.mbps, self.problem) {
            (Some(s), _) => format!("{s:.1} МБ/с"),
            (None, p) => format!("не измерено ({})", p.unwrap_or("?")),
        };
        let status = self.status.map(|s| format!(" (HTTP {s})")).unwrap_or_default();
        format!("сервер файлов {host}: {what}{status}, {} байт за {} мс", self.bytes, self.ms)
    }
}

/// MB/s from what came: after the warm-up mark when there is enough data past it, else over the whole body.
/// `warm`: (bytes at the mark, seconds from the mark to the end).
pub fn rate(bytes: u64, secs: f64, warm: Option<(u64, f64)>) -> f64 {
    if let Some((at, after)) = warm {
        if bytes.saturating_sub(at) >= WARMUP_BYTES && after >= 0.05 {
            return (bytes - at) as f64 / after / 1_000_000.0;
        }
    }
    bytes as f64 / secs.max(0.001) / 1_000_000.0
}

fn left(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Downloads `url` until `max_bytes`, the end of the file or `max_time` (the connection included), whatever comes
/// first. `stop()` true («Играть» was pressed): ends at once (within ~0.2 s).
pub async fn measure(client: &reqwest::Client, url: &str, max_bytes: u64, max_time: Duration, stop: impl Fn() -> bool) -> Measure {
    let started = Instant::now();
    let deadline = started + max_time;
    let ms = || started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    let failed = |problem: &'static str, status: Option<u16>, bytes: u64, ms: u32| Measure { mbps: None, bytes, ms, status, problem: Some(problem) };
    let mut resp = match tokio::time::timeout(left(deadline), client.get(url).send()).await {
        Err(_) => return failed("timeout", None, 0, ms()),
        Ok(Err(e)) if e.is_timeout() => return failed("timeout", None, 0, ms()),
        Ok(Err(e)) if e.is_connect() => return failed("connect", None, 0, ms()),
        Ok(Err(_)) => return failed("http", None, 0, ms()),
        Ok(Ok(r)) => r,
    };
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return failed("http", Some(status), 0, ms());
    }
    let body_from = Instant::now();
    let mut bytes = 0u64;
    let mut warm: Option<(u64, Instant)> = None;
    let mut ended: Option<&'static str> = None;
    loop {
        if stop() {
            ended = Some("aborted");
            break;
        }
        let wait = left(deadline);
        if wait.is_zero() {
            break;
        }
        // short waits, so `stop` is seen soon even when no data comes
        match tokio::time::timeout(wait.min(Duration::from_millis(200)), resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                bytes += chunk.len() as u64;
                if warm.is_none() && bytes >= WARMUP_BYTES {
                    warm = Some((bytes, Instant::now()));
                }
                if bytes >= max_bytes {
                    break;
                }
            }
            Ok(Ok(None)) => {
                ended = Some("short");
                break;
            }
            Ok(Err(_)) => {
                ended = Some("read");
                break;
            }
            Err(_) => {}
        }
    }
    let end = Instant::now();
    if bytes < MIN_BYTES {
        return failed(ended.unwrap_or("slow"), Some(status), bytes, ms());
    }
    let secs = end.duration_since(body_from).as_secs_f64();
    let mbps = rate(bytes, secs, warm.map(|(b, t)| (b, end.duration_since(t).as_secs_f64())));
    Measure { mbps: Some(mbps), bytes, ms: ms(), status: Some(status), problem: None }
}

// ---------------------------------------------------------------- for the window

/// The file-server line of «Проверить связь».
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Cdn {
    /// "off" (no `cdnSpeedUrl`) | "none" (not measured yet) | "ok" | "fail"
    pub state: &'static str,
    pub mbps: Option<f64>,
    pub slow: bool,
    /// Unix seconds of the measurement (0: none).
    pub at: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JoinInfo {
    pub first: bool,
    pub advice: Advice,
    pub cdn: Cdn,
}

/// The window's view: the last measurement of the address in use (an older address does not count).
pub fn info(first: bool, setup: &Setup, rec: Option<&Record>) -> JoinInfo {
    let rec = rec.filter(|r| !setup.url.is_empty() && r.url == setup.url);
    let mbps = rec.and_then(|r| r.mbps);
    let state = match rec {
        _ if setup.url.is_empty() => "off",
        None => "none",
        Some(r) if r.mbps.is_some() => "ok",
        Some(_) => "fail",
    };
    JoinInfo {
        first,
        advice: advice(first, setup, mbps),
        cdn: Cdn { state, mbps, slow: slow(mbps), at: rec.map_or(0, |r| r.at) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const URL: &str = "https://cdn.coldz.vip/speed/8mb.bin";

    fn on(gb: f64, update_hint: bool) -> Setup {
        Setup { url: URL.into(), gb, update_hint }
    }

    #[test]
    fn minutes_for_the_first_join() {
        // 6 GB: the whole CDN (90 Mbit/s = 11.25 MB/s), 40 and 10 Mbit/s, a slow line
        assert_eq!(minutes(6.0, 11.25), Some(9));
        assert_eq!(minutes(6.0, 10.0), Some(10));
        assert_eq!(minutes(6.0, 5.0), Some(20));
        assert_eq!(minutes(6.0, 3.0), Some(35)); // 33.3 → 35: 5-minute steps above 10
        assert_eq!(minutes(6.0, 1.25), Some(80)); // 10-minute steps above 60
        assert_eq!(minutes(6.0, 0.7), Some(150)); // 142.9 → 150
        assert_eq!(minutes(6.0, 9.0), Some(15)); // 11.1 → 12 → 15
        // never less than a minute
        assert_eq!(minutes(0.01, 100.0), Some(1));
        // no size, no speed, nonsense
        for (gb, s) in [(0.0, 5.0), (6.0, 0.0), (-1.0, 5.0), (6.0, -2.0), (f64::NAN, 5.0), (6.0, f64::INFINITY)] {
            assert_eq!(minutes(gb, s), None, "{gb} {s}");
        }
    }

    #[test]
    fn slow_below_two_mb_per_second() {
        assert!(slow(Some(1.99)));
        assert!(slow(Some(0.1)));
        assert!(!slow(Some(2.0)));
        assert!(!slow(Some(11.0)));
        assert!(!slow(None), "not measured is not slow");
    }

    #[test]
    fn first_join_or_returning_player() {
        // the first join: the size, minutes when the speed is known, «slow» below 2 MB/s
        assert_eq!(advice(true, &on(6.0, false), Some(5.0)), Advice { kind: "first", gb: 6.0, minutes: Some(20), slow: false });
        assert_eq!(advice(true, &on(6.0, false), Some(1.0)), Advice { kind: "first", gb: 6.0, minutes: Some(100), slow: true });
        assert_eq!(advice(true, &on(6.0, false), None), Advice { kind: "first", gb: 6.0, minutes: None, slow: false });
        // returning: nothing, or the short update line with `updateHint`
        assert_eq!(advice(false, &on(6.0, false), Some(1.0)).kind, "none");
        assert_eq!(advice(false, &on(6.0, true), Some(1.0)), Advice { kind: "update", gb: 6.0, minutes: None, slow: false });
        // `firstJoinGb: 0`: no first-join line (and no update line for a first join either)
        assert_eq!(advice(true, &on(0.0, true), Some(5.0)).kind, "none");

        // the mark: 0.5.5 writes joinedOnce; before that a launcher id means «Играть» was pressed in an older version
        assert!(!joined_before(None, ""));
        assert!(joined_before(None, "0123456789abcdef0123456789abcdef"));
        assert!(!joined_before(Some(false), "0123456789abcdef0123456789abcdef"));
        assert!(joined_before(Some(true), ""));
        // no cache folder: the first time whatever the mark says; with it the mark decides
        assert!(is_first(true, Cache::Missing));
        assert!(is_first(false, Cache::Missing));
        assert!(!is_first(true, Cache::Present));
        assert!(is_first(false, Cache::Present));
        assert!(!is_first(true, Cache::Unknown));
        assert!(is_first(false, Cache::Unknown));
    }

    /// Only the folder's existence, on temporary folders (never the real FiveM of this PC).
    #[test]
    fn cache_folder_only_exists_or_not() {
        let d = std::env::temp_dir().join(format!("coldz-firstjoin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let app = d.join("FiveM.app");
        assert_eq!(cache_of(None), Cache::Unknown);
        assert_eq!(cache_of(Some(&app)), Cache::Unknown, "no FiveM.app");
        std::fs::create_dir_all(app.join("data")).unwrap();
        assert_eq!(cache_of(Some(&app)), Cache::Missing);
        // a file with that name is not FiveM's cache
        std::fs::write(app.join("data").join(CACHE_DIR), b"").unwrap();
        assert_eq!(cache_of(Some(&app)), Cache::Missing);
        std::fs::remove_file(app.join("data").join(CACHE_DIR)).unwrap();
        std::fs::create_dir_all(app.join("data").join(CACHE_DIR)).unwrap();
        assert_eq!(cache_of(Some(&app)), Cache::Present);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn news_json_wins_and_switches_off() {
        // no keys: the built-in address and size
        let none = Feed::of(&json!({ "items": [] }));
        assert_eq!(none, Feed::default());
        assert_eq!(setup(&none, URL, 6.0), on(6.0, false));
        // news.json changes them
        let f = Feed::of(&json!({ "cdnSpeedUrl": " https://cdn2.coldz.vip/speed/8mb.bin ", "firstJoinGb": 7.5, "updateHint": true }));
        assert_eq!(setup(&f, URL, 6.0), Setup { url: "https://cdn2.coldz.vip/speed/8mb.bin".into(), gb: 7.5, update_hint: true });
        // an empty address: measuring off; 0 GB: no first-join line
        let off = Feed::of(&json!({ "cdnSpeedUrl": "", "firstJoinGb": 0 }));
        assert_eq!(setup(&off, URL, 6.0), Setup { url: String::new(), gb: 0.0, update_hint: false });
        // bad values: the built-in ones
        for bad in [
            json!({ "cdnSpeedUrl": "http://cdn.coldz.vip/speed/8mb.bin" }),
            json!({ "cdnSpeedUrl": "ftp://x/y" }),
            json!({ "cdnSpeedUrl": "https://" }),
            json!({ "cdnSpeedUrl": 5 }),
            json!({ "firstJoinGb": "6" }),
            json!({ "firstJoinGb": -1 }),
            json!({ "firstJoinGb": 500 }),
            json!({ "updateHint": "yes" }),
        ] {
            assert_eq!(setup(&Feed::of(&bad), URL, 6.0), on(6.0, false), "{bad}");
        }
        // a bad built-in value switches it off
        assert_eq!(setup(&none, "http://x/y", f64::NAN), Setup { url: String::new(), gb: 0.0, update_hint: false });
        assert!(valid_url("HTTPS://cdn.coldz.vip/a"));
        assert!(!valid_url(&format!("https://x/{}", "a".repeat(300))));
    }

    #[test]
    fn measured_once_in_ten_minutes() {
        let now = 1_800_000_000;
        let ok = Record { url: URL.into(), at: now - 300, mbps: Some(4.0), bytes: MAX_BYTES, problem: String::new() };
        assert!(due(None, URL, now));
        assert!(!due(Some(&ok), URL, now), "5 minutes ago");
        assert!(due(Some(&Record { at: now - 601, ..ok.clone() }), URL, now), "10 minutes ago");
        assert!(due(Some(&ok), "https://cdn2.coldz.vip/speed/8mb.bin", now), "another address");
        assert!(due(Some(&Record { at: now + 3600, ..ok.clone() }), URL, now), "the clock went back");
        // a failure without data: again after a minute; with data (a slow line): after 10 minutes
        let dead = Record { mbps: None, bytes: 0, problem: "connect".into(), at: now - 30, ..ok.clone() };
        assert!(!due(Some(&dead), URL, now));
        assert!(due(Some(&Record { at: now - 61, ..dead.clone() }), URL, now));
        let crawl = Record { mbps: None, bytes: MIN_BYTES, problem: "read".into(), at: now - 300, ..ok.clone() };
        assert!(!due(Some(&crawl), URL, now));

        // the window: the last measurement of this address
        let i = info(true, &on(6.0, false), Some(&ok));
        assert_eq!((i.cdn.state, i.cdn.mbps, i.advice.minutes), ("ok", Some(4.0), Some(25)));
        assert_eq!(info(true, &on(6.0, false), Some(&dead)).cdn.state, "fail");
        assert_eq!(info(true, &on(6.0, false), None).cdn.state, "none");
        let other = Record { url: "https://old.example/x".into(), ..ok.clone() };
        assert_eq!(info(true, &on(6.0, false), Some(&other)).cdn.state, "none");
        let off = Setup { url: String::new(), gb: 6.0, update_hint: false };
        let i = info(true, &off, Some(&ok));
        assert_eq!((i.cdn.state, i.advice.kind, i.advice.minutes), ("off", "first", None));
        let json = serde_json::to_value(info(true, &on(6.0, false), Some(&Record { mbps: Some(1.5), ..ok.clone() }))).unwrap();
        assert_eq!(json["advice"], json!({ "kind": "first", "gb": 6.0, "minutes": 70, "slow": true }));
        assert_eq!(json["cdn"]["slow"], json!(true));
        // the record in settings.json
        let m = Measure { mbps: None, bytes: 16384, ms: 6000, status: Some(200), problem: Some("slow") };
        assert_eq!(m.record(URL, 7), Record { url: URL.into(), at: 7, mbps: None, bytes: 16384, problem: "slow".into() });
        assert_eq!(m.log_line(URL), "сервер файлов cdn.coldz.vip: не измерено (slow) (HTTP 200), 16384 байт за 6000 мс");
        let m = Measure { mbps: Some(4.25), bytes: MAX_BYTES, ms: 2100, status: Some(200), problem: None };
        assert_eq!(m.log_line(URL), "сервер файлов cdn.coldz.vip: 4.2 МБ/с (HTTP 200), 8388608 байт за 2100 мс");
    }

    #[test]
    fn speed_after_the_warm_up() {
        // 8 MB in 2 s, of which the first 256 KiB took 0.5 s: the speed after the mark
        let after = (MAX_BYTES - WARMUP_BYTES) as f64 / 1.5 / 1e6;
        assert!((rate(MAX_BYTES, 2.0, Some((WARMUP_BYTES, 1.5))) - after).abs() < 1e-9);
        // too little past the mark, or no mark: the whole body
        assert!((rate(300_000, 1.0, Some((WARMUP_BYTES, 0.2))) - 0.3).abs() < 1e-9);
        assert!((rate(100_000, 0.5, None) - 0.2).abs() < 1e-9);
        assert!(rate(1000, 0.0, None).is_finite());
    }

    /// How a fake file server on 127.0.0.1 answers.
    #[derive(Clone, Copy)]
    enum Fake {
        /// 200 with `len` bytes at once
        Fast(usize),
        /// 200 with `len` bytes, `chunk` bytes every `ms` milliseconds
        Trickle(usize, usize, u64),
        /// 200 for `len` bytes, then only `sent` bytes and silence (the «16 KB» throttling)
        Stall(usize, usize),
        /// an error answer
        Status(u16),
        /// accept and never answer
        Silent,
    }

    async fn cdn(kind: Fake) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let mut got = Vec::new();
                    while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => got.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = |len: usize| format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n");
                    match kind {
                        Fake::Fast(len) => {
                            let _ = s.write_all(head(len).as_bytes()).await;
                            let _ = s.write_all(&vec![7u8; len]).await;
                        }
                        Fake::Trickle(len, chunk, ms) => {
                            let _ = s.write_all(head(len).as_bytes()).await;
                            let mut sent = 0;
                            while sent < len {
                                let n = chunk.min(len - sent);
                                if s.write_all(&vec![7u8; n]).await.is_err() {
                                    return;
                                }
                                sent += n;
                                tokio::time::sleep(Duration::from_millis(ms)).await;
                            }
                        }
                        Fake::Stall(len, sent) => {
                            let _ = s.write_all(head(len).as_bytes()).await;
                            let _ = s.write_all(&vec![7u8; sent]).await;
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                        Fake::Status(code) => {
                            let _ = s.write_all(format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
                        }
                        Fake::Silent => tokio::time::sleep(Duration::from_secs(30)).await,
                    }
                });
            }
        });
        format!("http://127.0.0.1:{port}/speed/8mb.bin")
    }

    fn rt() -> tokio::runtime::Runtime {
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().unwrap()
    }

    #[test]
    fn speed_on_a_fake_file_server() {
        rt().block_on(async {
            let c = client();
            let never = || false;
            // the whole 8 MiB at once: measured, fast
            let m = measure(&c, &cdn(Fake::Fast(MAX_BYTES as usize)).await, MAX_BYTES, Duration::from_secs(6), never).await;
            assert_eq!((m.problem, m.bytes, m.status), (None, MAX_BYTES, Some(200)));
            assert!(m.mbps.unwrap() > SLOW_MBPS, "{m:?}");
            // a bigger file: only the first 8 MiB are read
            let m = measure(&c, &cdn(Fake::Fast(3 * MAX_BYTES as usize)).await, MAX_BYTES, Duration::from_secs(6), never).await;
            assert_eq!(m.problem, None);
            assert!(m.bytes >= MAX_BYTES && m.bytes < MAX_BYTES + 1024 * 1024, "{}", m.bytes);
            // a slow line, ~0.5 MB/s (32 KiB every 64 ms): stopped by the time, the speed is about right and «slow»
            let t = Instant::now();
            let m = measure(&c, &cdn(Fake::Trickle(MAX_BYTES as usize, 32 * 1024, 64)).await, MAX_BYTES, Duration::from_millis(2500), never).await;
            assert!(t.elapsed() < Duration::from_millis(3200), "{:?}", t.elapsed());
            let s = m.mbps.unwrap();
            assert!(s > 0.3 && s < 0.7, "{s} MB/s, {m:?}");
            assert!(slow(m.mbps) && m.bytes < MAX_BYTES);
            // the «16 KB» throttling: no usable answer
            let m = measure(&c, &cdn(Fake::Stall(MAX_BYTES as usize, 16 * 1024)).await, MAX_BYTES, Duration::from_millis(1500), never).await;
            assert_eq!((m.problem, m.mbps, m.bytes), (Some("slow"), None, 16 * 1024));
            // an error answer, a file that is too small
            let m = measure(&c, &cdn(Fake::Status(404)).await, MAX_BYTES, Duration::from_secs(3), never).await;
            assert_eq!((m.problem, m.status), (Some("http"), Some(404)));
            let m = measure(&c, &cdn(Fake::Fast(1000)).await, MAX_BYTES, Duration::from_secs(3), never).await;
            assert_eq!((m.problem, m.bytes), (Some("short"), 1000));
            // connected, never answers: within the time
            let t = Instant::now();
            let m = measure(&c, &cdn(Fake::Silent).await, MAX_BYTES, Duration::from_millis(1200), never).await;
            assert_eq!(m.problem, Some("timeout"));
            assert!(t.elapsed() < Duration::from_millis(1800));
            // nobody listens
            let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let m = measure(&c, &format!("http://127.0.0.1:{closed}/speed/8mb.bin"), MAX_BYTES, Duration::from_secs(3), never).await;
            assert_eq!(m.problem, Some("connect"));
            // «Играть» pressed: ends at once; with enough data the speed still counts
            let pressed = Instant::now() + Duration::from_millis(400);
            let t = Instant::now();
            let url = cdn(Fake::Trickle(MAX_BYTES as usize, 4 * 1024, 100)).await;
            let m = measure(&c, &url, MAX_BYTES, Duration::from_secs(6), || Instant::now() > pressed).await;
            assert_eq!(m.problem, Some("aborted"));
            assert!(t.elapsed() < Duration::from_millis(1200), "{:?}", t.elapsed());
            let pressed = Instant::now() + Duration::from_millis(800);
            let url = cdn(Fake::Trickle(MAX_BYTES as usize, 64 * 1024, 20)).await;
            let m = measure(&c, &url, MAX_BYTES, Duration::from_secs(6), || Instant::now() > pressed).await;
            assert!(m.problem.is_none() && m.mbps.is_some() && m.bytes < MAX_BYTES, "{m:?}");
        });
    }

    /// The real test file from this PC (prints the speed): `cargo test real_cdn_speed -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_cdn_speed() {
        rt().block_on(async {
            let m = measure(&client(), URL, MAX_BYTES, MAX_TIME, || false).await;
            eprintln!("{}", m.log_line(URL));
            assert_eq!(m.status, Some(200), "{m:?}");
        });
    }
}
