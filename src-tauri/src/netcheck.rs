//! Connection check before «Играть» and on «Проверить связь» (15.11.2026).
//!
//! Since 2025 some Russian providers cut access to foreign services (since 9.06.2025 Cloudflare-hosted sites load
//! only the first 16 KB, then the connection hangs; Hetzner, DigitalOcean, OVH too:
//! blog.cloudflare.com/russian-internet-users-are-unable-to-access-the-open-internet). FiveM talks to its own
//! services (Cfx.re) at every start: without them it does not update, does not start («Could not contact entitlement
//! service») or cannot join a OneSync server like COLDZ («Could not check server feature policy»). Players then blame
//! COLDZ, which sits on Russian hosting and is reachable. So the launcher asks the same Cfx.re hosts FiveM asks, and
//! our server separately, and tells the player whose problem it is.
//!
//! The addresses and where each one comes from (FiveM sources, strings in the installed FiveM files): README 7б.
//!
//! Nothing about the player is sent: plain GET/HEAD requests, no cookies, no ids, the same User-Agent as the rest of
//! the launcher. Only a short log of what answered how is kept (net-check.log next to settings.json) for support.

use serde::Serialize;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// FiveM's update storage (`CFX_UPDATER_URL`, code/client/launcher/LauncherConfig.h; the same string is in FiveM.exe).
pub const UPDATES: &str = "https://cfx-content.akamaized.net/updates";

/// One Cfx.re service FiveM needs.
pub struct Service {
    /// "update" | "auth" | "policy" | "services" | "list" (the window shows `svc_<id>`)
    pub id: &'static str,
    /// What the launcher asks: the address FiveM itself asks, or the root of that host.
    pub url: &'static str,
    /// Without it FiveM does not start or cannot join COLDZ: «Играть» stops and shows the message.
    pub critical: bool,
}

/// Order = order in the window.
pub static SERVICES: [Service; 5] = [
    // Client updates: FiveM.exe asks heads/fivereborn/<channel>, then downloads the manifest of the build (~99 KB)
    // and the files (Bootstrap.cpp, Updater.cpp). The launcher does the same two requests: the manifest is bigger
    // than the 16 KB after which a throttled connection hangs.
    Service { id: "update", url: "https://cfx-content.akamaized.net/updates/heads/fivereborn/production", critical: true },
    // Sign-in: the license (entitlement) check after the Rockstar sign-in, Cfx.re tickets for joining servers
    // (CNL_ENDPOINT, code/client/shared/CnlEndpoint.h; legitimacy.dll). Only the root of the host is asked.
    Service { id: "auth", url: "https://lambda.fivem.net/", critical: true },
    // Joining a OneSync server: «Requesting server feature policy...» (NetLibrary.cpp POLICY_LIVE_ENDPOINT).
    Service { id: "policy", url: "https://policy-live.fivem.net/", critical: true },
    // Pool size limits (COLDZ asks FiveM for bigger pools; without the limits FiveM drops the request), the NUI
    // blocklist, notices (PoolSizesState.cpp, ResourceUI.cpp).
    Service { id: "services", url: "https://gss.cfx-services.net/v1/public/pool-size-limits/fivem", critical: false },
    // The server list in the FiveM menu (citizen/ui.zip). Not needed for fivem://connect/<address>. The redirect to
    // the 21 MB list is not followed.
    Service { id: "list", url: "https://frontend.cfx-services.net/api/servers/streamRedir/", critical: false },
];

/// How much of the update manifest to read: well past the 16 KB after which throttled connections hang.
pub const VOLUME_BYTES: u64 = 64 * 1024;
/// One service, all steps and a retry included, is never asked longer than this.
pub const PROBE_BUDGET: Duration = Duration::from_secs(8);
const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const TCP_TIMEOUT: Duration = Duration::from_secs(3);
/// A failure faster than this (name lookup, refused or reset connection) is asked once more: one hiccup of the
/// network must not stop «Играть».
const FAST_FAIL: Duration = Duration::from_millis(1500);

/// The answer of one service.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub id: &'static str,
    pub host: String,
    pub ok: bool,
    /// Where the path is cut (None when ok): "dns" (the name is not found), "connect" (no TCP connection to the
    /// port), "tls" (TCP works, the encrypted connection is reset or refused), "timeout" (connected, no answer),
    /// "slow" (the answer started and stopped: the «16 KB» throttling), "http" (anything else).
    pub problem: Option<&'static str>,
    /// HTTP status of the last answer. Any status counts as reachable: the path to the host works.
    pub status: Option<u16>,
    pub bytes: u64,
    pub ms: u32,
    pub critical: bool,
}

