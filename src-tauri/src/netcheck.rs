//! Connection check before «Играть», on «Проверить связь» (15.11.2026) and once at launcher start (0.3.4).
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
//!
//! 0.3.4: also once at launcher start (quiet: only a problem shows up, as a line under «Играть»); short timeouts
//! (5 s per address); our server is asked `/info.json`, the first thing FiveM asks when it joins; the texts agreed
//! with Alexander (ui/i18n.js `net_cfx`, `net_server_down`), buttons «Всё равно играть» / «Повторить».
//!
//! 0.3.5: the reserve routes to our server (route.rs) are asked too. The main server is down but a reserve route
//! answers: the server counts as reachable (no «Сервер COLDZ сейчас недоступен» card), the window shows the reserve
//! route line.

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
/// The time ran out before `VOLUME_BYTES`, but this much had come: a slow connection, not the «16 KB» cut.
pub const STALL_BYTES: u64 = 32 * 1024;
/// One attempt at one service, all steps included, is never asked longer than this (0.3.3: 8 s).
pub const PROBE_BUDGET: Duration = Duration::from_secs(5);
/// Our server's `/info.json`.
pub const SERVER_TIMEOUT: Duration = Duration::from_secs(5);
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
    /// 0.3.5: some reserve route to the COLDZ server answers (route.rs). None: no reserve routes, or not asked.
    pub reserve: Option<bool>,
    /// 0.3.5: the route «Играть» will use: "main" | "reserve" | "none" (nothing answered). None: not asked.
    pub route: Option<&'static str>,
    /// 0.4.0: the FiveM sign-in hosts (signin.rs) and the reserve route for them. None: not asked (no groups set up).
    pub signin: Option<SigninReport>,
    pub ms: u32,
}

/// 0.4.0: one group of sign-in hosts, asked straight (its first `check` address).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GroupProbe {
    pub id: String,
    pub host: String,
    pub ok: bool,
    pub problem: Option<&'static str>,
    pub status: Option<u16>,
    pub ms: u32,
}

/// 0.4.0: the relay tunnel (tunnel.rs), asked for the first group's host.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TunnelProbe {
    pub address: String,
    /// The relay answers and the FiveM build manifest came through it past the throttling mark (tunnel.rs).
    pub ok: bool,
    pub status: Option<u16>,
    /// How much of the manifest came through: below `STALL_BYTES` the tunnel is «frozen» (problem "slow").
    pub bytes: u64,
    pub problem: Option<&'static str>,
    pub ms: u32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SigninReport {
    pub groups: Vec<GroupProbe>,
    /// None: no tunnels in news.json.
    pub tunnel: Option<TunnelProbe>,
    /// The route is on now: the hosts file points the sign-in hosts at the launcher, which forwards them.
    pub active: bool,
    /// Some group does not answer and the route is off: FiveM will not sign in. With the route on, the groups are
    /// asked through it, so `blocked` then means the route itself does not work.
    pub blocked: bool,
}

/// 0.4.0: any https address, the way a service is asked (name, TCP, TLS, answer). For the sign-in hosts.
pub async fn probe_url(url: &str) -> Probe {
    let started = Instant::now();
    let step = fetch(&client(), url, 0, started + PROBE_BUDGET, true).await;
    let first = Probe {
        id: "signin",
        host: host_of(url),
        ok: step.problem.is_none(),
        problem: step.problem,
        status: step.status,
        bytes: step.bytes,
        ms: started.elapsed().as_millis().min(u32::MAX as u128) as u32,
        critical: true,
    };
    if first.ok || Duration::from_millis(first.ms as u64) >= FAST_FAIL || matches!(first.problem, Some("timeout") | Some("slow")) {
        return first;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    let again = Instant::now();
    let step = fetch(&client(), url, 0, again + PROBE_BUDGET, true).await;
    Probe {
        ok: step.problem.is_none(),
        problem: step.problem,
        status: step.status,
        bytes: step.bytes,
        ms: first.ms + 400 + again.elapsed().as_millis().min(u32::MAX as u128) as u32,
        ..first
    }
}

/// Our server, asked by lib.rs: the main address and the reserve routes (route.rs).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ServerCheck {
    pub main: Option<bool>,
    pub reserve: Option<bool>,
    pub route: Option<&'static str>,
}

impl ServerCheck {
    /// The player can reach the COLDZ server: straight or through a reserve route.
    pub fn reachable(&self) -> Option<bool> {
        match (self.main, self.reserve) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (main, _) => main,
        }
    }
}

