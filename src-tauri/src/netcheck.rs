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
//!
//! 0.5.7: the check itself says what «Играть» does next (`plan`), the window asks nothing: FiveM at once, through the
//! COLDZ server (signin.rs), or a card with one text and one button. And when our addresses in different countries
//! answer within 3 ms, which no cable can do, this PC answers for them (a VPN or proxy that takes every connection
//! itself): the results are then not believed (`answered_here`).

use serde::{Deserialize, Serialize};
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
    /// 0.5.0: the ping (TCP connect, best of 3) to the main server and to every reserve route, for the card.
    pub pings: Vec<RoutePing>,
    /// 0.5.7: addresses in different countries answer at once: this PC answers for them (`answered_here`).
    pub proxy: bool,
    /// 0.5.7: what «Играть» does with this report (`plan`).
    pub plan: &'static str,
    pub ms: u32,
}

/// 0.5.7: `proxyCheck` in launcher.config.json (a `proxyCheck` in news.json wins): how fast is too fast, and where our
/// addresses are.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxyCheck {
    /// An answer within this many milliseconds is too fast for another country. 0 = the check is off.
    pub max_ms: u32,
    /// How many different places must answer that fast (at least 2: one near server can really be that near).
    pub min_places: usize,
    /// `host:port` → place («ru», «pl», «kz»). An address that is not here does not count.
    pub places: std::collections::BTreeMap<String, String>,
}

/// The addresses that answered too fast, when they are in enough different places; empty: nothing unusual.
pub fn answered_here<'a>(pings: &'a [RoutePing], cfg: &ProxyCheck) -> Vec<&'a RoutePing> {
    // a typo in news.json must not accuse everybody: 10 ms is about 1000 km there and back
    let limit = cfg.max_ms.min(10);
    if limit == 0 {
        return Vec::new();
    }
    let fast: Vec<(&RoutePing, &String)> = pings
        .iter()
        .filter(|p| p.ms.is_some_and(|ms| ms <= limit))
        .filter_map(|p| cfg.places.get(&p.address).map(|place| (p, place)))
        .collect();
    let mut places: Vec<&String> = fast.iter().map(|(_, place)| *place).collect();
    places.sort();
    places.dedup();
    if places.len() >= cfg.min_places.max(2) {
        fast.into_iter().map(|(p, _)| p).collect()
    } else {
        Vec::new()
    }
}

/// 0.5.7: what «Играть» does (Alexander 23.11: no questions in the window):
/// * "go" — FiveM at once (also when only the main server is silent and a reserve route answers: route.rs picks it);
/// * "via" — the sign-in hosts do not answer, a COLDZ relay does: the sign-in through the COLDZ server, then FiveM;
/// * a card with one text and one button: "offline" (nothing answers), "server" (COLDZ is down: nothing on the player's
///   side helps), "noroute" (the sign-in does not answer and neither does a relay), "via_bad" (the sign-in through the
///   COLDZ server is on and a whole group is silent through it: one slow address is no reason to stop, there is nothing
///   better to offer), "cfx" (FiveM's own services are cut), "proxy" (something is wrong, and this
///   PC answers for our servers: the results mean nothing until that is switched off).
pub fn plan(r: &Report) -> &'static str {
    let signin = r.signin.as_ref();
    let cut = signin.is_some_and(|s| s.blocked);
    let relay = signin.and_then(|s| s.tunnel.as_ref()).is_some_and(|t| t.ok);
    let server_down = r.server == Some(false) && r.reserve != Some(true);
    let plan = if r.verdict == "offline" {
        "offline"
    } else if server_down && (if cut { relay } else { !r.blocking }) {
        "server"
    } else if cut {
        if relay {
            "via"
        } else {
            "noroute"
        }
    } else if signin.is_some_and(|s| s.active && s.groups.iter().any(|g| g.silent())) {
        "via_bad"
    } else if r.blocking {
        "cfx"
    } else {
        "go"
    };
    if r.proxy && plan != "go" {
        "proxy"
    } else {
        plan
    }
}

/// 0.5.0: one line of the ping list: "main" or "reserve" (number = place in the reserve list, from 1).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoutePing {
    pub kind: &'static str,
    pub index: usize,
    pub address: String,
    /// None: no answer within 2 s.
    pub ms: Option<u32>,
}

/// 0.4.0: one group of sign-in hosts, asked straight. 0.5.7: all its `check` addresses at once; the group is down when any of
/// them fails.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GroupProbe {
    pub id: String,
    /// The first address that failed, or the first one when all answer (the same for the next three).
    pub host: String,
    pub ok: bool,
    pub problem: Option<&'static str>,
    pub status: Option<u16>,
    pub ms: u32,
    /// Every address as it answered, for net-check.log.
    #[serde(skip)]
    pub checks: Vec<Probe>,
}