/// The whole check for the window.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// "ok" | "cfx_blocked" (some Cfx.re service is cut, the internet works) | "offline" (nothing answers)
    /// | "server_down" (Cfx.re answers, the COLDZ server does not)
    pub verdict: &'static str,
    /// A service FiveM cannot start or join COLDZ without is cut: «Играть» shows the message first.
    pub blocking: bool,
    pub services: Vec<Probe>,
    /// The COLDZ server answers. None: no address yet, or a cfx.re/join code (its status comes from Cfx.re itself).
    pub server: Option<bool>,
    pub ms: u32,
}

/// What the window says, from the answers. `control` — another host outside Cfx.re answered (GitHub, where the
/// launcher takes its news): tells "Cfx.re is cut" from "no internet" when the COLDZ server is down too.
pub fn verdict(services: &[Probe], server: Option<bool>, control: bool) -> &'static str {
    let answered = services.iter().filter(|p| p.ok).count();
    if answered == services.len() {
        return if server == Some(false) { "server_down" } else { "ok" };
    }
    let internet = server == Some(true) || control || answered > 0;
    if internet {
        "cfx_blocked"
    } else {
        "offline"
    }
}

pub fn blocking(services: &[Probe]) -> bool {
    services.iter().any(|p| p.critical && !p.ok)
}

/// FiveM's address of an object in the update storage (Updater.cpp GetObjectURL): `<updates>/ab/cd/abcd…`,
/// lower case. None for anything that is not a hex hash.
pub fn object_url(hash: &str) -> Option<String> {
    let h = hash.trim().to_ascii_lowercase();
    if h.len() < 8 || h.len() > 128 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{UPDATES}/{}/{}/{h}", &h[0..2], &h[2..4]))
}

fn client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                // the server list answers with a redirect to a 21 MB file: the redirect itself is the answer
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("COLDZ-Launcher/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client")
        })
        .clone()
}

fn left(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One request, step by step.
#[derive(Debug, Default)]
struct Step {
    problem: Option<&'static str>,
    status: Option<u16>,
    bytes: u64,
    /// `x-amz-meta-branch-manifest` of the update heads: the manifest of the current build.
    manifest: Option<String>,
}

fn failed(problem: &'static str) -> Step {
    Step { problem: Some(problem), ..Default::default() }
}

/// Name lookup and a TCP connection to the port (IPv4 first, up to 3 addresses), so a failure says which step is cut.
/// The connection is closed at once: the request itself goes through the HTTP client.
async fn reach_port(host: &str, port: u16, deadline: Instant) -> Option<&'static str> {
    let lookup = tokio::time::timeout(left(deadline).min(DNS_TIMEOUT), tokio::net::lookup_host((host, port))).await;
    let mut addrs: Vec<SocketAddr> = match lookup {
        Ok(Ok(found)) => found.collect(),
        _ => return Some("dns"),
    };
    if addrs.is_empty() {
        return Some("dns");
    }
    addrs.sort_by_key(|a| a.is_ipv6());
    for addr in addrs.iter().take(3) {
        let wait = left(deadline).min(TCP_TIMEOUT);
        if wait.is_zero() {
            break;
        }
        if let Ok(Ok(_)) = tokio::time::timeout(wait, tokio::net::TcpStream::connect(addr)).await {
            return None;
        }
    }
    Some("connect")
}