/// What the window says, from the answers. `control` — another host outside Cfx.re answered (GitHub, where the
/// launcher takes its news): tells "Cfx.re is cut" from "no internet" when the COLDZ server is down too.
/// Order = what stops the player first: no internet, a Cfx.re service FiveM cannot start or join without, our server
/// (0.3.4: also when only the server list or Cfx.re services are cut, they do not stop the join), the rest of Cfx.re.
pub fn verdict(services: &[Probe], server: Option<bool>, control: bool) -> &'static str {
    let answered = services.iter().filter(|p| p.ok).count();
    let internet = server == Some(true) || control || answered > 0;
    if !internet {
        "offline"
    } else if blocking(services) {
        "cfx_blocked"
    } else if server == Some(false) {
        "server_down"
    } else if answered < services.len() {
        "cfx_blocked"
    } else {
        "ok"
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
                .connect_timeout(TCP_TIMEOUT)
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

pub(crate) fn unix_now() -> u64 {
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
            // still coming when the time is up, well past the «16 KB»: a slow connection, but it works
            Err(_) if bytes >= STALL_BYTES => break,
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

/// Our server, asked the way FiveM starts every join: `GET http://<address>/info.json` (Cfx.re docs, «Proxy setup»:
/// the client first fetches `/info.json`, then `POST /client`; FiveM NetLibrary.cpp: «Failed to fetch /info.json to
/// obtain policy metadata.»). It answers = a JSON object within `wait` (`SERVER_TIMEOUT` for the connection check).
/// One request, nothing is sent. 0.3.5: route.rs asks the main server and the reserve routes with it.
pub(crate) async fn server_answers_within(url: &str, wait: Duration) -> bool {
    let fetch = async {
        let resp = client().get(url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<serde_json::Value>().await.ok()
    };
    matches!(tokio::time::timeout(wait, fetch).await, Ok(Some(v)) if v.is_object())
}

/// The whole check: all Cfx.re services at once, plus the COLDZ server (`server`) and a host outside Cfx.re
/// (`control`) in parallel. Takes about a second when everything answers, at most ~7 seconds when something hangs
/// (5 s per service, plus one quick retry after a fast failure).
pub async fn run<S, C>(server: S, control: C) -> Report
where
    S: std::future::Future<Output = ServerCheck>,
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
        verdict: verdict(&services, server.reachable(), control),
        blocking: blocking(&services),
        services,
        server: server.main,
        reserve: server.reserve,
        route: server.route,
        signin: None,
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
    let reserve = match r.reserve {
        Some(true) => "; резервный маршрут работает",
        Some(false) => "; резервный маршрут не отвечает",
        None => "",
    };
    let route = match r.route {
        Some("main") => "; «Играть»: основной сервер",
        Some("reserve") => "; «Играть»: резервный маршрут",
        Some(_) => "; «Играть»: ничего не ответило",
        None => "",
    };
    let title = format!(
        "проверка связи: {}{}; сервер COLDZ {}{}{}; {} мс",
        r.verdict,
        if r.blocking { " (FiveM без этого не запустится или не войдёт на сервер)" } else { "" },
        server,
        reserve,
        route,
        r.ms
    );
    let mut lines: Vec<String> = r
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
    if let Some(s) = &r.signin {
        for g in &s.groups {
            lines.push(format!(
                "вход {} {}: {}{}, {} мс",
                g.id,
                g.host,
                g.problem.unwrap_or("ok"),
                g.status.map(|s| format!(" (HTTP {s})")).unwrap_or_default(),
                g.ms
            ));
        }
        match &s.tunnel {
            Some(t) => lines.push(format!(
                "резервный маршрут входа {}: {}{}, {} байт, {} мс",
                t.address,
                t.problem.unwrap_or("ok"),
                t.status.map(|s| format!(" (HTTP {s})")).unwrap_or_default(),
                t.bytes,
                t.ms
            )),
            None => lines.push("резервный маршрут входа: не настроен".into()),
        }
        lines.push(format!(
            "резервный маршрут входа: {}{}",
            if s.active { "включён" } else { "выключен" },
            if s.blocked { "; вход в FiveM у провайдера не отвечает" } else { "" }
        ));
    }
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

        // 0.3.4: what stops the player comes first. The server list does not stop the join, our server does.
        assert_eq!(verdict(&list_cut, Some(false), true), "server_down");
        // a cut Cfx.re service FiveM needs stops it before our server
        assert_eq!(verdict(&auth_cut, Some(false), true), "cfx_blocked");
        assert_eq!(verdict(&auth_cut, Some(false), false), "cfx_blocked");
        // a cfx.re/join code: our server is not asked
        assert_eq!(verdict(&list_cut, None, false), "cfx_blocked");
        assert_eq!(verdict(&none, None, true), "cfx_blocked");
    }

    /// 0.3.5: the main server is down, a reserve route answers: the server is reachable, no «server_down».
    #[test]
    fn reserve_route_counts_as_reachable() {
        let ok = |main, reserve| ServerCheck { main, reserve, route: None }.reachable();
        assert_eq!(ok(Some(true), None), Some(true));
        assert_eq!(ok(Some(false), Some(true)), Some(true));
        assert_eq!(ok(Some(false), Some(false)), Some(false));
        assert_eq!(ok(Some(false), None), Some(false));
        assert_eq!(ok(None, None), None);
        let all_ok = [p("update", true, true), p("auth", true, true), p("list", true, false)];
        assert_eq!(verdict(&all_ok, ok(Some(false), Some(true)), true), "ok");
        assert_eq!(verdict(&all_ok, ok(Some(false), Some(false)), true), "server_down");

        let r = Report {
            verdict: "ok",
            blocking: false,
            services: vec![],
            server: Some(false),
            reserve: Some(true),
            route: Some("reserve"),
            signin: None,
            ms: 900,
        };
        let (title, _) = log_lines(&r);
        assert!(title.contains("сервер COLDZ не отвечает; резервный маршрут работает; «Играть»: резервный маршрут"), "{title}");
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!((json["reserve"].clone(), json["route"].clone()), (serde_json::json!(true), serde_json::json!("reserve")));
    }

    #[test]
    fn timeouts_are_short() {
        // the brief for 0.3.4: 3–5 s per request; «Играть» never waits much longer than that
        assert!(PROBE_BUDGET <= Duration::from_secs(5));
        assert!(SERVER_TIMEOUT <= Duration::from_secs(5));
        assert!(DNS_TIMEOUT <= PROBE_BUDGET && TCP_TIMEOUT <= PROBE_BUDGET);
        assert!(STALL_BYTES > 16 * 1024 && STALL_BYTES < VOLUME_BYTES);
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
        /// answer `status` with this body (our server's /info.json)
        Body(u16, &'static str),
        /// send the headers for `len` bytes, then only `sent` bytes, then hang (the «16 KB» throttling)
        Stall(usize, usize),
        /// send the headers for `len` bytes, then `chunk` bytes every `ms` milliseconds (a slow connection)
        Trickle(usize, usize, u64),
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
                        Fake::Body(status, body) => {
                            let head = format!(
                                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = sock.write_all(head.as_bytes()).await;
                            let _ = sock.write_all(body.as_bytes()).await;
                        }
                        Fake::Stall(len, sent) => {
                            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\r\n");
                            let _ = sock.write_all(head.as_bytes()).await;
                            let _ = sock.write_all(&vec![b'x'; sent]).await;
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                        Fake::Trickle(len, chunk, ms) => {
                            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\r\n");
                            let _ = sock.write_all(head.as_bytes()).await;
                            let mut sent = 0;
                            while sent < len {
                                let n = chunk.min(len - sent);
                                if sock.write_all(&vec![b'x'; n]).await.is_err() {
                                    return;
                                }
                                sent += n;
                                tokio::time::sleep(Duration::from_millis(ms)).await;
                            }
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

            // 0.3.4: a slow connection, 4 KB every 150 ms: still coming when the time is up, past 32 KB: it works
            let port = fake(Fake::Trickle(99_180, 4 * 1024, 150)).await;
            let s = fetch(&c, &format!("http://127.0.0.1:{port}/m"), VOLUME_BYTES, soon(1500), true).await;
            assert_eq!(s.problem, None, "{} bytes", s.bytes);
            assert!(s.bytes >= STALL_BYTES, "{}", s.bytes);

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

    /// Our server: `/info.json` answers a JSON object (FXServer: `{"vars": {...}, "resources": [...], ...}`).
    #[test]
    fn server_info_json() {
        rt().block_on(async {
            let url = |port: u16| format!("http://127.0.0.1:{port}/info.json");
            let wait = Duration::from_millis(1200);

            let port = fake(Fake::Body(200, r#"{"vars":{"sv_maxClients":"48"},"resources":["z_menu"],"server":"FXServer"}"#)).await;
            assert!(server_answers_within(&url(port), wait).await);

            // a restart: the HTTP port answers an error, or something that is not FXServer's info
            for body in [(503, r#"{"error":"starting"}"#), (200, "<html>starting</html>"), (200, "[1,2]")] {
                let port = fake(Fake::Body(body.0, body.1)).await;
                assert!(!server_answers_within(&url(port), wait).await, "{body:?}");
            }

            // connected, no answer: false once the time is up
            let port = fake(Fake::Silent).await;
            let started = Instant::now();
            assert!(!server_answers_within(&url(port), wait).await);
            assert!(started.elapsed() < wait + Duration::from_millis(500));

            // nobody listens on the port (the server is down)
            let closed = {
                let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                l.local_addr().unwrap().port()
            };
            assert!(!server_answers_within(&url(closed), SERVER_TIMEOUT).await);
        });
    }

    #[test]
    fn report_for_the_log() {
        let r = Report {
            verdict: "cfx_blocked",
            blocking: true,
            services: vec![Probe { problem: Some("slow"), bytes: 16384, ..p("update", false, true) }, p("auth", true, true)],
            server: Some(true),
            reserve: None,
            route: None,
            signin: None,
            ms: 8100,
        };
        let (title, lines) = log_lines(&r);
        assert!(title.starts_with("проверка связи: cfx_blocked"), "{title}");
        assert!(title.contains("сервер COLDZ отвечает; 8100"), "{title}");
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
            let report = run(async { ServerCheck::default() }, answers("https://github.com/")).await;
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