impl GroupProbe {
    /// The group from the answers of its addresses (at least one).
    pub fn of(id: &str, checks: Vec<Probe>) -> Option<Self> {
        let shown = checks.iter().find(|p| !p.ok).or(checks.first())?;
        Some(GroupProbe {
            id: id.to_string(),
            host: shown.host.clone(),
            ok: shown.ok,
            problem: shown.problem,
            status: shown.status,
            ms: checks.iter().map(|p| p.ms).max().unwrap_or(0),
            checks,
        })
    }

    /// Not one of its addresses answers.
    pub fn silent(&self) -> bool {
        !self.ok && self.checks.iter().all(|p| !p.ok)
    }
}

/// 0.4.0: the relay tunnel (tunnel.rs), asked for the first group's host.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TunnelProbe {
    /// 0.5.0: the relay's number in `signin.tunnels` (1, 2, ...), for the log and support.
    pub index: usize,
    pub address: String,
    /// The relay answers and the FiveM build manifest came through it past the throttling mark (tunnel.rs).
    pub ok: bool,
    pub status: Option<u16>,
    /// How much of the manifest came through: below `STALL_BYTES` the tunnel is «frozen» (problem "slow").
    pub bytes: u64,
    /// Why the probe stopped (tunnel.rs `Trace::stop`): relay, host_tls, read, write, http, manifest, timeout, slow.
    pub problem: Option<&'static str>,
    /// This relay's own time.
    pub ms: u32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SigninReport {
    pub groups: Vec<GroupProbe>,
    /// The tunnel the route uses (the first that carries data), or the one that came closest. None: no tunnels.
    pub tunnel: Option<TunnelProbe>,
    /// 0.5.0: every relay as it answered (net-check.log: number, address, stop reason, bytes, ms).
    #[serde(default)]
    pub tunnels: Vec<TunnelProbe>,
    /// 0.5.0: what the running route carried so far (tunnel.rs `Stats::line`). None: the route is off.
    pub route_stats: Option<String>,
    /// The route is on now: the hosts file points the sign-in hosts at the launcher, which forwards them.
    pub active: bool,
    /// Some group does not answer and the route is off: FiveM will not sign in. With the route on, the groups are
    /// asked through it, so `blocked` then means the route itself does not work.
    pub blocked: bool,
}