/// GET `url`, then its body until the end or `want` bytes. `precheck` — look up the name and open a TCP connection
/// first (not needed for a second request to the same host).
async fn fetch(client: &reqwest::Client, url: &str, want: u64, deadline: Instant, precheck: bool) -> Step {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return failed("http");
    };
    if precheck {
        let host = parsed.host_str().unwrap_or_default().trim_matches(['[', ']']).to_string();
        let port = parsed.port_or_known_default().unwrap_or(443);
        if let Some(problem) = reach_port(&host, port, deadline).await {
            return failed(problem);
        }
    }
    let mut resp = match tokio::time::timeout(left(deadline), client.get(url).send()).await {
        Err(_) => return failed("timeout"),
        Ok(Err(e)) if e.is_timeout() => return failed("timeout"),
        // the TCP connection works (checked above), so this is the encrypted connection: reset, refused, bad certificate
        Ok(Err(e)) if e.is_connect() => return failed("tls"),
        Ok(Err(_)) => return failed("http"),
        Ok(Ok(r)) => r,
    };
    let status = resp.status().as_u16();
    let manifest = resp
        .headers()
        .get("x-amz-meta-branch-manifest")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut bytes = 0u64;
    loop {
        match tokio::time::timeout(left(deadline), resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                bytes += chunk.len() as u64;
                if want > 0 && bytes >= want {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            // the answer started and then stopped or broke: what a throttled connection looks like
            Ok(Err(_)) | Err(_) => return Step { problem: Some("slow"), status: Some(status), bytes, manifest },
        }
    }
    Step { problem: None, status: Some(status), bytes, manifest }
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

/// Asks one service once.
async fn probe_once(client: &reqwest::Client, svc: &Service) -> Probe {
    let started = Instant::now();
    let deadline = started + PROBE_BUDGET;
    let step = if svc.id == "update" {
        // FiveM: heads/fivereborn/<channel>?time=… (the answer is in the headers), then the build manifest
        let heads = fetch(client, &format!("{}?time={}", svc.url, unix_now()), 0, deadline, true).await;
        match (heads.problem, heads.manifest.as_deref().and_then(object_url)) {
            (None, Some(manifest)) => {
                let body = fetch(client, &manifest, VOLUME_BYTES, deadline, false).await;
                Step { bytes: heads.bytes + body.bytes, ..body }
            }
            _ => heads,
        }
    } else {
        fetch(client, svc.url, 0, deadline, true).await
    };
    Probe {
        id: svc.id,
        host: host_of(svc.url),
        ok: step.problem.is_none(),
        problem: step.problem,
        status: step.status,
        bytes: step.bytes,
        ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
        critical: svc.critical,
    }
}

/// Asks one service; a fast failure (not a timeout or a stop in the middle) is asked once more.
pub async fn probe(client: &reqwest::Client, svc: &Service) -> Probe {
    let first = probe_once(client, svc).await;
    let fast = Duration::from_millis(first.ms as u64) < FAST_FAIL;
    if first.ok || !fast || matches!(first.problem, Some("timeout") | Some("slow")) {
        return first;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    let second = probe_once(client, svc).await;
    Probe { ms: first.ms + 400 + second.ms, ..second }
}

/// Some HTTP answer from `url` (HEAD) within 5 seconds.
pub async fn answers(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    let req = client().head(url).timeout(Duration::from_secs(5)).send();
    matches!(tokio::time::timeout(Duration::from_secs(6), req).await, Ok(Ok(_)))
}

/// The whole check: all Cfx.re services at once, plus the COLDZ server (`server`) and a host outside Cfx.re
/// (`control`) in parallel. Takes about a second when everything answers, at most ~9 seconds when something hangs.
pub async fn run<S, C>(server: S, control: C) -> Report
where
    S: std::future::Future<Output = Option<bool>>,
    C: std::future::Future<Output = bool>,
{
    let started = Instant::now();
    let c = client();
    let (update, auth, policy, services, list, server, control) = tokio::join!(
        probe(&c, &SERVICES[0]),
        probe(&c, &SERVICES[1]),
        probe(&c, &SERVICES[2]),
        probe(&c, &SERVICES[3]),
        probe(&c, &SERVICES[4]),
        server,
        control
    );
    let services = vec![update, auth, policy, services, list];
    Report {
        verdict: verdict(&services, server, control),
        blocking: blocking(&services),
        services,
        server,
        ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
    }
}

/// The report for net-check.log (fivem::write_log): a title and one line per service.
pub fn log_lines(r: &Report) -> (String, Vec<String>) {
    let server = match r.server {
        Some(true) => "отвечает",
        Some(false) => "не отвечает",
        None => "не проверен",
    };
    let title = format!(
        "проверка связи: {}{}; сервер COLDZ {}; {} мс",
        r.verdict,
        if r.blocking { " (FiveM без этого не запустится или не войдёт на сервер)" } else { "" },
        server,
        r.ms
    );
    let lines = r
        .services
        .iter()
        .map(|p| {
            format!(
                "{} {}: {}{}, {} байт, {} мс",
                p.id,
                p.host,
                p.problem.unwrap_or("ok"),
                p.status.map(|s| format!(" (HTTP {s})")).unwrap_or_default(),
                p.bytes,
                p.ms
            )
        })
        .collect();
    (title, lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn rt() -> tokio::runtime::Runtime {
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    fn p(id: &'static str, ok: bool, critical: bool) -> Probe {
        Probe {
            id,
            host: format!("{id}.example"),
            ok,
            problem: if ok { None } else { Some("connect") },
            status: ok.then_some(200),
            bytes: 0,
            ms: 1,
            critical,
        }
    }

    #[test]
    fn verdicts() {
        let all_ok = [p("update", true, true), p("auth", true, true), p("list", true, false)];
        assert_eq!(verdict(&all_ok, Some(true), true), "ok");
        assert_eq!(verdict(&all_ok, None, false), "ok");
        assert_eq!(verdict(&all_ok, Some(false), true), "server_down");

        let auth_cut = [p("update", true, true), p("auth", false, true), p("list", true, false)];
        assert_eq!(verdict(&auth_cut, Some(true), true), "cfx_blocked");
        assert!(blocking(&auth_cut));

        let none = [p("update", false, true), p("auth", false, true), p("list", false, false)];
        // our server answers, Cfx.re does not: the provider, not COLDZ
        assert_eq!(verdict(&none, Some(true), false), "cfx_blocked");
        // our server is down too, but GitHub answers: the internet works, Cfx.re is cut
        assert_eq!(verdict(&none, Some(false), true), "cfx_blocked");
        // nothing answers at all
        assert_eq!(verdict(&none, Some(false), false), "offline");
        assert_eq!(verdict(&none, None, false), "offline");

        // only the server list is cut: the message in the window, but «Играть» is not stopped
        let list_cut = [p("update", true, true), p("auth", true, true), p("list", false, false)];
        assert_eq!(verdict(&list_cut, Some(true), true), "cfx_blocked");
        assert!(!blocking(&list_cut));
    }

    #[test]
    fn object_urls_like_fivem() {
        assert_eq!(
            object_url("83FC1656A2FF7CE5101D40B5E744F3FF509F08A0ECE06871573F7340138CF2DC").as_deref(),
            Some("https://cfx-content.akamaized.net/updates/83/fc/83fc1656a2ff7ce5101d40b5e744f3ff509f08a0ece06871573f7340138cf2dc")
        );
        for bad in ["", "abc", "../../etc/passwd", "zzzzzzzzzz", "83fc1656 a2ff"] {
            assert_eq!(object_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn services_are_the_known_hosts() {
        let hosts: Vec<String> = SERVICES.iter().map(|s| host_of(s.url)).collect();
        assert_eq!(
            hosts,
            ["cfx-content.akamaized.net", "lambda.fivem.net", "policy-live.fivem.net", "gss.cfx-services.net", "frontend.cfx-services.net"]
        );
        assert!(SERVICES.iter().all(|s| s.url.starts_with("https://")));
        assert_eq!(SERVICES.iter().filter(|s| s.critical).count(), 3);
    }

    /// What a fake server on 127.0.0.1 does with each request.
    #[derive(Clone, Copy)]
    enum Fake {
        /// answer `status` with `len` bytes of body
        Answer(u16, usize),
        /// send the headers for `len` bytes, then only `sent` bytes, then hang (the «16 KB» throttling)
        Stall(usize, usize),
        /// accept and never answer
        Silent,
        /// accept and close at once (an https:// client sees its encrypted connection broken)
        Close,
    }

    /// A fake server: accepts connections until the test ends. Connections that send nothing (the launcher's TCP
    /// check) are just closed.
    async fn fake(kind: Fake) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    if let Fake::Close = kind {
                        return;
                    }
                    let mut buf = [0u8; 4096];
                    let mut got = Vec::new();
                    loop {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                got.extend_from_slice(&buf[..n]);
                                if got.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                        }
                    }
                    match kind {
                        Fake::Answer(status, len) => {
                            let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n");
                            let _ = sock.write_all(head.as_bytes()).await;
                            let _ = sock.write_all(&vec![b'x'; len]).await;
                        }
                        Fake::Stall(len, sent) => {
                            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\r\n");
                            let _ = sock.write_all(head.as_bytes()).await;
                            let _ = sock.write_all(&vec![b'x'; sent]).await;
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                        Fake::Silent => tokio::time::sleep(Duration::from_secs(30)).await,
                        Fake::Close => {}
                    }
                });
            }
        });
        port
    }

    fn soon(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn steps_say_where_the_path_is_cut() {
        rt().block_on(async {
            let c = client();

            // any HTTP answer counts, 404 too
            let port = fake(Fake::Answer(404, 18)).await;
            let s = fetch(&c, &format!("http://127.0.0.1:{port}/"), 0, soon(3000), true).await;
            assert_eq!((s.problem, s.status, s.bytes), (None, Some(404), 18));

            // a big answer: read up to `want` bytes and stop
            let port = fake(Fake::Answer(200, 100_000)).await;
            let s = fetch(&c, &format!("http://127.0.0.1:{port}/m"), VOLUME_BYTES, soon(3000), true).await;
            assert_eq!(s.problem, None);
            assert!(s.bytes >= VOLUME_BYTES, "{}", s.bytes);

            // the «16 KB» throttling: headers and 16 KB, then nothing
            let port = fake(Fake::Stall(99_180, 16 * 1024)).await;
            let s = fetch(&c, &format!("http://127.0.0.1:{port}/m"), VOLUME_BYTES, soon(1500), true).await;
            assert_eq!(s.problem, Some("slow"));
            assert_eq!(s.bytes, 16 * 1024);

            // connected, no answer
            let port = fake(Fake::Silent).await;
            let s = fetch(&c, &format!("http://127.0.0.1:{port}/"), 0, soon(1200), true).await;
            assert_eq!(s.problem, Some("timeout"));

            // TCP works, the encrypted connection is broken (what a reset after the TLS hello looks like)
            let port = fake(Fake::Close).await;
            let s = fetch(&c, &format!("https://127.0.0.1:{port}/"), 0, soon(3000), true).await;
            assert_eq!(s.problem, Some("tls"));

            // nobody listens on the port
            let closed = {
                let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                l.local_addr().unwrap().port()
            };
            let s = fetch(&c, &format!("https://127.0.0.1:{closed}/"), 0, soon(3000), true).await;
            assert_eq!(s.problem, Some("connect"));

            // the name does not exist (.invalid never resolves)
            let s = fetch(&c, "https://coldz-check.invalid/", 0, soon(3000), true).await;
            assert_eq!(s.problem, Some("dns"));
        });
    }

    #[test]
    fn report_for_the_log() {
        let r = Report {
            verdict: "cfx_blocked",
            blocking: true,
            services: vec![Probe { problem: Some("slow"), bytes: 16384, ..p("update", false, true) }, p("auth", true, true)],
            server: Some(true),
            ms: 8100,
        };
        let (title, lines) = log_lines(&r);
        assert!(title.starts_with("проверка связи: cfx_blocked"), "{title}");
        assert!(title.contains("сервер COLDZ отвечает"), "{title}");
        assert_eq!(lines[0], "update update.example: slow, 16384 байт, 1 мс");
        assert_eq!(lines[1], "auth auth.example: ok (HTTP 200), 0 байт, 1 мс");
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["services"][0]["problem"], "slow");
        assert_eq!(json["services"][1]["problem"], serde_json::Value::Null);
    }

    /// The real Cfx.re hosts from this PC (prints the report; offline machine: nothing to check).
    #[test]
    fn real_cfx_hosts() {
        rt().block_on(async {
            let report = run(async { None }, answers("https://github.com/")).await;
            for s in &report.services {
                eprintln!("{} {} ok={} problem={:?} status={:?} bytes={} ms={}", s.id, s.host, s.ok, s.problem, s.status, s.bytes, s.ms);
            }
            eprintln!("verdict {} blocking {} in {} ms", report.verdict, report.blocking, report.ms);
            assert_eq!(report.services.len(), SERVICES.len());
            assert!(report.ms <= 2 * PROBE_BUDGET.as_millis() as u32 + 3000, "{}", report.ms);
            if report.verdict == "offline" {
                eprintln!("offline: nothing to check");
                return;
            }
            // from here (outside Russia) everything answers; the update check really downloaded the manifest
            let update = &report.services[0];
            if update.ok {
                assert!(update.bytes >= VOLUME_BYTES, "update manifest: {} bytes", update.bytes);
            }
        });
    }
}