/// 0.4.0: any https address, the way a service is asked (name, TCP, TLS, answer). For the sign-in hosts. 0.5.7: of a big
/// answer no more than `VOLUME_BYTES` is read; one that stops below `STALL_BYTES` is a failure ("slow": the «16 KB» cut).
pub async fn probe_url(url: &str) -> Probe {
    let started = Instant::now();
    let step = fetch(&client(), url, VOLUME_BYTES, started + PROBE_BUDGET, true).await;
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
    let step = fetch(&client(), url, VOLUME_BYTES, again + PROBE_BUDGET, true).await;
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
        pings: Vec::new(),
        // filled by lib.rs once the sign-in and the pings are known
        proxy: false,
        plan: "go",
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
    let mut lines: Vec<String> = Vec::new();
    // 0.5.0: the ping to the main server and every reserve route
    if !r.pings.is_empty() {
        let parts: Vec<String> = r
            .pings
            .iter()
            .map(|p| {
                let name = if p.kind == "main" { "основной".to_string() } else { format!("резерв №{}", p.index) };
                format!("{name} {} {}", p.address, p.ms.map_or("нет ответа".to_string(), |m| format!("{m} мс")))
            })
            .collect();
        lines.push(format!("пинг: {}", parts.join(", ")));
    }
    // 0.5.7: which way «Играть» goes, and why the numbers above may mean nothing
    if r.proxy {
        lines.push(
            "похоже, на ПК VPN или прокси: адреса в разных странах отвечают сразу, отвечает сам ПК; результатам проверки не верить"
                .into(),
        );
    }
    lines.push(format!("план «Играть»: {}", r.plan));
    lines.extend(r
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
        }));
    if let Some(s) = &r.signin {
        for g in &s.groups {
            // 0.5.7: every address of the group, with the bytes (a big one that stops at 16 KB is «slow»)
            for p in &g.checks {
                lines.push(format!(
                    "вход {} {}: {}{}, {} байт, {} мс",
                    g.id,
                    p.host,
                    p.problem.unwrap_or("ok"),
                    p.status.map(|s| format!(" (HTTP {s})")).unwrap_or_default(),
                    p.bytes,
                    p.ms
                ));
            }
        }
        // 0.5.0: every relay with its number and why it stopped
        for t in &s.tunnels {
            lines.push(format!(
                "резервный маршрут входа, релей №{} {}: {}{}, {} байт, {} мс{}",
                t.index,
                t.address,
                t.problem.unwrap_or("ok"),
                t.status.map(|s| format!(" (HTTP {s})")).unwrap_or_default(),
                t.bytes,
                t.ms,
                if s.tunnel.as_ref().map_or(false, |c| c.ok && c.index == t.index) { "; выбран" } else { "" }
            ));
        }
        if s.tunnels.is_empty() {
            lines.push("резервный маршрут входа: не настроен".into());
        }
        if let Some(st) = &s.route_stats {
            lines.push(st.clone());
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
pub(crate) mod tests {
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
            pings: Vec::new(),
            proxy: false,
            plan: "go",
            ms: 900,
        };
        let (title, _) = log_lines(&r);
        assert!(title.contains("сервер COLDZ не отвечает; резервный маршрут работает; «Играть»: резервный маршрут"), "{title}");
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!((json["reserve"].clone(), json["route"].clone()), (serde_json::json!(true), serde_json::json!("reserve")));
    }

    /// 0.5.7: the reports of the brief, (а)–(ж), and what «Играть» does with each.
    #[test]
    fn what_play_does() {
        let tunnel = |ok: bool| TunnelProbe {
            index: 1,
            address: "10.0.0.1:8443".into(),
            ok,
            status: ok.then_some(403),
            bytes: if ok { 99_000 } else { 0 },
            problem: (!ok).then_some("relay"),
            ms: 300,
        };
        let group = |id: &str, ok: bool| GroupProbe {
            id: id.into(),
            host: format!("{id}.example"),
            ok,
            problem: (!ok).then_some("connect"),
            status: ok.then_some(403),
            ms: 5,
            checks: vec![],
        };
        // rockstar answers, cfx answers, a relay answers, the route is on
        let signin = |rockstar: bool, cfx: bool, relay: bool, active: bool| {
            Some(SigninReport {
                blocked: !(rockstar && cfx) && !active,
                groups: vec![group("rockstar", rockstar), group("cfx", cfx)],
                tunnel: Some(tunnel(relay)),
                tunnels: vec![tunnel(relay)],
                route_stats: None,
                active,
            })
        };
        // update, auth answer; main server, reserve routes
        let report = |update: bool, auth: bool, main: bool, reserve: Option<bool>, s: Option<SigninReport>| {
            let services = vec![p("update", update, true), p("auth", auth, true), p("policy", auth, true), p("list", update, false)];
            let server = ServerCheck { main: Some(main), reserve, route: None };
            let internet = update || auth || s.as_ref().is_some_and(|s| s.groups.iter().any(|g| g.ok) || s.tunnel.as_ref().is_some_and(|t| t.ok));
            let mut r = Report {
                verdict: verdict(&services, server.reachable(), internet),
                blocking: blocking(&services),
                services,
                server: server.main,
                reserve: server.reserve,
                route: None,
                signin: s,
                pings: Vec::new(),
                proxy: false,
                plan: "",
                ms: 1,
            };
            r.plan = plan(&r);
            r
        };
        let up = Some(true);

        // (а) everything answers
        assert_eq!(report(true, true, true, up, signin(true, true, true, false)).plan, "go");
        // no sign-in setup, no reserve routes, only the server list is cut: FiveM at once
        assert_eq!(report(true, true, true, None, None).plan, "go");
        // (б) the sign-in is cut, the relay answers: through the COLDZ server, no questions
        assert_eq!(report(true, true, true, up, signin(false, true, true, false)).plan, "via");
        // also when the license service is cut (the same route carries it)
        assert_eq!(report(true, false, true, up, signin(true, false, true, false)).plan, "via");
        // (в), (г) the prompt is declined, port 443 is taken: the same plan, the window shows the error of signin_enable
        // (д) the relay is dead too
        assert_eq!(report(true, true, true, up, signin(false, true, false, false)).plan, "noroute");
        // (е) the main server is silent, a reserve route answers: FiveM at once, through it
        assert_eq!(report(true, true, false, up, signin(true, true, true, false)).plan, "go");
        // both: through the reserve route and the sign-in through the COLDZ server
        assert_eq!(report(true, true, false, up, signin(false, true, true, false)).plan, "via");
        // (ж) nothing answers
        assert_eq!(report(false, false, false, Some(false), signin(false, false, false, false)).plan, "offline");

        // COLDZ is down: no Windows prompt for nothing
        assert_eq!(report(true, true, false, Some(false), signin(true, true, true, false)).plan, "server");
        assert_eq!(report(true, true, false, Some(false), signin(false, true, true, false)).plan, "server");
        // the player's provider comes first when nothing of ours can help
        assert_eq!(report(true, true, false, Some(false), signin(false, true, false, false)).plan, "noroute");
        assert_eq!(report(false, false, false, Some(false), signin(true, true, true, false)).plan, "cfx");
        // FiveM's updates are cut, the sign-in answers: the agreed text about the provider
        assert_eq!(report(false, true, true, up, signin(true, true, true, false)).plan, "cfx");
        // the route is on: it answers, or it does not
        assert_eq!(report(true, true, true, up, signin(true, true, true, true)).plan, "go");
        assert_eq!(report(true, true, true, up, signin(false, true, true, true)).plan, "via_bad");
        assert_eq!(report(false, false, true, up, signin(false, false, false, true)).plan, "via_bad");
        // one address of the group does not answer through it, the others do: FiveM at once
        let mut half = report(true, true, true, up, signin(true, true, true, true));
        if let Some(s) = half.signin.as_mut() {
            s.groups[0] = GroupProbe::of("rockstar", vec![p("a", true, true), p("b", false, true)]).unwrap();
            assert!(!s.groups[0].ok && !s.groups[0].silent());
        }
        assert_eq!(plan(&half), "go");

        // a VPN or proxy on the PC: what is wrong cannot be believed, what works still works
        let with_proxy = |mut r: Report| {
            r.proxy = true;
            plan(&r)
        };
        assert_eq!(with_proxy(report(true, true, true, up, signin(false, true, true, false))), "proxy");
        assert_eq!(with_proxy(report(true, true, true, up, signin(false, true, false, false))), "proxy");
        assert_eq!(with_proxy(report(false, false, false, Some(false), None)), "proxy");
        assert_eq!(with_proxy(report(true, true, true, up, signin(true, true, true, false))), "go");
        let json = serde_json::to_value(report(true, true, true, up, None)).unwrap();
        assert_eq!((json["plan"].clone(), json["proxy"].clone()), (serde_json::json!("go"), serde_json::json!(false)));
    }

    /// 0.5.7: a group of sign-in hosts is down when any of its addresses is.
    #[test]
    fn a_group_fails_with_any_of_its_addresses() {
        let (ok, bad) = (p("a", true, true), p("b", false, true));
        let g = GroupProbe::of("rockstar", vec![ok.clone(), ok.clone()]).unwrap();
        assert!(g.ok && g.host == "a.example" && g.checks.len() == 2);
        // the second one fails: the group fails, and the window and the log name it
        let g = GroupProbe::of("rockstar", vec![ok.clone(), bad.clone(), ok.clone()]).unwrap();
        assert_eq!((g.ok, g.host.as_str(), g.problem), (false, "b.example", Some("connect")));
        assert_eq!(GroupProbe::of("rockstar", vec![]), None);
        let r = SigninReport { groups: vec![g], tunnel: None, tunnels: vec![], route_stats: None, active: false, blocked: true };
        let report = Report {
            verdict: "ok",
            blocking: false,
            services: vec![],
            server: Some(true),
            reserve: None,
            route: None,
            signin: Some(r),
            pings: vec![],
            proxy: false,
            plan: "noroute",
            ms: 1,
        };
        let (_, lines) = log_lines(&report);
        assert!(lines.contains(&"вход rockstar a.example: ok (HTTP 200), 0 байт, 1 мс".to_string()), "{lines:?}");
        assert!(lines.contains(&"вход rockstar b.example: connect, 0 байт, 1 мс".to_string()), "{lines:?}");
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["signin"]["groups"][0]["host"], "b.example");
        assert!(json["signin"]["groups"][0].get("checks").is_none());
    }

    /// 0.5.7, a player's screenshot: Moscow, Warsaw and Almaty in 1 ms each.
    #[test]
    fn answers_too_fast_for_the_distance() {
        let cfg: ProxyCheck = serde_json::from_value(serde_json::json!({
            "maxMs": 3, "minPlaces": 2,
            "places": { "92.118.11.28:28683": "ru", "177.3.198.34:30120": "pl", "46.247.40.34:30120": "kz", "92.118.11.28:1": "ru" }
        }))
        .unwrap();
        let ping = |address: &str, ms: Option<u32>| RoutePing { kind: "reserve", index: 1, address: address.into(), ms };
        let check = |a: Option<u32>, b: Option<u32>, c: Option<u32>| {
            let pings = [ping("92.118.11.28:28683", a), ping("177.3.198.34:30120", b), ping("46.247.40.34:30120", c)];
            answered_here(&pings, &cfg).len()
        };
        assert_eq!(check(Some(1), Some(1), Some(1)), 3);
        assert_eq!(check(Some(3), Some(2), Some(80)), 2);
        // a player next door to one of them, or ordinary pings
        assert_eq!(check(Some(1), Some(34), Some(80)), 0);
        assert_eq!(check(Some(4), Some(4), Some(4)), 0);
        assert_eq!(check(None, None, None), 0);
        // two addresses in one place are one place; an unknown address does not count
        assert_eq!(answered_here(&[ping("92.118.11.28:28683", Some(1)), ping("92.118.11.28:1", Some(1))], &cfg).len(), 0);
        assert_eq!(answered_here(&[ping("92.118.11.28:28683", Some(1)), ping("1.2.3.4:5", Some(1))], &cfg).len(), 0);
        // off, and limits that make no sense
        let fast = [ping("92.118.11.28:28683", Some(1)), ping("177.3.198.34:30120", Some(1))];
        assert!(answered_here(&fast, &ProxyCheck::default()).is_empty());
        assert!(answered_here(&fast, &ProxyCheck { max_ms: 0, ..cfg.clone() }).is_empty());
        let one = ProxyCheck { max_ms: 5000, min_places: 0, ..cfg.clone() };
        assert_eq!(answered_here(&[ping("92.118.11.28:28683", Some(1))], &one).len(), 0);
        assert_eq!(answered_here(&[ping("92.118.11.28:28683", Some(50)), ping("177.3.198.34:30120", Some(50))], &one).len(), 0);
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
    pub enum Fake {
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
    pub async fn fake(kind: Fake) -> u16 {
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
            let s = fetch(&c, &format!("http://127.0.0.1:{port}/m"), 2 * VOLUME_BYTES, soon(3000), true).await;
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
            pings: Vec::new(),
            proxy: false,
            plan: "cfx",
            ms: 8100,
        };
        let (title, lines) = log_lines(&r);
        assert!(title.starts_with("проверка связи: cfx_blocked"), "{title}");
        assert!(title.contains("сервер COLDZ отвечает; 8100"), "{title}");
        assert_eq!(lines[0], "план «Играть»: cfx");
        assert_eq!(lines[1], "update update.example: slow, 16384 байт, 1 мс");
        assert_eq!(lines[2], "auth auth.example: ok (HTTP 200), 0 байт, 1 мс");
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["services"][0]["problem"], "slow");
        assert_eq!(json["services"][1]["problem"], serde_json::Value::Null);
    }

    /// 0.5.0: every relay of the sign-in route in the log, with its number and why it stopped.
    #[test]
    fn tunnel_lines_name_the_relay_and_the_stop() {
        let t = |index: usize, ok: bool, problem: Option<&'static str>, bytes: u64| TunnelProbe {
            index,
            address: format!("10.0.0.{index}:8443"),
            ok,
            status: Some(403),
            bytes,
            problem,
            ms: 700 + index as u32,
        };
        let r = Report {
            verdict: "ok",
            blocking: false,
            services: vec![],
            server: Some(true),
            reserve: None,
            route: None,
            signin: Some(SigninReport {
                groups: vec![],
                tunnel: Some(t(2, true, None, 99_000)),
                tunnels: vec![t(1, false, Some("timeout"), 18_000), t(2, true, None, 99_000)],
                route_stats: Some("работа маршрута через 10.0.0.2:8443: соединений 3".into()),
                active: true,
                blocked: false,
            }),
            pings: vec![
                RoutePing { kind: "main", index: 0, address: "92.118.11.28:28683".into(), ms: Some(34) },
                RoutePing { kind: "reserve", index: 1, address: "177.3.198.34:30120".into(), ms: None },
            ],
            proxy: false,
            plan: "go",
            ms: 1,
        };
        let (_, lines) = log_lines(&r);
        assert!(lines.contains(&"резервный маршрут входа, релей №1 10.0.0.1:8443: timeout (HTTP 403), 18000 байт, 701 мс".to_string()), "{lines:?}");
        assert!(lines.contains(&"резервный маршрут входа, релей №2 10.0.0.2:8443: ok (HTTP 403), 99000 байт, 702 мс; выбран".to_string()), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("работа маршрута через 10.0.0.2:8443")), "{lines:?}");
        assert_eq!(lines[0], "пинг: основной 92.118.11.28:28683 34 мс, резерв №1 177.3.198.34:30120 нет ответа");
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
