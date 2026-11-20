//! COLDZ launcher backend.
//!
//! The launcher never touches FiveM program files or game files. It only:
//!   * looks for an installed FiveM (fivem.rs: the path chosen by hand, registry, a running FiveM, usual folders and
//!     shortcuts; where it looked goes to fivem-search.log next to settings.json),
//!   * starts the real FiveM client with `fivem://connect/<address>` — not a second time while FiveM is starting or
//!     stuck (running.rs: that is FiveM's "LinkProtocolIPC" error); can close stuck FiveM processes on request,
//!   * reads the public server endpoints (/dynamic.json, /players.json, /info.json),
//!   * downloads a news JSON file (it may also carry the server address and the rules,
//!     so both can be changed without a new launcher build),
//!   * sends the player's nickname to our own server before "Play" (nick.rs: `POST /z_menu/nick`),
//!   * at start, before "Play" and on «Проверить связь» asks the Cfx.re services FiveM needs and our server
//!     separately (netcheck.rs): a provider that cuts Cfx.re is not the COLDZ server's fault,
//!   * on «Играть» asks the main server first and, when it does not answer, the reserve routes from news.json
//!     (route.rs, 0.3.5), and connects through the first that answers,
//!   * 0.5.4: before starting FiveM sets the game build the server asks for (`/info.json` vars) in FiveM's settings
//!     file `FiveM.app\CitizenFX.ini` (gamebuild.rs: only the keys FiveM itself writes when it joins), so FiveM
//!     downloads the build data in its start window, not inside the game,
//!   * 0.5.4: when the server's build needs FiveM's override `update.rpf` (storage.rs), downloads it into FiveM's cache
//!     before FiveM starts, from FiveM's own address, continuing after a break, checked by SHA-1,
//!   * 0.5.5: says under «Играть» how much the first join downloads and about how long it takes, from the speed to our
//!     file server (firstjoin.rs: a test file of `cdnSpeedUrl`, once in 10 minutes; FiveM's own download is not touched),
//!   * 0.5.6: after a FiveM crash asks under «Играть» whether to send a report to the developers (crashreport.rs: FiveM's
//!     crash dump and log of that session, read and cleaned here; only on the player's «Отправить», the dump only with
//!     its own tick; switched off by `crashReport.enabled: false` in news.json),
//!   * updates itself through the Tauri updater (signed with our own key).

mod crashreport;
mod firstjoin;
mod fivem;
mod gamebuild;
mod hosts;
mod netcheck;
mod nick;
mod route;
mod running;
mod signin;
mod startup;
mod storage;
mod support;
mod tunnel;
mod window;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex;

const CONFIG_JSON: &str = include_str!("../../launcher.config.json");
const BUNDLED_NEWS: &str = include_str!("../../news/news.json");

const HTTP_TIMEOUT: Duration = Duration::from_secs(4);
const PING_TIMEOUT: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------- settings

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Link {
    /// Icon name from ui/icons.js: vk, facebook, youtube, tiktok, discord, telegram, instagram, x.
    icon: String,
    label: String,
    /// Optional small text on the icon, e.g. "EN" / "PT".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    badge: Option<String>,
    /// Empty = button hidden.
    #[serde(default)]
    url: String,
    /// Window languages the button shows in (ru, en, es, pt-BR); empty = all. 20.11: Instagram per language.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    langs: Vec<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Config {
    server_name: String,
    /// Built-in address. Empty = not known yet (the launcher then waits for
    /// `serverAddress` in news.json). A non-empty `serverAddress` in news.json wins.
    #[serde(default)]
    server_address: String,
    /// 0.3.5: built-in reserve routes to the server (route.rs), `host:port`. A `relays` list in news.json wins
    /// (an empty list there = no reserve routes).
    #[serde(default)]
    relays: Vec<String>,
    /// 0.4.0: built-in sign-in hosts and relay tunnels (signin.rs). A `signin` object in news.json wins.
    #[serde(default)]
    signin: signin::Setup,
    max_players: u32,
    owner_email: String,
    fivem_download_url: String,
    /// 0.5.0: where the portable build sends the player for a new version (it cannot update itself).
    #[serde(default)]
    portable_download_url: String,
    /// The first address of news.json (GitHub). Kept for older tools; the launcher asks `news_urls` too.
    news_url: String,
    /// 0.5.0: more addresses of the same news.json (coldz.vip): asked at the same time, the first good answer wins.
    #[serde(default)]
    news_urls: Vec<String>,
    /// 0.5.0: the public key news.json is signed with (minisign, base64 like the updater's `pubkey`; its own key,
    /// not the updater's). Empty: no check (only for a test build). A file without a good signature is not used:
    /// the launcher stays on the last good copy or the one built in.
    #[serde(default)]
    news_pubkey: String,
    #[serde(default)]
    links: Vec<Link>,
    /// 0.5.4 (storage.rs): FiveM's table of override files (build → sha1, address, size). `gameStorage` in news.json
    /// wins.
    #[serde(default)]
    game_storage: Vec<storage::Entry>,
    /// 0.5.4 (gamebuild.rs): `{"preset": false}` — the launcher does not touch CitizenFX.ini (starts FiveM as 0.5.3).
    /// `gameBuild.preset` in news.json wins.
    #[serde(default)]
    game_build: GameBuildSwitch,
    /// 0.5.5 (firstjoin.rs): the test file on our file server for the speed line; "" = no measuring. `cdnSpeedUrl` in
    /// news.json wins (an empty one there switches it off).
    #[serde(default)]
    cdn_speed_url: String,
    /// 0.5.5 (firstjoin.rs): about how many GB the first join downloads from our file server; 0 = no first-join line.
    /// `firstJoinGb` in news.json wins.
    #[serde(default)]
    first_join_gb: f64,
    /// 0.5.6 (crashreport.rs): crash reports to our receiver: `enabled`, `url`, `key` (no secret), `dump` (the dump may go
    /// with its tick). `crashReport` in news.json wins (`{"enabled": false}` switches it off). Missing: off.
    #[serde(default)]
    crash_report: crashreport::Switch,
}

/// 0.5.4: the remote switch for writing CitizenFX.ini (gamebuild.rs).
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct GameBuildSwitch {
    /// Set the server's game build in CitizenFX.ini before FiveM starts. Missing: on.
    #[serde(default = "preset_default")]
    preset: bool,
}

fn preset_default() -> bool {
    true
}

impl Default for GameBuildSwitch {
    fn default() -> Self {
        GameBuildSwitch { preset: true }
    }
}

fn config() -> &'static Config {
    static CFG: OnceLock<Config> = OnceLock::new();
    CFG.get_or_init(|| serde_json::from_str(CONFIG_JSON).expect("launcher.config.json is invalid"))
}

/// Where to connect: a plain `host:port` or a `cfx.re/join/<code>` link.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Direct { host: String, port: u16 },
    JoinCode(String),
}

fn parse_address(raw: &str) -> Target {
    let mut a = raw.trim();
    for prefix in ["fivem://connect/", "https://", "http://"] {
        if let Some(rest) = a.strip_prefix(prefix) {
            a = rest;
        }
    }
    let a = a.trim_end_matches('/');
    if let Some(code) = a.strip_prefix("cfx.re/join/") {
        return Target::JoinCode(code.to_string());
    }
    // A bare join code: short, letters/digits only, no dots or colon.
    if !a.contains('.') && !a.contains(':') && !a.is_empty() && a.len() <= 8 {
        return Target::JoinCode(a.to_string());
    }
    match a.rsplit_once(':') {
        Some((h, p)) => Target::Direct {
            host: h.to_string(),
            port: p.parse().unwrap_or(30120),
        },
        None => Target::Direct { host: a.to_string(), port: 30120 },
    }
}

fn connect_url(t: &Target) -> String {
    match t {
        Target::Direct { host, port } => format!("fivem://connect/{host}:{port}"),
        Target::JoinCode(code) => format!("fivem://connect/cfx.re/join/{code}"),
    }
}

/// Accepts only a sane `host:port`, host name, IP or cfx.re join code, so a typo
/// (or anything strange) in news.json can never end up in the fivem:// link.
fn valid_address(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 100 {
        return false;
    }
    if let Some((_, p)) = raw.rsplit_once(':') {
        if !raw.contains("://") && p.parse::<u16>().map_or(true, |n| n == 0) {
            return false;
        }
    }
    match parse_address(raw) {
        Target::JoinCode(code) => {
            !code.is_empty() && code.len() <= 16 && code.chars().all(|c| c.is_ascii_alphanumeric())
        }
        Target::Direct { host, port } => {
            port != 0
                && !host.is_empty()
                && host.len() <= 253
                && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
                && !host.starts_with(['.', '-'])
        }
    }
}

/// Address from news.json (remote, cached or bundled). `None` = use the built-in one.
fn remote_address() -> &'static RwLock<Option<String>> {
    static ADDR: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    ADDR.get_or_init(|| RwLock::new(None))
}

/// Reserve routes from news.json. `None` = the key is missing: use the built-in ones.
fn remote_relays() -> &'static RwLock<Option<Vec<String>>> {
    static RELAYS: OnceLock<RwLock<Option<Vec<String>>>> = OnceLock::new();
    RELAYS.get_or_init(|| RwLock::new(None))
}

/// 0.4.0: `signin` from news.json. `None` = the key is missing: use the built-in setup.
fn remote_signin() -> &'static RwLock<Option<signin::Setup>> {
    static S: OnceLock<RwLock<Option<signin::Setup>>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(None))
}

/// 0.5.4: `gameStorage` from news.json. `None` = the key is missing: the built-in table.
fn remote_storage() -> &'static RwLock<Option<Vec<storage::Entry>>> {
    static S: OnceLock<RwLock<Option<Vec<storage::Entry>>>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(None))
}

/// 0.5.4: `gameBuild.preset` from news.json. `None` = no such key (or not true/false): launcher.config.json decides.
fn remote_preset() -> &'static RwLock<Option<bool>> {
    static S: OnceLock<RwLock<Option<bool>>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(None))
}

/// 0.5.5: `cdnSpeedUrl`, `firstJoinGb`, `updateHint` from news.json (firstjoin.rs). Default: no keys, the built-in ones.
fn remote_join() -> &'static RwLock<firstjoin::Feed> {
    static S: OnceLock<RwLock<firstjoin::Feed>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(firstjoin::Feed::default()))
}

/// The test file, the first-join size and the update line in use: news.json wins over launcher.config.json.
fn join_setup() -> firstjoin::Setup {
    let feed = remote_join().read().map(|g| g.clone()).unwrap_or_default();
    firstjoin::setup(&feed, &config().cdn_speed_url, config().first_join_gb)
}

/// `gameBuild.preset` of a news file: Some only for a real true/false.
fn preset_of_feed(v: &serde_json::Value) -> Option<bool> {
    v.get("gameBuild").and_then(|g| g.get("preset")).and_then(|p| p.as_bool())
}

/// Does the launcher write the game build to CitizenFX.ini (gamebuild.rs)? news.json wins over launcher.config.json.
fn preset_on(remote: Option<bool>, built_in: &GameBuildSwitch) -> bool {
    remote.unwrap_or(built_in.preset)
}

/// FiveM's override files in use (storage.rs `clean`): from news.json, or the built-in ones.
fn effective_storage() -> Vec<storage::Entry> {
    let remote = remote_storage().read().ok().and_then(|g| g.clone());
    storage::clean(remote.as_ref().unwrap_or(&config().game_storage))
}

/// The sign-in hosts and tunnels in use (signin.rs `clean`): from news.json, or the built-in ones.
fn effective_signin() -> signin::Setup {
    let remote = remote_signin().read().ok().and_then(|g| g.clone());
    signin::clean(remote.as_ref().unwrap_or(&config().signin))
}

/// Reads `serverAddress` from a news file. Missing, empty or invalid = back to the built-in address.
/// 0.3.5: also `relays` (a list of `host:port`): missing or not a list = the built-in reserve routes.
/// 0.4.0: also `signin` (an object: groups and tunnels): missing or not an object = the built-in setup.
fn apply_feed(v: &serde_json::Value) {
    let addr = v
        .get("serverAddress")
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|a| valid_address(a))
        .map(str::to_string);
    if let Ok(mut slot) = remote_address().write() {
        *slot = addr;
    }
    let relays = v
        .get("relays")
        .and_then(|r| r.as_array())
        .map(|list| list.iter().filter_map(|r| r.as_str()).map(str::to_string).collect::<Vec<_>>());
    if let Ok(mut slot) = remote_relays().write() {
        *slot = relays;
    }
    let signin = v
        .get("signin")
        .filter(|s| s.is_object())
        .and_then(|s| serde_json::from_value::<signin::Setup>(s.clone()).ok());
    if let Ok(mut slot) = remote_signin().write() {
        *slot = signin;
    }
    // 0.5.4: `gameStorage` (a list): missing or not a list = the built-in table
    let table = v
        .get("gameStorage")
        .filter(|t| t.is_array())
        .and_then(|t| serde_json::from_value::<Vec<storage::Entry>>(t.clone()).ok());
    if let Ok(mut slot) = remote_storage().write() {
        *slot = table;
    }
    // 0.5.4: `gameBuild: {"preset": false}` — CitizenFX.ini is not touched; missing or not true/false = built-in
    if let Ok(mut slot) = remote_preset().write() {
        *slot = preset_of_feed(v);
    }
    // 0.5.5: `cdnSpeedUrl` ("" = no measuring), `firstJoinGb` (0 = no first-join line), `updateHint`; missing or bad =
    // launcher.config.json
    if let Ok(mut slot) = remote_join().write() {
        *slot = firstjoin::Feed::of(v);
    }
    // 0.5.6: `crashReport` ({"enabled": false} = no questions, no sending); missing or bad keys = launcher.config.json
    if let Ok(mut slot) = remote_crash().write() {
        *slot = crashreport::Feed::of(v);
    }
}

/// Only plain `host:port` addresses (a cfx.re/join code cannot be a reserve route), written as `host:port`, without
/// repeats and without the main address, at most `route::MAX_RELAYS`.
fn clean_relays(raw: &[String], primary: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in raw.iter().map(|r| r.trim()).filter(|r| valid_address(r)) {
        let Target::Direct { host, port } = parse_address(r) else { continue };
        let a = format!("{host}:{port}");
        if Some(a.as_str()) != primary && !out.contains(&a) {
            out.push(a);
        }
        if out.len() >= route::MAX_RELAYS {
            break;
        }
    }
    out
}

/// The main address as `host:port` (or `cfx.re/join/<code>`).
fn primary_address() -> Option<String> {
    effective_address().map(|a| display_address(&parse_address(&a)))
}

/// The reserve routes in use: from news.json, or the built-in ones.
fn effective_relays() -> Vec<String> {
    let remote = remote_relays().read().ok().and_then(|g| g.clone());
    clean_relays(remote.as_deref().unwrap_or(&config().relays), primary_address().as_deref())
}

/// The route chosen last (route.rs): by «Играть» or by the connection check.
fn current_route() -> &'static RwLock<Option<route::Route>> {
    static ROUTE: OnceLock<RwLock<Option<route::Route>>> = OnceLock::new();
    ROUTE.get_or_init(|| RwLock::new(None))
}

/// The chosen route, while it is still the main address or one of the reserve routes (news.json may change both).
fn valid_route() -> Option<route::Route> {
    let r = current_route().read().ok().and_then(|g| g.clone())?;
    let primary = primary_address()?;
    (r.address == primary || effective_relays().contains(&r.address)).then_some(r)
}

/// Where FiveM, the nick and the status go: the chosen route, or the main address.
fn route_address() -> Option<String> {
    valid_route().map(|r| r.address).or_else(effective_address)
}

/// Remembers the chosen route for this run and (when something answered) in settings.json for the next start.
fn remember_route(app: &AppHandle, r: &route::Route) {
    if let Ok(mut slot) = current_route().write() {
        *slot = Some(r.clone());
    }
    if r.kind == "none" || last_route(app) == r.address {
        return;
    }
    let _ = update_settings(app, |s| s.last_route = r.address.clone());
}

fn last_route(app: &AppHandle) -> String {
    settings_path(app).map(|p| nick::load(&p).last_route).unwrap_or_default()
}

/// «Играть», step 0 (0.3.5): the main server answers within ~3 s: straight to it; else the first reserve route that
/// answers (route.rs). Err("no_address"): no address yet. A cfx.re/join code is not asked: Cfx.re finds the server.
#[tauri::command]
async fn choose_route(app: AppHandle) -> Result<route::Route, String> {
    // 0.5.5: «Играть» has started: the file-server speed test stops and waits (firstjoin.rs)
    if let Ok(mut g) = play_pressed().lock() {
        *g = Some(Instant::now());
    }
    let Some(address) = effective_address() else {
        return Err("no_address".into());
    };
    let target = parse_address(&address);
    let primary = display_address(&target);
    let chosen = match &target {
        Target::JoinCode(_) => route::Route::main(&primary),
        Target::Direct { .. } => route::pick(&primary, &effective_relays(), &last_route(&app), route::PROBE_TIMEOUT).await,
    };
    remember_route(&app, &chosen);
    Ok(chosen)
}

/// The address the launcher really uses, or `None` if it is not set anywhere yet.
fn effective_address() -> Option<String> {
    if let Some(a) = remote_address().read().ok().and_then(|g| g.clone()) {
        return Some(a);
    }
    let built_in = config().server_address.trim();
    valid_address(built_in).then(|| built_in.to_string())
}

/// What players see and type into F8: `1.2.3.4:30120` or `cfx.re/join/abc123`.
fn display_address(t: &Target) -> String {
    match t {
        Target::Direct { host, port } => format!("{host}:{port}"),
        Target::JoinCode(code) => format!("cfx.re/join/{code}"),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicConfig {
    #[serde(flatten)]
    cfg: Config,
    version: String,
}

#[tauri::command]
fn get_config(app: AppHandle) -> PublicConfig {
    PublicConfig {
        cfg: config().clone(),
        version: app.package_info().version.to_string(),
    }
}

// ---------------------------------------------------------------- portable build (0.5.0)

/// The marker file of the portable build (README 11 / 7и): next to `COLDZ Launcher.exe`.
const PORTABLE_MARK: &str = "portable.txt";

/// Portable build: settings, logs and the news cache live next to the exe, in `data\`. None: the installed launcher.
fn portable_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        portable_dir_of(&exe)
    })
    .clone()
}

fn portable_dir_of(exe: &std::path::Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    dir.join(PORTABLE_MARK).is_file().then(|| dir.join("data"))
}

/// Where settings.json and the logs live: `data\` of the portable build, or `%APPDATA%\com.coldz.launcher`.
fn config_dir(app: &AppHandle) -> Option<PathBuf> {
    portable_dir().or_else(|| app.path().app_config_dir().ok())
}

fn cache_dir(app: &AppHandle) -> Option<PathBuf> {
    portable_dir().map(|d| d.join("cache")).or_else(|| app.path().app_cache_dir().ok())
}

#[tauri::command]
fn is_portable() -> bool {
    portable_dir().is_some()
}

// ---------------------------------------------------------------- FiveM

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct FivemInfo {
    installed: bool,
    path: Option<String>,
    /// Which source found it (fivem.rs): manual, registry, process, folder, shortcut.
    found_by: Option<String>,
}

/// `<app config dir>\fivem-search.log`: where the launcher looked for FiveM (next to settings.json).
fn search_log_path(app: &AppHandle) -> Option<PathBuf> {
    config_dir(app).map(|d| d.join("fivem-search.log"))
}

/// What the log said last time: the window asks every few seconds while FiveM is missing, the log gets a new entry
/// only when the answer changes (or on a manual choice).
fn last_logged() -> &'static std::sync::Mutex<Option<String>> {
    static LAST: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();
    LAST.get_or_init(|| std::sync::Mutex::new(None))
}

/// Looks for FiveM (fivem.rs). `why` — a title for the log; `force` — write the log even if nothing changed.
fn find_fivem(app: &AppHandle, why: &str, force: bool) -> Option<fivem::Found> {
    let manual = settings_path(app)
        .map(|p| nick::load(&p).fivem_path)
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from);
    let (found, trace) = fivem::search(&fivem::System { manual });
    let key = found.as_ref().map_or_else(|| "none".to_string(), |f| f.path.display().to_string());
    let changed = last_logged()
        .lock()
        .map(|mut g| {
            let changed = g.as_deref() != Some(key.as_str());
            *g = Some(key.clone());
            changed
        })
        .unwrap_or(true);
    if force || changed {
        if let Some(log) = search_log_path(app) {
            let title = match &found {
                Some(f) => format!("{why}: FiveM найден ({}): {}", f.stage, f.path.display()),
                None => format!("{why}: FiveM не найден"),
            };
            fivem::write_log(&log, &title, &trace);
        }
    }
    found
}

fn info_of(found: Option<fivem::Found>) -> FivemInfo {
    FivemInfo {
        installed: found.is_some(),
        path: found.as_ref().map(|f| f.path.display().to_string()),
        found_by: found.map(|f| f.stage.to_string()),
    }
}

#[tauri::command]
fn detect_fivem(app: AppHandle) -> FivemInfo {
    info_of(find_fivem(&app, "проверка", false))
}

/// "Choose FiveM.exe manually": the Windows file dialog. Ok(None) — closed without a choice; Err("not_fivem") — the
/// file does not lead to FiveM.exe (fivem.rs resolve), nothing saved; Ok(Some) — saved to settings.json and found.
#[tauri::command]
async fn pick_fivem(app: AppHandle, title: String) -> Result<Option<FivemInfo>, String> {
    #[cfg(windows)]
    let owner: isize = app
        .get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map_or(0, |h| h.0 as isize);
    #[cfg(not(windows))]
    let owner: isize = 0;
    let picked = tauri::async_runtime::spawn_blocking(move || fivem::pick_file(&title, owner))
        .await
        .map_err(|_| "dialog_failed".to_string())?;
    let Some(file) = picked else {
        return Ok(None);
    };
    let Some(exe) = fivem::resolve(&file) else {
        if let Some(log) = search_log_path(&app) {
            fivem::write_log(&log, &format!("вручную выбран {}: это не FiveM.exe", file.display()), &[]);
        }
        return Err("not_fivem".into());
    };
    update_settings(&app, |s| s.fivem_path = exe.display().to_string())
        .1
        .map_err(|_| "save_failed".to_string())?;
    Ok(Some(info_of(find_fivem(&app, "указан вручную", true))))
}

/// When the launcher last started FiveM: a second "Play" while FiveM is still starting must not start it again.
fn last_launch() -> &'static std::sync::Mutex<Option<Instant>> {
    static LAST: OnceLock<std::sync::Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| std::sync::Mutex::new(None))
}

/// FiveM may take this long to show its window (update check, first start).
const STARTING_FOR: Duration = Duration::from_secs(150);

fn launched_recently() -> bool {
    last_launch().lock().ok().and_then(|g| *g).map_or(false, |t| t.elapsed() < STARTING_FOR)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlayResult {
    /// "launched" — FiveM started with the link; "passed" — FiveM was open, the link went into it;
    /// "starting" — FiveM is still starting from the previous "Play", nothing done.
    action: &'static str,
    /// 0.5.4: the launcher has just set FiveM's game build settings (CitizenFX.ini written or created): FiveM may
    /// offer to update the game data first (the window says to press «Continue» and wait).
    build: bool,
}

// ---------------------------------------------------------------- game build (0.5.4, gamebuild.rs)

/// What `/info.json` of the route asked for, kept between `game_build` and `play`.
struct BuildAsk {
    address: String,
    at: Instant,
    wanted: Result<gamebuild::Wanted, String>,
    /// 0.5.4 (storage.rs): the override file FiveM will need and does not have yet, and the FiveM.app it goes to.
    storage: Option<(storage::Entry, PathBuf)>,
}

/// What «Играть» learns from `game_build`.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct GameBuild {
    /// The server said what build it needs.
    ok: bool,
    /// 0.5.4: a game file to download before FiveM starts: all bytes and bytes already there (a part from last time).
    storage: Option<StorageNeed>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageNeed {
    total: u64,
    have: u64,
}

fn build_ask() -> &'static std::sync::Mutex<Option<BuildAsk>> {
    static ASK: OnceLock<std::sync::Mutex<Option<BuildAsk>>> = OnceLock::new();
    ASK.get_or_init(|| std::sync::Mutex::new(None))
}

/// An answer this old is not used by «Играть».
const BUILD_ASK_FOR: Duration = Duration::from_secs(120);

/// «Играть», with the nick (0.5.4): `vars` of `/info.json` of the chosen route (gamebuild.rs). Only reads; `play`
/// writes CitizenFX.ini right before FiveM starts, when FiveM is not running. Also says whether FiveM will need a game
/// file it does not have yet (storage.rs): the window then downloads it first (`storage_download`).
#[tauri::command]
async fn game_build(app: AppHandle) -> GameBuild {
    let Some(Target::Direct { host, port }) = route_address().map(|a| parse_address(&a)) else {
        return GameBuild::default();
    };
    let address = format!("{host}:{port}");
    let wanted = ask_game_build(&address, route::PROBE_TIMEOUT).await;
    let ok = wanted.is_ok();
    let storage = match &wanted {
        Ok(w) => {
            let w = w.clone();
            tauri::async_runtime::spawn_blocking(move || storage_needed(&app, &w)).await.ok().flatten()
        }
        Err(_) => None,
    };
    let need = storage.as_ref().and_then(|(e, dir)| {
        storage::missing(&storage::paths(dir, e), e).map(|have| StorageNeed { total: e.size, have })
    });
    if let Ok(mut slot) = build_ask().lock() {
        *slot = Some(BuildAsk { address, at: Instant::now(), wanted, storage });
    }
    GameBuild { ok, storage: need }
}

/// 0.5.4 (storage.rs): the override file FiveM will need for this server and does not have, when FiveM is installed,
/// not running, and its FiveM.exe knows the same file. Anything unusual goes to fivem-search.log.
fn storage_needed(app: &AppHandle, w: &gamebuild::Wanted) -> Option<(storage::Entry, PathBuf)> {
    let exe = fivem_exe_quiet(app)?;
    if running::state_of(&running::processes()).count > 0 {
        return None;
    }
    let app_dir = support::app_dir(&exe);
    if !app_dir.is_dir() {
        return None;
    }
    let ini = std::fs::read(gamebuild::ini_path(&exe)).unwrap_or_default();
    let e = storage::needed(w, &ini, &effective_storage())?;
    storage::missing(&storage::paths(&app_dir, &e), &e)?;
    let knows = std::fs::read(&exe).map(|b| storage::fivem_knows(&b, &e)).unwrap_or(false);
    if !knows {
        if let Some(log) = search_log_path(app) {
            let line = format!("{} ({} байт): в {} нет этого sha1 и адреса, не качаем", e.cache_name(), e.size, exe.display());
            fivem::write_log(&log, "«Играть»: файл игры для сервера (game-storage)", &[line]);
        }
        return None;
    }
    Some((e, app_dir))
}

/// The download running now (one at a time): its «Отменить».
fn storage_cancel_slot() -> &'static std::sync::Mutex<Option<std::sync::Arc<storage::Cancel>>> {
    static S: OnceLock<std::sync::Mutex<Option<std::sync::Arc<storage::Cancel>>>> = OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(None))
}

/// For big files: no overall time limit (a stalled answer is caught by storage.rs), the same User-Agent.
fn download_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .user_agent(concat!("COLDZ-Launcher/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("download client")
        })
        .clone()
}

/// «Играть», 0.5.4: downloads (or continues) the game file `game_build` found missing into FiveM's cache
/// (storage.rs). Progress: event `storage-progress` ({done, total, phase}). Err: "cancelled" | "network" | "remote" |
/// "bad" | "space" | "disk" | "busy" | "none" (nothing to download any more).
#[tauri::command]
async fn storage_download(app: AppHandle) -> Result<(), String> {
    let job = build_ask()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().filter(|a| a.at.elapsed() < BUILD_ASK_FOR).and_then(|a| a.storage.clone()));
    let Some((entry, app_dir)) = job else {
        return Err("none".into());
    };
    let cancel = std::sync::Arc::new(storage::Cancel::default());
    {
        let mut slot = storage_cancel_slot().lock().map_err(|_| "busy".to_string())?;
        if slot.is_some() {
            return Err("busy".into());
        }
        *slot = Some(cancel.clone());
    }
    let paths = storage::paths(&app_dir, &entry);
    let before = storage::missing(&paths, &entry).unwrap_or(entry.size);
    let free = support::drive_of(&app_dir).map(|d| d.free);
    let started = Instant::now();
    let handle = app.clone();
    let result = storage::run(download_client(), app_dir, entry.clone(), free, cancel, move |p| {
        let _ = handle.emit("storage-progress", p);
    })
    .await;
    if let Ok(mut slot) = storage_cancel_slot().lock() {
        *slot = None;
    }
    // the download may take minutes: «Играть» right after it still uses this answer of the server
    if result.is_ok() {
        if let Ok(mut g) = build_ask().lock() {
            if let Some(a) = g.as_mut() {
                a.at = Instant::now();
                a.storage = None;
            }
        }
    }
    if let Some(log) = search_log_path(&app) {
        let now = storage::missing(&paths, &entry).unwrap_or(entry.size);
        let secs = started.elapsed().as_secs_f64().max(0.001);
        let got = now.saturating_sub(before);
        let mut lines = vec![
            format!("{} ← {}", paths.done.display(), entry.url),
            format!(
                "было {} из {} байт, стало {now}; {:.0} с, {:.1} МБ/с",
                before,
                entry.size,
                secs,
                got as f64 / secs / 1_000_000.0
            ),
        ];
        lines.push(match &result {
            Ok(()) => "готово: SHA-1 совпал, файл на месте".to_string(),
            Err(f) => format!("остановлено: {}", f.describe()),
        });
        fivem::write_log(&log, "«Играть»: файл игры для сервера (game-storage)", &lines);
    }
    result.map_err(|f| f.key().to_string())
}

/// «Отменить» under the download: the part stays, «Играть» continues it.
#[tauri::command]
fn storage_cancel() {
    if let Some(c) = storage_cancel_slot().lock().ok().and_then(|g| g.clone()) {
        c.cancel();
    }
}

/// `vars` of `http://<address>/info.json` → what FiveM needs (gamebuild.rs). Err: why nothing is set (for the log).
async fn ask_game_build(address: &str, wait: Duration) -> Result<gamebuild::Wanted, String> {
    let resp = http()
        .get(format!("http://{address}/info.json"))
        .timeout(wait)
        .send()
        .await
        .map_err(|_| "нет ответа /info.json".to_string())?;
    if !resp.status().is_success() {
        return Err(format!("/info.json: HTTP {}", resp.status().as_u16()));
    }
    let info: serde_json::Value = resp.json().await.map_err(|_| "/info.json не JSON".to_string())?;
    gamebuild::wanted(&info).map_err(str::to_string)
}

/// Right before FiveM starts (FiveM not running): CitizenFX.ini as the server asks. True: the file was written.
/// Anything else (no answer, an error) only goes to fivem-search.log: FiveM starts as before.
fn sync_game_build(app: &AppHandle, exe: &std::path::Path, address: &str) -> bool {
    let ask = build_ask().lock().ok().and_then(|mut g| g.take());
    let ini = gamebuild::ini_path(exe);
    let (head, outcome) = match ask {
        Some(a) if a.address == address && a.at.elapsed() < BUILD_ASK_FOR => match a.wanted {
            Ok(w) => (format!("сервер {address}: {}", w.describe()), Some(gamebuild::sync(&support::app_dir(exe), &w))),
            Err(why) => (format!("сервер {address}: {why}; файл не тронут, FiveM запускается как раньше"), None),
        },
        _ => (format!("сервер {address}: нет свежего ответа /info.json; файл не тронут"), None),
    };
    let written = matches!(outcome, Some(gamebuild::Outcome::Written { .. }));
    if let Some(log) = search_log_path(app) {
        let mut lines = vec![head, format!("файл: {}", ini.display())];
        if let Some(o) = &outcome {
            lines.extend(gamebuild::log_lines(o));
        }
        fivem::write_log(&log, "«Играть»: сборка игры для FiveM (CitizenFX.ini)", &lines);
    }
    written
}

/// Starts the real FiveM client and tells it to join our server (running.rs: not a second time).
/// Errors for the UI: no_address, fivem_missing, fivem_stuck (FiveM processes without a game window), launch_failed.
#[tauri::command]
fn play(app: AppHandle) -> Result<PlayResult, String> {
    let result = start_fivem(app.clone());
    // 0.5.5: FiveM started with the link (or got it): the launcher's mark of the first join (firstjoin.rs)
    if matches!(&result, Ok(r) if r.action != "starting") {
        mark_joined(&app);
    }
    result
}

/// settings.json `joinedOnce` (0.5.5): written once, after «Играть» started FiveM.
fn mark_joined(app: &AppHandle) {
    let already = settings_path(app).map(|p| nick::load(&p).joined_once == Some(true)).unwrap_or(false);
    if !already {
        let _ = update_settings(app, |s| s.joined_once = Some(true));
    }
}

fn start_fivem(app: AppHandle) -> Result<PlayResult, String> {
    // 0.3.5: the route chosen right before (choose_route): the main server or a reserve route
    let Some(address) = route_address() else {
        return Err("no_address".into());
    };
    let url = connect_url(&parse_address(&address));
    let Some(exe) = find_fivem(&app, "«Играть»", false).map(|f| f.path) else {
        return Err("fivem_missing".into());
    };
    let decision = running::decide(&running::state_of(&running::processes()), launched_recently());
    // 0.5.4: FiveM runs (open, starting, stuck): CitizenFX.ini is FiveM's now (it writes the file itself), not touched
    if decision != running::Decision::Launch {
        if let Some(log) = search_log_path(&app) {
            fivem::write_log(&log, "«Играть»: FiveM запущен, CitizenFX.ini не трогаем", &[]);
        }
        if let Ok(mut g) = build_ask().lock() {
            *g = None;
        }
    }
    let mut build = false;
    let action = match decision {
        running::Decision::Starting => return Ok(PlayResult { action: "starting", build }),
        running::Decision::Stuck => return Err("fivem_stuck".into()),
        running::Decision::PassLink => "passed",
        running::Decision::Launch => {
            if let Ok(mut g) = last_launch().lock() {
                *g = Some(Instant::now());
            }
            // 0.5.4: the game build the server asks for, before FiveM reads the file (gamebuild.rs); switched off by
            // `gameBuild.preset: false` (news.json, else launcher.config.json): as 0.5.3
            let remote = remote_preset().read().ok().and_then(|g| *g);
            if !preset_on(remote, &config().game_build) {
                if let Some(log) = search_log_path(&app) {
                    fivem::write_log(&log, "«Играть»: запись CitizenFX.ini выключена (gameBuild.preset = false)", &[]);
                }
                if let Ok(mut g) = build_ask().lock() {
                    *g = None;
                }
            } else if let Target::Direct { host, port } = parse_address(&address) {
                build = sync_game_build(&app, &exe, &format!("{host}:{port}"));
            }
            "launched"
        }
    };
    // FiveM refuses to start when its parent process is not the Windows shell or a browser
    // ("This application should be launched directly from the shell or a web browser").
    // So hand the fivem:// link to explorer.exe: the shell then starts FiveM itself (or, if FiveM is open,
    // FiveM passes the link to the running game).
    // 16.11.2026: explorer opens the link with the registered fivem:// handler. Missing or pointing to a FiveM.exe
    // that is gone, Windows shows «explorer.exe — Приложение не найдено» and FiveM does not start: point the handler
    // at the FiveM.exe found above first (fivem.rs ensure_link_handler); could not — skip explorer, start FiveM.exe.
    #[cfg(windows)]
    {
        let (handler_ok, lines) = fivem::ensure_link_handler(&exe);
        if !lines.is_empty() {
            if let Some(log) = search_log_path(&app) {
                fivem::write_log(&log, &format!("«Играть»: обработчик ссылки fivem:// для {}", exe.display()), &lines);
            }
        }
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        let explorer = std::path::Path::new(&windir).join("explorer.exe");
        if handler_ok && std::process::Command::new(&explorer).arg(&url).spawn().is_ok() {
            return Ok(PlayResult { action, build });
        }
    }
    // Fallback: `FiveM.exe "<link>"` directly (works on FiveM builds without the parent check).
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(&url);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    match cmd.spawn() {
        Ok(_) => Ok(PlayResult { action, build }),
        // Fall back to the registered fivem:// link handler.
        Err(_) => app
            .opener()
            .open_url(&url, None::<&str>)
            .map(|_| PlayResult { action, build })
            .map_err(|_| "launch_failed".into()),
    }
}

/// "Close FiveM" after fivem_stuck: ends every FiveM process. (closed, failed) — failed: FiveM runs as administrator.
#[tauri::command]
fn close_fivem() -> (u32, u32) {
    let result = running::close_matching(running::is_fivem);
    if let Ok(mut g) = last_launch().lock() {
        *g = None;
    }
    result
}

// ---------------------------------------------------------------- server status

#[derive(Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
struct Status {
    online: bool,
    players: u32,
    max_players: u32,
    ping_ms: Option<u32>,
    /// Address in use (for the "copy" line and F8 `connect`). Empty = not set yet.
    address: String,
    /// 0.3.5: the chosen route (route.rs): "main" | "reserve" | "none"; None: not chosen yet.
    route: Option<&'static str>,
}

fn http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(HTTP_TIMEOUT)
                .user_agent(concat!("COLDZ-Launcher/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client")
        })
        .clone()
}

async fn get_json(url: &str) -> Option<serde_json::Value> {
    let resp = http().get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

fn as_u32(v: &serde_json::Value) -> Option<u32> {
    v.as_u64()
        .map(|n| n as u32)
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// "Ping" = time to open a TCP connection to the game port (best of 3).
/// Close to what FiveM shows and works without admin rights (no ICMP).
async fn tcp_ping(host: &str, port: u16) -> Option<u32> {
    let mut best: Option<u32> = None;
    for _ in 0..3 {
        let start = Instant::now();
        let ok = tokio::time::timeout(PING_TIMEOUT, tokio::net::TcpStream::connect((host, port)))
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);
        if ok {
            let ms = start.elapsed().as_millis().max(1) as u32;
            best = Some(best.map_or(ms, |b| b.min(ms)));
        }
    }
    best
}

async fn status_direct(host: &str, port: u16) -> Status {
    let base = format!("http://{host}:{port}");
    let max_default = config().max_players;
    let (dynamic_url, players_url, info_url) = (
        format!("{base}/dynamic.json"),
        format!("{base}/players.json"),
        format!("{base}/info.json"),
    );
    let (dynamic, ping) = tokio::join!(get_json(&dynamic_url), tcp_ping(host, port));

    if let Some(d) = dynamic {
        return Status {
            online: true,
            players: d.get("clients").and_then(as_u32).unwrap_or(0),
            max_players: d.get("sv_maxclients").and_then(as_u32).unwrap_or(max_default),
            ping_ms: ping,
            ..Default::default()
        };
    }
    // Older servers: count /players.json and read the limit from /info.json.
    let (players, info) = tokio::join!(get_json(&players_url), get_json(&info_url));
    match players.as_ref().and_then(|p| p.as_array()) {
        Some(list) => Status {
            online: true,
            players: list.len() as u32,
            max_players: info
                .as_ref()
                .and_then(|i| i.pointer("/vars/sv_maxClients"))
                .and_then(as_u32)
                .unwrap_or(max_default),
            ping_ms: ping,
            ..Default::default()
        },
        None => Status { max_players: max_default, ..Default::default() },
    }
}

async fn status_join_code(code: &str) -> Status {
    let max_default = config().max_players;
    let url = format!("https://servers-frontend.fivem.net/api/servers/single/{code}");
    let Some(v) = get_json(&url).await else {
        return Status { max_players: max_default, ..Default::default() };
    };
    let data = v.get("Data").cloned().unwrap_or_default();
    let ping = match data
        .pointer("/connectEndPoints/0")
        .and_then(|e| e.as_str())
        .map(parse_address)
    {
        Some(Target::Direct { host, port }) => tcp_ping(&host, port).await,
        _ => None,
    };
    Status {
        online: true,
        players: data.get("clients").and_then(as_u32).unwrap_or(0),
        max_players: data.get("sv_maxclients").and_then(as_u32).unwrap_or(max_default),
        ping_ms: ping,
        ..Default::default()
    }
}

// ---------------------------------------------------------------- connection check (15.11)

/// `<app config dir>\net-check.log`: what answered how at the connection checks (netcheck.rs), next to settings.json.
fn net_log_path(app: &AppHandle) -> Option<PathBuf> {
    config_dir(app).map(|d| d.join("net-check.log"))
}

/// «Проверить связь», the check before «Играть» and (0.3.4) once at launcher start (netcheck.rs): the Cfx.re services
/// FiveM needs, our server separately (`/info.json`, the first thing FiveM asks when it joins), GitHub (news) to tell
/// "Cfx.re is cut" from "no internet".
#[tauri::command]
async fn check_network(app: AppHandle) -> netcheck::Report {
    // 0.3.5: the main server and the reserve routes (route.rs), all at once; the route for «Играть» is chosen too
    let server = async {
        match effective_address().map(|a| parse_address(&a)) {
            Some(Target::Direct { host, port }) => {
                let primary = format!("{host}:{port}");
                let relays = effective_relays();
                let answers = route::probe_all(&primary, &relays, netcheck::SERVER_TIMEOUT).await;
                let chosen = route::choose(&primary, &relays, &answers, &last_route(&app));
                remember_route(&app, &chosen);
                netcheck::ServerCheck { main: Some(answers.main), reserve: answers.reserve(), route: Some(chosen.kind) }
            }
            // no address yet, or a cfx.re/join code: its status itself comes from Cfx.re
            _ => netcheck::ServerCheck::default(),
        }
    };
    // 0.5.0: «is there internet at all» = GitHub or coldz.vip (news addresses) answers
    let control = async {
        let urls = news_urls();
        let futs: Vec<_> = urls.iter().map(|u| netcheck::answers(u)).collect();
        join_all(futs).await.into_iter().any(|a| a)
    };
    let (mut report, signin, pings) = tokio::join!(netcheck::run(server, control), signin_check(), route_pings());
    report.signin = signin;
    report.pings = pings;
    if let Some(log) = net_log_path(&app) {
        let (title, lines) = netcheck::log_lines(&report);
        fivem::write_log(&log, &title, &lines);
    }
    report
}

/// 0.5.0: the ping to the main server and every reserve route at once (for the connection card and the log).
async fn route_pings() -> Vec<netcheck::RoutePing> {
    let Some(Target::Direct { host, port }) = effective_address().map(|a| parse_address(&a)) else {
        return Vec::new();
    };
    let mut targets: Vec<(&'static str, usize, String, String, u16)> = vec![("main", 0, format!("{host}:{port}"), host, port)];
    for (i, r) in effective_relays().iter().enumerate() {
        if let Target::Direct { host, port } = parse_address(r) {
            targets.push(("reserve", i + 1, r.clone(), host, port));
        }
    }
    let futs: Vec<_> = targets.iter().map(|t| tcp_ping(&t.3, t.4)).collect();
    let ms = join_all(futs).await;
    targets
        .into_iter()
        .zip(ms)
        .map(|((kind, index, address, _, _), ms)| netcheck::RoutePing { kind, index, address, ms })
        .collect()
}

// ---------------------------------------------------------------- restart notice (0.5.0; online graph removed in 0.5.3)

#[derive(Serialize, Default, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ServerInfo {
    /// Seconds until the announced restart (server clock, so a wrong PC clock does not matter). None: none announced.
    restart_in: Option<i64>,
}

/// `/z_menu/online` (winter-server z_menu server/online.lua): { now, restartAt, ... }; 0.5.3 reads only the restart.
fn parse_online(v: &serde_json::Value) -> Option<ServerInfo> {
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return None;
    }
    let now = v.get("now").and_then(|n| n.as_i64())?;
    let restart = v.get("restartAt").and_then(|r| r.as_i64()).unwrap_or(0);
    Some(ServerInfo {
        restart_in: (restart > now).then_some(restart - now),
    })
}

/// An older server without `/z_menu/online`: `vars.coldz_restart_at` of `/info.json` and the PC clock.
fn parse_info_restart(v: &serde_json::Value, local_now: i64) -> Option<i64> {
    let at = v.pointer("/vars/coldz_restart_at").and_then(|a| a.as_str().and_then(|s| s.trim().parse::<i64>().ok()).or(a.as_i64()))?;
    (at > local_now).then_some(at - local_now)
}

/// The restart notice, through the chosen route (the window asks once a minute).
#[tauri::command]
async fn server_info() -> ServerInfo {
    let Some(Target::Direct { host, port }) = route_address().map(|a| parse_address(&a)) else {
        return ServerInfo::default();
    };
    let base = format!("http://{host}:{port}");
    if let Some(info) = get_json(&format!("{base}/z_menu/online")).await.as_ref().and_then(parse_online) {
        return info;
    }
    let restart_in = get_json(&format!("{base}/info.json"))
        .await
        .and_then(|v| parse_info_restart(&v, netcheck::unix_now() as i64));
    ServerInfo { restart_in }
}

// ---------------------------------------------------------------- sign-in route (0.4.0, signin.rs)

/// What the launcher keeps about the route while it runs.
#[derive(Default)]
struct SigninRun {
    /// The forwarding task (tunnel.rs `serve`) and what it forwards.
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    forward: Option<std::sync::Arc<tunnel::Forward>>,
    ip: Option<std::net::IpAddr>,
    /// The hosts file carries the launcher's block.
    active: bool,
    /// The tunnel that answered at the last check.
    tunnel_ok: Option<signin::Tunnel>,
    /// Groups whose straight check failed at the last check (signin.rs `hosts_to_route`).
    failed: Vec<String>,
}

fn signin_run() -> &'static std::sync::Mutex<SigninRun> {
    static RUN: OnceLock<std::sync::Mutex<SigninRun>> = OnceLock::new();
    RUN.get_or_init(|| std::sync::Mutex::new(SigninRun::default()))
}

fn signin_lock() -> std::sync::MutexGuard<'static, SigninRun> {
    signin_run().lock().unwrap_or_else(|e| e.into_inner())
}

/// The elevated copy (`--coldz-hosts <file>`, main.rs): writes the block and exits.
pub fn hosts_main(file: &str) -> i32 {
    hosts::apply(file)
}

/// Runs the futures at once, answers in order (no extra crate for one join).
async fn join_all<F: std::future::Future>(futs: Vec<F>) -> Vec<F::Output> {
    let mut pending: Vec<Option<std::pin::Pin<Box<F>>>> = futs.into_iter().map(|f| Some(Box::pin(f))).collect();
    let mut results: Vec<Option<F::Output>> = (0..pending.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut all_done = true;
        for (i, slot) in pending.iter_mut().enumerate() {
            if let Some(f) = slot {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => {
                        results[i] = Some(v);
                        *slot = None;
                    }
                    std::task::Poll::Pending => all_done = false,
                }
            }
        }
        if all_done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    results.into_iter().flatten().collect()
}

fn check_host(setup: &signin::Setup) -> Option<String> {
    reqwest::Url::parse(setup.groups.first()?.check.first()?).ok()?.host_str().map(str::to_string)
}

/// For the window and the connection check: the sign-in hosts straight (through the route when it is on) and the
/// first tunnel that answers. None: no groups set up.
async fn signin_check() -> Option<netcheck::SigninReport> {
    let setup = effective_signin();
    let probe_host = check_host(&setup)?;
    let groups = async {
        let futs: Vec<_> = setup.groups.iter().map(|g| netcheck::probe_url(&g.check[0])).collect();
        setup
            .groups
            .iter()
            .zip(join_all(futs).await)
            .map(|(g, p)| netcheck::GroupProbe { id: g.id.clone(), host: p.host, ok: p.ok, problem: p.problem, status: p.status, ms: p.ms })
            .collect::<Vec<_>>()
    };
    let tunnels = async {
        if setup.tunnels.is_empty() {
            return (None, None, Vec::new());
        }
        // 0.5.0: every relay traced (tunnel.rs `probe_traced`): number, why it stopped, bytes, its own time
        let futs: Vec<_> = setup.tunnels.iter().map(|t| tunnel::probe_traced(t, &probe_host, tunnel::PROBE_TIMEOUT)).collect();
        let traces = join_all(futs).await;
        let all: Vec<netcheck::TunnelProbe> = traces
            .iter()
            .enumerate()
            .map(|(i, tr)| {
                let ok = matches!(tr.answer(), Ok(a) if a.ok());
                netcheck::TunnelProbe {
                    index: i + 1,
                    address: setup.tunnels[i].address.clone(),
                    ok,
                    status: tr.status,
                    bytes: tr.bytes,
                    // the host answered but the volume did not come: the tunnel is «frozen»
                    problem: if ok { None } else { Some(tr.stop.unwrap_or("slow")) },
                    ms: tr.ms,
                }
            })
            .collect();
        // the first tunnel that carries data wins; else the one that came furthest, for the window
        if let Some(i) = all.iter().position(|p| p.ok) {
            return (Some(setup.tunnels[i].clone()), Some(all[i].clone()), all);
        }
        let closest = all.iter().max_by_key(|p| (p.status.is_some(), p.bytes)).cloned();
        (None, closest, all)
    };
    let (groups, tunnel) = tokio::join!(groups, tunnels);
    let active = { signin_lock().active };
    let failed: Vec<String> = groups.iter().filter(|g| !g.ok).map(|g| g.id.clone()).collect();
    let (tunnel_ok, tunnel, all_tunnels) = tunnel;
    let route_stats = route_stats_line();
    {
        let mut run = signin_lock();
        run.tunnel_ok = tunnel_ok;
        if !active {
            run.failed = failed.clone();
        }
    }
    Some(netcheck::SigninReport { blocked: !failed.is_empty() && !active, groups, tunnel, tunnels: all_tunnels, route_stats, active })
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct SigninStatus {
    active: bool,
    /// Where the hosts point while the route is on.
    ip: Option<String>,
    hosts: usize,
    tunnel: Option<String>,
}

fn signin_status_now() -> SigninStatus {
    let run = signin_lock();
    SigninStatus {
        active: run.active,
        ip: run.ip.map(|ip| ip.to_string()),
        hosts: run.forward.as_ref().map(|f| f.hosts.len()).unwrap_or(0),
        tunnel: run.forward.as_ref().map(|f| f.tunnel.address.clone()),
    }
}

#[tauri::command]
fn signin_status() -> SigninStatus {
    signin_status_now()
}

/// Starts (or restarts) the local forwarder for `hosts` through `tunnel`. Returns the loopback address it listens on.
fn start_forwarder(tunnel: signin::Tunnel, hosts: Vec<String>) -> Result<std::net::IpAddr, String> {
    let mut run = signin_lock();
    if let Some(t) = run.task.take() {
        t.abort();
    }
    run.forward = None;
    let (std_listener, ip) = tunnel::bind_local().map_err(|_| "port_busy".to_string())?;
    let listener = tokio::net::TcpListener::from_std(std_listener).map_err(|_| "port_busy".to_string())?;
    let forward = std::sync::Arc::new(tunnel::Forward::new(tunnel, hosts));
    run.forward = Some(forward.clone());
    run.ip = Some(ip);
    run.task = Some(tauri::async_runtime::spawn(tunnel::serve(listener, forward)));
    Ok(ip)
}

/// 0.5.0: what the running route carried (tunnel.rs `Stats`), one line for net-check.log. None: the route is off.
fn route_stats_line() -> Option<String> {
    let run = signin_lock();
    run.forward.as_ref().map(|f| f.stats().line(&f.tunnel.address))
}

fn stop_forwarder() {
    let mut run = signin_lock();
    if let Some(t) = run.task.take() {
        t.abort();
    }
    run.forward = None;
    run.ip = None;
}

/// A folder the launcher may write the block file to (hosts.rs): next to settings.json.
fn signin_work_dir(app: &AppHandle) -> PathBuf {
    config_dir(app).unwrap_or_else(std::env::temp_dir)
}

/// The tunnel to use: the one that answered at the last check, or asked now.
async fn pick_tunnel(setup: &signin::Setup) -> Option<signin::Tunnel> {
    if let Some(t) = signin_lock().tunnel_ok.clone() {
        return Some(t);
    }
    let host = check_host(setup)?;
    let futs: Vec<_> = setup.tunnels.iter().map(|t| tunnel::probe_full(t, &host)).collect();
    let answers = join_all(futs).await;
    let picked = answers.iter().position(|a| matches!(a, Ok(x) if x.ok())).map(|i| setup.tunnels[i].clone());
    signin_lock().tunnel_ok = picked.clone();
    picked
}

/// «Включить резервный маршрут входа»: the forwarder, then the hosts block through the Windows prompt.
/// Err: no_setup (no hosts or tunnels in news.json), no_route (no tunnel answers), port_busy (443 is taken on both
/// loopback addresses), cancelled (the player said no to the prompt), failed.
#[tauri::command]
async fn signin_enable(app: AppHandle) -> Result<SigninStatus, String> {
    let setup = effective_signin();
    if setup.groups.is_empty() || setup.tunnels.is_empty() {
        return Err("no_setup".into());
    }
    let tunnel = pick_tunnel(&setup).await.ok_or_else(|| "no_route".to_string())?;
    let failed = signin_lock().failed.clone();
    let hosts = signin::hosts_to_route(&setup, &failed);
    let ip = start_forwarder(tunnel, hosts.clone())?;
    let block = signin::block(&ip.to_string(), &hosts);
    let work = signin_work_dir(&app);
    let written = tauri::async_runtime::spawn_blocking(move || hosts::request(Some(&block), &work))
        .await
        .map_err(|_| "failed".to_string())?;
    match written {
        Ok(()) => {
            signin_lock().active = true;
            let _ = update_settings(&app, |s| s.signin_route = "on".into());
            if let Some(log) = net_log_path(&app) {
                fivem::write_log(&log, &format!("резервный маршрут входа включён: {} хостов → {}", hosts.len(), ip), &hosts);
            }
            Ok(signin_status_now())
        }
        Err(hosts::Refused::Cancelled) => {
            stop_forwarder();
            Err("cancelled".into())
        }
        Err(hosts::Refused::Failed) => {
            stop_forwarder();
            Err("failed".into())
        }
    }
}

/// «Выключить резервный маршрут входа» and the launcher's exit: the block out of the hosts file (one prompt), then
/// the forwarder stops. The player says no: the block stays and so does the forwarder (Err("cancelled")).
#[tauri::command]
async fn signin_disable(app: AppHandle) -> Result<SigninStatus, String> {
    let work = signin_work_dir(&app);
    let removed = tauri::async_runtime::spawn_blocking(move || hosts::request(None, &work))
        .await
        .map_err(|_| "failed".to_string())?;
    match removed {
        Ok(()) => {
            let stats = route_stats_line();
            stop_forwarder();
            signin_lock().active = false;
            let _ = update_settings(&app, |s| s.signin_route = "off".into());
            if let Some(log) = net_log_path(&app) {
                fivem::write_log(&log, "резервный маршрут входа выключен", &stats.into_iter().collect::<Vec<_>>());
            }
            Ok(signin_status_now())
        }
        Err(hosts::Refused::Cancelled) => Err("cancelled".into()),
        Err(hosts::Refused::Failed) => Err("failed".into()),
    }
}

/// At the launcher's exit, on the main thread: the block must not outlive the forwarder.
fn signin_cleanup(app: &AppHandle) {
    let active = { signin_lock().active };
    if !active && hosts::installed().is_none() {
        return;
    }
    let work = signin_work_dir(app);
    let stats = route_stats_line();
    if hosts::request(None, &work).is_ok() {
        if let (Some(log), Some(line)) = (net_log_path(app), stats) {
            fivem::write_log(&log, "резервный маршрут входа выключен при выходе", &[line]);
        }
        stop_forwarder();
        signin_lock().active = false;
        let _ = update_settings(app, |s| s.signin_route = "off".into());
    }
}

/// At the launcher's start: a block left in the hosts file (a crash, or the player said no to the removal) with
/// the route "on" in settings.json is taken over without a prompt when the forwarder can listen on the same
/// address and a tunnel answers; otherwise the block is removed (one prompt), so the sign-in hosts never point at
/// a port nobody listens on.
async fn signin_resume(app: AppHandle) {
    let Some((ip, hosts)) = hosts::installed() else { return };
    let wanted_on = settings_path(&app).map(|p| nick::load(&p).signin_route == "on").unwrap_or(false);
    let setup = effective_signin();
    let same_ip = ip.parse::<std::net::IpAddr>().ok();
    if wanted_on && !hosts.is_empty() {
        if let Some(t) = pick_tunnel(&setup).await {
            // the forwarder takes the first free loopback address; it must be the one the block names
            if let Ok(bound) = start_forwarder(t, hosts.clone()) {
                if Some(bound) == same_ip {
                    signin_lock().active = true;
                    if let Some(log) = net_log_path(&app) {
                        fivem::write_log(
                            &log,
                            &format!("резервный маршрут входа продолжен после перезапуска: {} хостов → {}", hosts.len(), bound),
                            &[],
                        );
                    }
                    return;
                }
                stop_forwarder();
            }
        }
    }
    let work = signin_work_dir(&app);
    let removed = tauri::async_runtime::spawn_blocking(move || hosts::request(None, &work)).await;
    if matches!(removed, Ok(Ok(()))) {
        let _ = update_settings(&app, |s| s.signin_route = "off".into());
        if let Some(log) = net_log_path(&app) {
            fivem::write_log(&log, "резервный маршрут входа: старые строки hosts убраны при запуске", &[]);
        }
    }
}


#[tauri::command]
async fn server_status() -> Status {
    // 0.3.5: through the chosen route (a reserve route answers the same /dynamic.json), so the status and the ping are
    // what the player really gets
    let Some(address) = route_address() else {
        return Status { max_players: config().max_players, ..Default::default() };
    };
    let target = parse_address(&address);
    let mut status = match &target {
        Target::Direct { host, port } => status_direct(host, *port).await,
        Target::JoinCode(code) => status_join_code(code).await,
    };
    status.address = display_address(&target);
    status.route = valid_route().map(|r| r.kind);
    status
}

// ---------------------------------------------------------------- nickname

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    config_dir(app).map(|d| d.join("settings.json"))
}

/// Changes settings.json: one read-modify-write at a time (0.3.5: the connection check remembers the route while
/// «Играть» saves the nick). Returns the saved settings and how the write went.
fn update_settings(app: &AppHandle, change: impl FnOnce(&mut nick::Settings)) -> (nick::Settings, std::io::Result<()>) {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = settings_path(app);
    let mut saved = path.as_deref().map(nick::load).unwrap_or_default();
    change(&mut saved);
    let written = path.as_deref().map_or(Ok(()), |p| nick::save(p, &saved));
    (saved, written)
}

/// The nick saved last time ("" = FiveM name).
#[tauri::command]
fn get_nick(app: AppHandle) -> String {
    settings_path(&app).map(|p| nick::load(&p).nick).unwrap_or_default()
}

/// Where the server takes the nick: the game address, or for a `cfx.re/join/` code its first real endpoint.
async fn nick_endpoint(target: &Target) -> Option<String> {
    match target {
        Target::Direct { host, port } => Some(nick::endpoint(host, *port)),
        Target::JoinCode(code) => {
            let url = format!("https://servers-frontend.fivem.net/api/servers/single/{code}");
            let v = get_json(&url).await?;
            match parse_address(v.pointer("/Data/connectEndPoints/0")?.as_str()?) {
                Target::Direct { host, port } => Some(nick::endpoint(&host, port)),
                Target::JoinCode(_) => None,
            }
        }
    }
}

/// "Play", step 1: remember the nick and tell the server (nick.rs). Err("chars" | "long") — the nick breaks the
/// rules, nothing is sent. The UI starts FiveM unless the server rejected the nick.
#[tauri::command]
async fn send_nick(app: AppHandle, nick: String) -> Result<nick::NickResult, String> {
    let wanted = nick::check(&nick).map_err(str::to_string)?;
    let (saved, _) = update_settings(&app, |s| {
        // 0.5.5 (firstjoin.rs): the first «Играть» of 0.5.5 writes the mark: an id from an older version means the
        // player has pressed «Играть» before; FiveM started sets it to true (`mark_joined`)
        if s.joined_once.is_none() {
            s.joined_once = Some(firstjoin::joined_before(None, &s.launcher_id));
        }
        if s.launcher_id.is_empty() {
            s.launcher_id = nick::new_launcher_id();
        }
        s.nick = wanted.clone();
    });
    // 0.3.5: the same route FiveM will use (choose_route runs before the nick)
    let Some(address) = route_address() else {
        return Ok(nick::NickResult::unreachable(&wanted));
    };
    let Some(url) = nick_endpoint(&parse_address(&address)).await else {
        return Ok(nick::NickResult::unreachable(&wanted));
    };
    Ok(post_nick(&url, &wanted, &saved.launcher_id).await)
}

/// `POST <url> {"nick", "launcher"}` and what the server said.
async fn post_nick(url: &str, wanted: &str, launcher_id: &str) -> nick::NickResult {
    let body = serde_json::json!({ "nick": wanted, "launcher": launcher_id });
    match http().post(url).json(&body).send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            let v = resp.json::<serde_json::Value>().await.ok();
            nick::interpret(code, v.as_ref(), wanted)
        }
        Err(_) => nick::NickResult::unreachable(wanted),
    }
}

// ---------------------------------------------------------------- news

#[derive(Serialize)]
struct News {
    source: &'static str,
    data: serde_json::Value,
}

fn valid_news(v: &serde_json::Value) -> bool {
    v.get("items").map_or(false, |i| i.is_array())
}

/// 0.5.0: every address of news.json, in order, without repeats: `newsUrl`, then `newsUrls`.
fn news_urls() -> Vec<String> {
    let c = config();
    let mut out: Vec<String> = Vec::new();
    for u in std::iter::once(&c.news_url).chain(c.news_urls.iter()) {
        let u = u.trim();
        if u.starts_with("https://") && !out.iter().any(|x| x == u) {
            out.push(u.to_string());
        }
    }
    out
}

/// The signature of a news.json body (`<file>.sig`, what `tauri signer sign` writes: base64 of the minisign text)
/// checked with `pubkey` (base64 of the minisign public key, as in tauri.conf.json). An empty key: not checked.
fn news_signed(body: &[u8], sig: &str, pubkey: &str) -> bool {
    use base64::Engine;
    if pubkey.trim().is_empty() {
        return true;
    }
    let b64 = |s: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
    };
    let (Some(pk), Some(sig)) = (b64(pubkey), b64(sig)) else { return false };
    let (Ok(pk), Ok(sig)) = (minisign_verify::PublicKey::decode(&pk), minisign_verify::Signature::decode(&sig)) else {
        return false;
    };
    pk.verify(body, &sig, false).is_ok()
}

/// One copy of news.json: the body and its signature. Only a good signature and a valid file count.
#[derive(Debug, Clone)]
struct SignedNews {
    value: serde_json::Value,
    body: Vec<u8>,
    sig: String,
}

/// `signedAt` (scripts/sign-news.py writes it, the signature covers it): the newest copy wins when two answer.
fn signed_at(v: &serde_json::Value) -> String {
    v.get("signedAt").and_then(|s| s.as_str()).unwrap_or("").to_string()
}

fn check_news(body: Vec<u8>, sig: &str, pubkey: &str) -> Option<SignedNews> {
    if !news_signed(&body, sig, pubkey) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&body).ok().filter(valid_news)?;
    Some(SignedNews { value, body, sig: sig.trim().to_string() })
}

async fn get_bytes(url: &str) -> Option<Vec<u8>> {
    let resp = http().get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.bytes().await.ok().map(|b| b.to_vec())
}

async fn fetch_signed(url: String, pubkey: String) -> Option<SignedNews> {
    let sig_url = format!("{url}.sig");
    let (body, sig) = tokio::join!(get_bytes(&url), get_bytes(&sig_url));
    let sig = if pubkey.trim().is_empty() { String::new() } else { String::from_utf8(sig?).ok()? };
    check_news(body?, &sig, &pubkey)
}

/// After the first good copy, the others get this long to answer too (a newer `signedAt` wins).
const NEWS_GRACE: Duration = Duration::from_millis(1500);

/// All addresses at once; the first good copy, or a newer one that comes within `NEWS_GRACE` after it.
async fn first_signed(urls: Vec<String>, pubkey: String) -> Option<(SignedNews, String)> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Option<(SignedNews, String)>>(urls.len().max(1));
    for u in urls {
        let (tx, pk) = (tx.clone(), pubkey.clone());
        tokio::spawn(async move {
            let got = fetch_signed(u.clone(), pk).await.map(|n| (n, u));
            let _ = tx.send(got).await;
        });
    }
    drop(tx);
    let mut best: Option<(SignedNews, String)> = None;
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let next = match deadline {
            Some(d) => match tokio::time::timeout_at(d, rx.recv()).await {
                Ok(n) => n,
                Err(_) => break,
            },
            None => rx.recv().await,
        };
        let Some(answer) = next else { break };
        if let Some((n, u)) = answer {
            if best.as_ref().map_or(true, |(b, _)| signed_at(&n.value) > signed_at(&b.value)) {
                best = Some((n, u));
            }
            deadline.get_or_insert_with(|| tokio::time::Instant::now() + NEWS_GRACE);
        }
    }
    best
}

#[tauri::command]
async fn fetch_news(app: AppHandle) -> News {
    let cache = cache_dir(&app).map(|d| d.join("news.json"));
    let pubkey = config().news_pubkey.clone();
    if let Some((n, _url)) = first_signed(news_urls(), pubkey).await {
        if let Some(c) = &cache {
            let _ = std::fs::create_dir_all(c.parent().unwrap());
            let _ = std::fs::write(c, &n.body);
            let _ = std::fs::write(c.with_extension("json.sig"), &n.sig);
        }
        apply_feed(&n.value);
        return News { source: "remote", data: with_rules(n.value) };
    }
    let (source, data) = local_news(cache);
    apply_feed(&data);
    News { source, data: with_rules(data) }
}

/// A news file without "rules" (e.g. an older one on GitHub) gets the rules built into the launcher.
fn with_rules(mut v: serde_json::Value) -> serde_json::Value {
    let has_rules = v.get("rules").and_then(|r| r.as_array()).map_or(false, |r| !r.is_empty());
    if !has_rules {
        let bundled: serde_json::Value = serde_json::from_str(BUNDLED_NEWS).unwrap_or_default();
        if let (Some(rules), Some(obj)) = (bundled.get("rules"), v.as_object_mut()) {
            obj.insert("rules".into(), rules.clone());
        }
    }
    v
}

/// Last downloaded news (0.5.0: only with its good signature next to it), or the copy built into the launcher.
fn local_news(cache: Option<PathBuf>) -> (&'static str, serde_json::Value) {
    if let Some(n) = cache.and_then(|c| {
        let body = std::fs::read(&c).ok()?;
        let sig = std::fs::read_to_string(c.with_extension("json.sig")).unwrap_or_default();
        check_news(body, &sig, &config().news_pubkey)
    }) {
        return ("cache", n.value);
    }
    ("bundled", serde_json::from_str(BUNDLED_NEWS).unwrap_or_default())
}

// ---------------------------------------------------------------- first join and the file-server speed (0.5.5)

/// When «Играть» was pressed last (`choose_route`): the speed test stops, and does not start for a while after it.
fn play_pressed() -> &'static std::sync::Mutex<Option<Instant>> {
    static AT: OnceLock<std::sync::Mutex<Option<Instant>>> = OnceLock::new();
    AT.get_or_init(|| std::sync::Mutex::new(None))
}

fn pressed_at() -> Option<Instant> {
    play_pressed().lock().ok().and_then(|g| *g)
}

/// The last measurement (settings.json `cdnSpeed`), kept across starts.
fn saved_speed(app: &AppHandle) -> Option<firstjoin::Record> {
    settings_path(app).and_then(|p| nick::load(&p).cdn_speed)
}

/// Is this the player's first join to COLDZ (firstjoin.rs `is_first`): the launcher's mark and whether FiveM's
/// `FiveM.app\data\server-cache-priv` exists (only that: the launcher never reads or changes FiveM's cache).
fn first_join_now(app: &AppHandle) -> bool {
    let s = settings_path(app).map(|p| nick::load(&p)).unwrap_or_default();
    let joined = firstjoin::joined_before(s.joined_once, &s.launcher_id);
    let app_dir = fivem_exe_quiet(app).map(|exe| support::app_dir(&exe));
    firstjoin::is_first(joined, firstjoin::cache_of(app_dir.as_deref()))
}

/// Downloads the test file when it is time (firstjoin.rs `due`: once in 10 minutes), not in the 2 minutes after
/// «Играть» and not while the game file downloads (storage.rs); stops when «Играть» is pressed. Every download goes
/// to net-check.log, the result to settings.json. Returns the measurement in use (maybe the last one).
async fn cdn_measure(app: &AppHandle, url: &str) -> Option<firstjoin::Record> {
    static ONE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    // one at a time: a second caller (start and «Проверить связь») then sees the fresh result
    let _one = ONE.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    let saved = saved_speed(app);
    let downloading = storage_cancel_slot().lock().map(|g| g.is_some()).unwrap_or(false);
    let after_play = pressed_at().map_or(false, |t| t.elapsed() < firstjoin::PLAY_QUIET);
    if downloading || after_play || !firstjoin::due(saved.as_ref(), url, netcheck::unix_now()) {
        return saved;
    }
    let started = Instant::now();
    let at = netcheck::unix_now();
    let m = firstjoin::measure(&download_client(), url, firstjoin::MAX_BYTES, firstjoin::MAX_TIME, || {
        pressed_at().map_or(false, |t| t > started)
    })
    .await;
    if let Some(log) = net_log_path(app) {
        fivem::write_log(&log, "замер скорости до сервера файлов", &[m.log_line(url)]);
    }
    // «Играть» came first and there is too little to count: as if not measured, the next start asks again
    if m.problem == Some("aborted") {
        return saved;
    }
    let rec = m.record(url, at);
    let _ = update_settings(app, |s| s.cdn_speed = Some(rec.clone()));
    Some(rec)
}

/// 0.5.5 (firstjoin.rs): the first-join line under «Играть» and the file-server line of «Проверить связь».
/// `measure`: download the test file now if it is time (`cdn_measure`); false: only what is known.
#[tauri::command]
async fn join_info(app: AppHandle, measure: bool) -> firstjoin::JoinInfo {
    let setup = join_setup();
    let rec = if measure && !setup.url.is_empty() { cdn_measure(&app, &setup.url).await } else { saved_speed(&app) };
    let handle = app.clone();
    let first = tauri::async_runtime::spawn_blocking(move || first_join_now(&handle)).await.unwrap_or(false);
    firstjoin::info(first, &setup, rec.as_ref())
}

// ---------------------------------------------------------------- support report and crash hint (0.5.0, support.rs)

/// The FiveM.exe the launcher found (no log entry: this is asked in the background).
fn fivem_exe_quiet(app: &AppHandle) -> Option<PathBuf> {
    let manual = settings_path(app).map(|p| nick::load(&p).fivem_path).filter(|p| !p.trim().is_empty()).map(PathBuf::from);
    fivem::search(&fivem::System { manual }).0.map(|f| f.path)
}

/// One line under «Играть» after a fresh FiveM crash (support.rs `hint`). None: nothing to say.
#[tauri::command]
async fn crash_hint(app: AppHandle) -> Option<support::Hint> {
    tauri::async_runtime::spawn_blocking(move || {
        let exe = fivem_exe_quiet(&app);
        support::hint(&support::gather(exe.as_deref(), std::time::SystemTime::now()))
    })
    .await
    .ok()
    .flatten()
}

fn launcher_logs(app: &AppHandle) -> Vec<PathBuf> {
    // 0.5.6: and the crash reports' log (crashreport.rs)
    [net_log_path(app), search_log_path(app), crash_log_path(app)].into_iter().flatten().collect()
}

/// Our own addresses may stay in the report: the server, the reserve routes, the relays of the sign-in route.
fn our_ips() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |a: &str| {
        let host = a.rsplit_once(':').map_or(a, |(h, _)| h).to_string();
        if !out.contains(&host) {
            out.push(host);
        }
    };
    if let Some(a) = primary_address() {
        add(&a);
    }
    for r in effective_relays().iter().chain(config().relays.iter()) {
        add(r);
    }
    for t in effective_signin().tunnels.iter().chain(config().signin.tunnels.iter()) {
        add(&t.address);
    }
    out
}

/// «Собрать отчёт для поддержки», step 1: what will go into the zip (shown to the player before anything is made).
#[tauri::command]
async fn support_preview(app: AppHandle) -> support::Plan {
    tauri::async_runtime::spawn_blocking(move || {
        let exe = fivem_exe_quiet(&app);
        support::plan(&launcher_logs(&app), exe.as_deref(), std::time::SystemTime::now()).0
    })
    .await
    .unwrap_or_default()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportDone {
    path: String,
    name: String,
    size: u64,
}

/// Step 2: the zip on the desktop (`COLDZ-support-<date>-<time>.zip`), Explorer shows it. `with_dump` — the player
/// ticked the crash dump. Err: no_desktop, write_failed.
#[tauri::command]
async fn support_create(app: AppHandle, with_dump: bool) -> Result<SupportDone, String> {
    let version = app.package_info().version.to_string();
    let keep = our_ips();
    let route_on = signin_lock().active;
    tauri::async_runtime::spawn_blocking(move || {
        let now = std::time::SystemTime::now();
        let exe = fivem_exe_quiet(&app);
        let (plan, dumps) = support::plan(&launcher_logs(&app), exe.as_deref(), now);
        let facts = support::gather(exe.as_deref(), now);
        let hint = support::hint(&facts);
        let s = settings_path(&app).map(|p| nick::load(&p)).unwrap_or_default();
        let extra = vec![
            format!("Ник в лаунчере: {}", if s.nick.is_empty() { "(из FiveM)" } else { &s.nick }),
            format!("Маршрут к серверу: {}", if s.last_route.is_empty() { "-" } else { &s.last_route }),
            format!("Резервный маршрут входа: {}", if route_on { "включён" } else { "выключен" }),
            format!("FiveM.exe указан вручную: {}", if s.fivem_path.is_empty() { "нет" } else { "да" }),
            // 0.5.5 (firstjoin.rs)
            format!("Первый вход на COLDZ (по лаунчеру): {}", if first_join_now(&app) { "да" } else { "нет" }),
            format!(
                "Сервер файлов: {}",
                match &s.cdn_speed {
                    Some(r) => match r.mbps {
                        Some(v) => format!("{v:.1} МБ/с ({})", fivem::utc_stamp(r.at)),
                        None => format!("не измерено: {} ({})", r.problem, fivem::utc_stamp(r.at)),
                    },
                    None => "не измерялся".to_string(),
                }
            ),
        ];
        let system = support::system_text(&version, &facts, hint.as_ref(), &dumps, &extra);
        let bytes = support::build(&plan, &system, with_dump, &support::Scrub::of_this_pc(keep));
        let desk = support::desktop_dir().ok_or_else(|| "no_desktop".to_string())?;
        let stamp = fivem::utc_stamp(netcheck::unix_now());
        // «2026-11-18 22:30:05 UTC» -> «20261118-223005»
        let compact: String = stamp.chars().filter(|c| c.is_ascii_digit()).take(14).collect();
        let name = format!("COLDZ-support-{}-{}.zip", &compact[..8.min(compact.len())], &compact[8.min(compact.len())..]);
        let path = desk.join(&name);
        std::fs::write(&path, &bytes).map_err(|_| "write_failed".to_string())?;
        #[cfg(windows)]
        {
            let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
            let _ = std::process::Command::new(std::path::Path::new(&windir).join("explorer.exe"))
                .arg(format!("/select,{}", path.display()))
                .spawn();
        }
        Ok(SupportDone { path: path.display().to_string(), name, size: bytes.len() as u64 })
    })
    .await
    .map_err(|_| "write_failed".to_string())?
}

// ---------------------------------------------------------------- crash reports (0.5.6, crashreport.rs)

/// 0.5.6: `crashReport` from news.json. Default: no key, launcher.config.json decides.
fn remote_crash() -> &'static RwLock<crashreport::Feed> {
    static S: OnceLock<RwLock<crashreport::Feed>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(crashreport::Feed::default()))
}

/// The receiver, its key and the switches in use: news.json wins over launcher.config.json.
fn crash_setup() -> crashreport::Setup {
    let feed = remote_crash().read().map(|g| g.clone()).unwrap_or_default();
    crashreport::setup(&feed, &config().crash_report)
}

/// `<app config dir>\crash-report.log`: what was found, asked, answered, sent (and why not).
fn crash_log_path(app: &AppHandle) -> Option<PathBuf> {
    config_dir(app).map(|d| d.join("crash-report.log"))
}

/// `<app config dir>\crash-outbox`: reports that did not go out yet.
fn crash_outbox(app: &AppHandle) -> Option<PathBuf> {
    config_dir(app).map(|d| d.join("crash-outbox"))
}

fn crash_log(app: &AppHandle, title: &str, lines: &[String]) {
    if let Some(log) = crash_log_path(app) {
        fivem::write_log(&log, title, lines);
    }
}

fn crash_state(app: &AppHandle) -> crashreport::State {
    settings_path(app).map(|p| nick::load(&p).crash_report).unwrap_or_default()
}

/// The salt that hashes the Rockstar ID: made once, kept in settings.json, never sent.
fn crash_salt(app: &AppHandle) -> String {
    let (s, _) = update_settings(app, |s| {
        if s.crash_report.salt.is_empty() {
            s.crash_report.salt = nick::new_launcher_id();
        }
    });
    s.crash_report.salt
}

/// FiveM's crashes on this PC (crashreport.rs `scan`) and FiveM's log folder.
fn crash_scan(app: &AppHandle) -> (Vec<crashreport::Crash>, Option<PathBuf>) {
    let app_dir = fivem_exe_quiet(app).map(|exe| support::app_dir(&exe));
    let crashes = app_dir.as_ref().map(|d| d.join("crashes"));
    let packs: Vec<PathBuf> = [support::desktop_dir(), crashreport::downloads_dir()].into_iter().flatten().collect();
    (crashreport::scan(crashes.as_deref(), &packs, netcheck::unix_now()), app_dir.map(|d| d.join("logs")))
}

fn crash_ctx(app: &AppHandle, lang: &str, logs: Option<PathBuf>, new_count: usize) -> crashreport::Ctx {
    crashreport::Ctx {
        launcher: app.package_info().version.to_string(),
        lang: if ["ru", "en", "es", "pt-BR"].contains(&lang) { lang.to_string() } else { String::new() },
        clean: crashreport::Clean { scrub: support::Scrub::of_this_pc(our_ips()), salt: crash_salt(app), rockstar: Vec::new() },
        windows: support::windows_version(),
        gpus: crashreport::gpus(),
        ram_gb: crashreport::ram_gb(),
        logs,
        new_count,
    }
}

/// The crash the window asked about last (the log gets one line per crash, not one per look).
fn crash_asked() -> &'static std::sync::Mutex<String> {
    static S: OnceLock<std::sync::Mutex<String>> = OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(String::new()))
}

/// 0.5.6: a FiveM crash the player has not answered about (the line under «Играть»). None: nothing new, «Не спрашивать»,
/// or switched off. Also sends, once per run in the background, reports that did not go out last time.
#[tauri::command]
async fn crash_pending(app: AppHandle) -> Option<crashreport::Ask> {
    let setup = crash_setup();
    if !setup.enabled {
        return None;
    }
    crash_resend(&app, &setup);
    tauri::async_runtime::spawn_blocking(move || {
        let state = crash_state(&app);
        let (found, logs) = crash_scan(&app);
        let (c, count) = crashreport::pending(&found, &state)?;
        let (hash, key) = crashreport::quick_hash(&c, logs.as_deref());
        let new = crash_asked()
            .lock()
            .map(|mut g| {
                let new = *g != c.id;
                *g = c.id.clone();
                new
            })
            .unwrap_or(false);
        if new {
            let place = if hash.is_empty() { "не известно".to_string() } else { hash.clone() };
            crash_log(&app, "вылет FiveM: спрашиваем, отправить ли отчёт", &[format!("{} ({}), место {place}, новых вылетов {count}", c.id, fivem::utc_stamp(c.at))]);
        }
        Some(crashreport::Ask { id: c.id, at: c.at, hash, key, count })
    })
    .await
    .ok()
    .flatten()
}

/// 0.5.6: what the report about this crash holds (the card, before anything is sent).
#[tauri::command]
async fn crash_preview(app: AppHandle, id: String) -> Option<crashreport::Preview> {
    let setup = crash_setup();
    if !setup.enabled {
        return None;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let (found, logs) = crash_scan(&app);
        let c = found.into_iter().find(|c| c.id == id)?;
        let p = crashreport::prepare(&c, &crash_ctx(&app, "", logs, 0));
        Some(crashreport::preview(&p, setup.dump))
    })
    .await
    .ok()
    .flatten()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CrashSent {
    /// "sent" | "queued" (again at the next start) | "refused" | "failed"
    status: &'static str,
    /// The dump went with it.
    dump: bool,
    /// Why the ticked dump stayed: "" | "too_big" | "unreadable" | "off"
    dump_skipped: &'static str,
}

/// After a try: sent or refused — the file goes; not now — one more try counted, the file waits for the next start.
fn crash_after_send(app: &AppHandle, file: &str, sent: &crashreport::Sent, in_outbox: bool) -> &'static str {
    let gone = !matches!(sent, crashreport::Sent::Retry(_));
    let _ = update_settings(app, |s| {
        let ob = &mut s.crash_report.outbox;
        if gone {
            ob.retain(|p| p.file != file);
        } else if let Some(p) = ob.iter_mut().find(|p| p.file == file) {
            p.tries = p.tries.saturating_add(1);
        }
    });
    if gone && crashreport::safe_file_name(file) {
        if let Some(d) = crash_outbox(app) {
            let _ = std::fs::remove_file(d.join(file));
        }
    }
    match sent {
        crashreport::Sent::Ok(_) => "sent",
        crashreport::Sent::Retry(_) if in_outbox => "queued",
        crashreport::Sent::Retry(_) => "failed",
        crashreport::Sent::Refused(_) => "refused",
    }
}

fn crash_sent_line(sent: &crashreport::Sent, attempt: u32) -> String {
    match sent {
        crashreport::Sent::Ok(id) => format!("отправлен: приёмник принял{}", if id.is_empty() { String::new() } else { format!(", номер {id}") }),
        crashreport::Sent::Retry(why) => {
            format!("не отправлен (попытка {attempt} из {}): {why}; повтор при следующем запуске", crashreport::OUTBOX_TRIES)
        }
        crashreport::Sent::Refused(why) => format!("приёмник отказал: {why}; файл удалён, повтора не будет"),
    }
}

/// 0.5.6: «Отправить» in the card: the report is made (texts cleaned, the dump only with `with_dump`), kept in the outbox,
/// sent at once. Whatever happens: this crash is answered (not asked about again). Err: "busy" | "off" | "not_found" |
/// "too_big" | "build".
#[tauri::command]
async fn crash_send(app: AppHandle, id: String, with_dump: bool, lang: String) -> Result<CrashSent, String> {
    static ONE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let Ok(_one) = ONE.get_or_init(|| tokio::sync::Mutex::new(())).try_lock() else {
        return Err("busy".into());
    };
    let setup = crash_setup();
    if !setup.enabled {
        return Err("off".into());
    }
    let handle = app.clone();
    let dump_allowed = setup.dump;
    let built = tauri::async_runtime::spawn_blocking(move || {
        let state = crash_state(&handle);
        let (found, logs) = crash_scan(&handle);
        let new_count = found.iter().filter(|x| x.at > state.seen).count();
        let c = found.into_iter().find(|c| c.id == id).ok_or_else(|| "not_found".to_string())?;
        let p = crashreport::prepare(&c, &crash_ctx(&handle, &lang, logs, new_count));
        let packed = crashreport::pack(&p, with_dump, dump_allowed)?;
        Ok::<_, String>((c, p.hash.clone(), packed))
    })
    .await
    .map_err(|_| "build".to_string())?;
    let (crash, hash, packed) = match built {
        Ok(b) => b,
        Err(e) => {
            crash_log(&app, "отчёт о вылете не собран", &[e.clone()]);
            return Err(e);
        }
    };
    let now = netcheck::unix_now();
    let name = crashreport::outbox_name(&crash, now);
    let saved = crash_outbox(&app).map(|d| crashreport::outbox_put(&d, &name, &packed.bytes));
    let in_outbox = matches!(saved, Some(Ok(_)));
    let _ = update_settings(&app, |s| {
        let st = &mut s.crash_report;
        st.seen = st.seen.max(crash.at);
        if in_outbox {
            st.outbox.push(crashreport::Pending { file: name.clone(), at: now, tries: 0, hash: hash.clone(), dump: packed.dump_attached });
        }
    });
    let size = packed.bytes.len();
    let (dump_attached, dump_skipped) = (packed.dump_attached, packed.dump_skipped);
    let sent = crashreport::send(&setup.url, &setup.key, packed.bytes).await;
    let status = crash_after_send(&app, &name, &sent, in_outbox);
    let dump_line = if dump_attached {
        "приложен".to_string()
    } else if with_dump {
        format!("не приложен ({dump_skipped})")
    } else {
        "нет".to_string()
    };
    let mut lines = vec![
        format!("{} ({}), место {}", crash.id, fivem::utc_stamp(crash.at), if hash.is_empty() { "не известно" } else { &hash }),
        format!("{name}: {size} байт, снимок памяти: {dump_line}"),
        crash_sent_line(&sent, 1),
    ];
    if let Some(Err(e)) = &saved {
        lines.push(format!("в очередь не записан: {e}"));
    }
    crash_log(&app, "отчёт о вылете: игрок нажал «Отправить»", &lines);
    Ok(CrashSent { status, dump: dump_attached, dump_skipped })
}

/// 0.5.6: «Не отправлять» (this crash and older ones are not asked about again) or «Не спрашивать» (no more questions).
#[tauri::command]
fn crash_answer(app: AppHandle, at: u64, never: bool) {
    let at = at.min(netcheck::unix_now());
    let _ = update_settings(&app, |s| {
        let st = &mut s.crash_report;
        st.seen = st.seen.max(at);
        st.never |= never;
    });
    let title = if never { "отчёт о вылете: «Не спрашивать»" } else { "отчёт о вылете: «Не отправлять»" };
    crash_log(&app, title, &[format!("вылет {}", fivem::utc_stamp(at))]);
}

/// Reports that did not go out last time (crash-outbox): once per run, in the background, one after another; too old,
/// too many tries or too many files: deleted.
fn crash_resend(app: &AppHandle, setup: &crashreport::Setup) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ONCE: AtomicBool = AtomicBool::new(false);
    if ONCE.swap(true, Ordering::SeqCst) {
        return;
    }
    let (app, setup) = (app.clone(), setup.clone());
    tauri::async_runtime::spawn(async move {
        let Some(dir) = crash_outbox(&app) else { return };
        let now = netcheck::unix_now();
        let (send, drop) = crashreport::outbox_plan(&crash_state(&app), now);
        if !drop.is_empty() {
            let gone: Vec<String> = drop.iter().map(|p| p.file.clone()).collect();
            let _ = update_settings(&app, |s| s.crash_report.outbox.retain(|p| !gone.contains(&p.file)));
            for p in drop.iter().filter(|p| crashreport::safe_file_name(&p.file)) {
                let _ = std::fs::remove_file(dir.join(&p.file));
            }
            let lines: Vec<String> = drop.iter().map(|p| format!("{} (попыток {}, от {})", p.file, p.tries, fivem::utc_stamp(p.at))).collect();
            crash_log(&app, "очередь отчётов о вылетах: удалены (старше 7 дней, 3 попытки или больше 3 файлов)", &lines);
        }
        for p in send {
            let path = dir.join(&p.file);
            let bytes = tauri::async_runtime::spawn_blocking(move || std::fs::read(&path)).await.ok().and_then(|r| r.ok());
            let Some(bytes) = bytes.filter(|b| !b.is_empty() && b.len() <= crashreport::LIMIT_DUMP) else {
                let file = p.file.clone();
                let _ = update_settings(&app, |s| s.crash_report.outbox.retain(|x| x.file != file));
                crash_log(&app, "очередь отчётов о вылетах: файла нет", &[p.file.clone()]);
                continue;
            };
            let sent = crashreport::send(&setup.url, &setup.key, bytes).await;
            crash_after_send(&app, &p.file, &sent, true);
            crash_log(&app, "очередь отчётов о вылетах: повтор", &[p.file.clone(), crash_sent_line(&sent, p.tries.saturating_add(1))]);
        }
        // files settings.json no longer knows (a lost settings.json, a half-written .part): a week later they go
        let known: Vec<String> = crash_state(&app).outbox.into_iter().map(|p| p.file).collect();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let old = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .map_or(false, |a| a.as_secs() > crashreport::OUTBOX_SECS);
            if old && !known.contains(&name) && name.starts_with("crash-") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    });
}

// ---------------------------------------------------------------- links

/// Opens a web link or e-mail in the default program. Only https:// and mailto:.
#[tauri::command]
fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("mailto:")) {
        return Err("blocked".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- self-update

struct PendingUpdate(Mutex<Option<tauri_plugin_updater::Update>>);

/// 0.5.0: the new version, already downloaded in the background (its signature is checked again by `install`).
/// «Обновить» installs it at once; closing the launcher installs it quietly (no restart): `install_on_exit`.
struct ReadyUpdate(std::sync::Mutex<Option<(tauri_plugin_updater::Update, Vec<u8>)>>);

fn ready_lock(app: &AppHandle) -> std::sync::MutexGuard<'_, Option<(tauri_plugin_updater::Update, Vec<u8>)>> {
    app.state::<ReadyUpdate>().inner().0.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    version: String,
    notes: Option<String>,
}

/// 0.5.0: downloads the update in the background after the check; `update-ready` tells the window.
fn download_in_background(app: &AppHandle, update: tauri_plugin_updater::Update) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match update.download(|_, _| {}, || {}).await {
            Ok(bytes) => {
                let version = update.version.clone();
                *ready_lock(&app) = Some((update, bytes));
                let _ = app.emit("update-ready", version);
            }
            Err(e) => {
                if let Some(log) = net_log_path(&app) {
                    fivem::write_log(&log, &format!("обновление {}: не скачалось заранее ({e})", update.version), &[]);
                }
            }
        }
    });
}

/// At the launcher's exit (after the sign-in route is cleaned up): a downloaded update is installed without
/// questions (NSIS «passive»: only a progress bar) and the launcher is not started again. The installer exits this
/// process itself.
fn install_on_exit(app: &AppHandle) {
    let Some((update, bytes)) = ready_lock(app).take() else { return };
    if let Some(log) = net_log_path(app) {
        fivem::write_log(&log, &format!("обновление {} ставится при закрытии лаунчера", update.version), &[]);
    }
    let _ = update.restart_after_install(false).install(&bytes);
}

#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    let info = update.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        notes: u.body.clone(),
    });
    let already = ready_lock(&app).as_ref().map(|(u, _)| u.version.clone());
    // 0.5.0: the portable build is not installed anywhere: the window offers to download the new zip instead
    if let Some(u) = update.as_ref().filter(|u| portable_dir().is_none() && already.as_deref() != Some(u.version.as_str())) {
        download_in_background(&app, u.clone());
    }
    *app.state::<PendingUpdate>().0.lock().await = update;
    Ok(info)
}

/// 0.5.0: the window asks whether the update is already downloaded (it then says it installs itself on close).
#[tauri::command]
fn update_ready(app: AppHandle) -> Option<String> {
    ready_lock(&app).as_ref().map(|(u, _)| u.version.clone())
}

#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    if portable_dir().is_some() {
        return Err("portable".into());
    }
    // 0.5.0: already downloaded: install at once (the installer starts the launcher again)
    let ready = ready_lock(&app).take();
    if let Some((update, bytes)) = ready {
        let _ = app.emit("update-progress", 100u64);
        update.install(&bytes).map_err(|e| e.to_string())?;
        app.restart();
    }
    let Some(update) = app.state::<PendingUpdate>().0.lock().await.take() else {
        return Err("no_update".into());
    };
    let mut done: u64 = 0;
    let handle = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                done += chunk as u64;
                if let Some(t) = total.filter(|t| *t > 0) {
                    let _ = handle.emit("update-progress", (done * 100 / t).min(100));
                }
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    // On Windows the installer closes the app itself; restart covers other cases.
    app.restart();
}

// ---------------------------------------------------------------- window: size, place, tray (0.5.0, window.rs)

/// Work areas of the monitors in logical pixels, the main monitor first.
fn work_areas(w: &tauri::WebviewWindow) -> Vec<window::Area> {
    let area = |m: &tauri::Monitor| {
        let s = m.scale_factor().max(0.5);
        let r = m.work_area();
        window::Area { x: r.position.x as f64 / s, y: r.position.y as f64 / s, w: r.size.width as f64 / s, h: r.size.height as f64 / s }
    };
    let mut out: Vec<window::Area> = Vec::new();
    if let Ok(Some(m)) = w.primary_monitor() {
        out.push(area(&m));
    }
    for m in w.available_monitors().unwrap_or_default() {
        let a = area(&m);
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// At start: the saved place (or a size that fits the screen), then the window is shown (it starts hidden in
/// tauri.conf.json so it does not jump). The window is shown whatever happens here.
fn restore_window(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else { return };
    let saved = settings_path(app).map(|p| nick::load(&p).window).unwrap_or(None);
    let p = window::place(saved, &work_areas(&w));
    let _ = w.set_size(tauri::LogicalSize::new(p.size.0, p.size.1));
    match p.pos {
        Some((x, y)) => {
            let _ = w.set_position(tauri::LogicalPosition::new(x, y));
        }
        None => {
            let _ = w.center();
        }
    }
    if p.maximized {
        let _ = w.maximize();
    }
    let _ = w.show();
    let _ = w.set_focus();
}

/// When the window closes: its size and place for the next start (not while minimized; maximized keeps the size it
/// had before).
fn save_window(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else { return };
    if w.is_minimized().unwrap_or(false) || !w.is_visible().unwrap_or(true) {
        return;
    }
    let maximized = w.is_maximized().unwrap_or(false);
    let s = w.scale_factor().unwrap_or(1.0).max(0.5);
    let (pos, size) = (w.outer_position(), w.inner_size());
    let _ = update_settings(app, |st| {
        let old = st.window.unwrap_or_default();
        st.window = Some(match (maximized, pos, size) {
            (false, Ok(pos), Ok(size)) => window::Geometry {
                x: pos.x as f64 / s,
                y: pos.y as f64 / s,
                w: size.width as f64 / s,
                h: size.height as f64 / s,
                maximized: false,
            },
            _ => window::Geometry { maximized, ..old },
        });
    });
}

const TRAY_ID: &str = "coldz";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Prefs {
    tray: bool,
    /// 0.5.6: ask about sending a crash report (false after «Не спрашивать»).
    crash_ask: bool,
    /// 0.5.6: crash reports are switched on (launcher.config.json / news.json): the settings card shows the checkbox.
    crash_on: bool,
}

fn prefs_now(app: &AppHandle) -> Prefs {
    let s = settings_path(app).map(|p| nick::load(&p)).unwrap_or_default();
    Prefs { tray: s.tray, crash_ask: !s.crash_report.never, crash_on: crash_setup().enabled }
}

#[tauri::command]
fn get_prefs(app: AppHandle) -> Prefs {
    prefs_now(&app)
}

/// 0.5.6: «Предлагать отправить отчёт после вылета игры» in «Настройки». On again: only crashes after now are asked about.
#[tauri::command]
fn set_crash_ask(app: AppHandle, on: bool) -> Prefs {
    let now = netcheck::unix_now();
    let _ = update_settings(&app, |s| {
        s.crash_report.never = !on;
        if on {
            s.crash_report.seen = s.crash_report.seen.max(now);
        }
    });
    crash_log(&app, if on { "настройки: спрашивать об отчётах снова" } else { "настройки: не спрашивать об отчётах" }, &[]);
    prefs_now(&app)
}

/// The tray icon with its menu («Открыть», «Выйти» in the window's language); a left click opens the window.
fn ensure_tray(app: &AppHandle, open: &str, quit: &str) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let menu = Menu::with_items(
        app,
        &[&MenuItem::with_id(app, "open", open, true, None::<&str>)?, &MenuItem::with_id(app, "quit", quit, true, None::<&str>)?],
    )?;
    if let Some(t) = app.tray_by_id(TRAY_ID) {
        return t.set_menu(Some(menu));
    }
    let mut b = TrayIconBuilder::with_id(TRAY_ID).tooltip("COLDZ").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.on_menu_event(|app, e| match e.id().as_ref() {
        "open" => show_main(app),
        "quit" => {
            save_window(app);
            app.exit(0);
        }
        _ => {}
    })
    .on_tray_icon_event(|tray, e| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            show_main(tray.app_handle());
        }
    })
    .build(app)?;
    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// «Сворачивать в трей» in «Настройки»: saved in settings.json; the icon appears or goes at once.
#[tauri::command]
fn set_tray(app: AppHandle, on: bool, open: String, quit: String) -> Prefs {
    let _ = update_settings(&app, |s| s.tray = on);
    if on {
        let _ = ensure_tray(&app, &open, &quit);
    } else if let Some(t) = app.remove_tray_by_id(TRAY_ID) {
        drop(t);
    }
    Prefs { tray: on, ..prefs_now(&app) }
}

/// The minimize button and the auto-minimize after «Играть»: to the tray when it is on, else the taskbar.
#[tauri::command]
fn minimize_window(app: AppHandle) {
    let Some(w) = app.get_webview_window("main") else { return };
    let tray = settings_path(&app).map(|p| nick::load(&p).tray).unwrap_or(false);
    if tray && app.tray_by_id(TRAY_ID).is_some() {
        let _ = w.hide();
    } else {
        let _ = w.minimize();
    }
}

// ---------------------------------------------------------------- app

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // TLS crypto for https requests (news, cfx.re list, updates).
    let _ = rustls::crypto::ring::default_provider().install_default();
    // 0.5.2 (startup.rs): never a silent exit at start; without WebView2 a message with the download page
    startup::install_panic_hook(portable_dir());
    if !startup::check_webview2(portable_dir()) {
        return;
    }
    // Smoke test of the start-error message (README 7л): the same path as a real start-up panic.
    if std::env::var_os("COLDZ_FAKE_START_PANIC").is_some() {
        panic!("COLDZ_FAKE_START_PANIC");
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 0.5.0: also out of the tray
            show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(PendingUpdate(Mutex::new(None)))
        .manage(ReadyUpdate(std::sync::Mutex::new(None)))
        .setup(|app| {
            // Address from the last downloaded news.json, so the status works before a new download.
            let cache = cache_dir(app.handle()).map(|d| d.join("news.json"));
            apply_feed(&local_news(cache).1);
            // 0.4.0: a sign-in route block left in the hosts file from the last run (signin_resume)
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { signin_resume(handle).await });
            // 0.5.0: the saved size and place (or one that fits a small screen), then the window shows
            restore_window(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            detect_fivem,
            pick_fivem,
            choose_route,
            game_build,
            storage_download,
            storage_cancel,
            play,
            close_fivem,
            get_nick,
            send_nick,
            server_status,
            check_network,
            fetch_news,
            open_external,
            check_update,
            update_ready,
            is_portable,
            get_prefs,
            set_tray,
            minimize_window,
            install_update,
            signin_status,
            signin_enable,
            signin_disable,
            crash_hint,
            server_info,
            support_preview,
            support_create,
            join_info,
            crash_pending,
            crash_preview,
            crash_send,
            crash_answer,
            set_crash_ask
        ])
        .build(tauri::generate_context!())
        .expect("error while building COLDZ launcher")
        .run(|app, event| {
            // 0.5.0: the window's size and place for the next start
            if let tauri::RunEvent::WindowEvent { event: tauri::WindowEvent::CloseRequested { .. }, .. } = &event {
                save_window(app);
            }
            // 0.4.0: the hosts block must not outlive the forwarder (signin.rs)
            if let tauri::RunEvent::Exit = event {
                signin_cleanup(app);
                // 0.5.0: a downloaded update goes in quietly now (after the hosts block is gone)
                install_on_exit(app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(
            parse_address("127.0.0.1:30120"),
            Target::Direct { host: "127.0.0.1".into(), port: 30120 }
        );
        assert_eq!(
            parse_address("play.coldz.ru"),
            Target::Direct { host: "play.coldz.ru".into(), port: 30120 }
        );
        assert_eq!(parse_address("cfx.re/join/abc123"), Target::JoinCode("abc123".into()));
        assert_eq!(parse_address("https://cfx.re/join/abc123/"), Target::JoinCode("abc123".into()));
        assert_eq!(parse_address("abc123"), Target::JoinCode("abc123".into()));
    }

    #[test]
    fn builds_connect_links() {
        assert_eq!(
            connect_url(&parse_address("1.2.3.4:30120")),
            "fivem://connect/1.2.3.4:30120"
        );
        assert_eq!(
            connect_url(&parse_address("cfx.re/join/abc123")),
            "fivem://connect/cfx.re/join/abc123"
        );
    }

    #[test]
    fn reads_exe_from_handler_command() {
        let exe = std::env::current_exe().unwrap();
        let cmd = format!("\"{}\" \"%1\"", exe.display());
        assert_eq!(fivem::path_from_command(&cmd), Some(exe));
        // a FiveM that is gone: the path is read, but it does not lead to FiveM.exe (fivem.rs resolve)
        let gone = fivem::path_from_command("\"C:\\nope\\FiveM.exe\" \"%1\"").unwrap();
        assert_eq!(fivem::resolve(&gone), None);
    }

    #[test]
    fn https_requests_work() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let res = rt.block_on(async { http().get("https://github.com/").send().await });
        match res {
            Ok(r) => { eprintln!("github status {}", r.status()); assert!(r.status().as_u16() < 500) }
            // Offline machine: nothing to check. Any other error = broken TLS setup.
            Err(e) if e.is_connect() || e.is_timeout() => eprintln!("offline: {e}"),
            Err(e) => panic!("https request failed: {e:?}"),
        }
    }

    #[test]
    fn validates_addresses() {
        for ok in ["1.2.3.4:30120", "play.coldz.ru", "play.coldz.ru:30121", "cfx.re/join/abc123", "abc123"] {
            assert!(valid_address(ok), "{ok}");
        }
        for bad in ["", "   ", "1.2.3.4:0", "1.2.3.4:99999", "evil host:30120", "a\"b:1", "-x.ru", "x/../y:1", "1.2.3.4:30120?x"] {
            assert!(!valid_address(bad), "{bad}");
        }
    }

    #[test]
    fn news_address_overrides_built_in() {
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "5.6.7.8:30120" }));
        assert_eq!(effective_address().as_deref(), Some("5.6.7.8:30120"));
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "bad host" }));
        assert_ne!(effective_address().as_deref(), Some("bad host"));
        apply_feed(&serde_json::json!({ "items": [] }));
        let built_in = config().server_address.trim();
        assert_eq!(effective_address(), valid_address(built_in).then(|| built_in.to_string()));

        // 0.3.5 reserve routes (same test: these are the same global slots). No `relays` key: the built-in ones.
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "92.118.11.28:28683" }));
        assert_eq!(effective_relays(), clean_relays(&config().relays, Some("92.118.11.28:28683")));
        assert_eq!(effective_relays(), s(&["177.3.198.34:30120", "46.247.40.34:30120"]));
        // news.json changes them without a release
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "92.118.11.28:28683", "relays": ["5.6.7.8:30120", "bad host", 7] }));
        assert_eq!(effective_relays(), s(&["5.6.7.8:30120"]));
        // an empty list: no reserve routes
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "92.118.11.28:28683", "relays": [] }));
        assert!(effective_relays().is_empty());
        // a chosen reserve route that news.json no longer lists is dropped: back to the main address
        *current_route().write().unwrap() = Some(route::Route { kind: "reserve", address: "177.3.198.34:30120".into() });
        assert_eq!(valid_route(), None);
        assert_eq!(route_address().as_deref(), Some("92.118.11.28:28683"));
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "92.118.11.28:28683" }));
        assert_eq!(route_address().as_deref(), Some("177.3.198.34:30120"));
        assert_eq!(valid_route().map(|r| r.kind), Some("reserve"));
        *current_route().write().unwrap() = None;
        // 0.5.4: gameBuild.preset from news.json is kept, a feed without it brings launcher.config.json back
        apply_feed(&serde_json::json!({ "items": [], "gameBuild": { "preset": false } }));
        assert_eq!(*remote_preset().read().unwrap(), Some(false));
        assert!(!preset_on(*remote_preset().read().unwrap(), &config().game_build));
        apply_feed(&serde_json::json!({ "items": [] }));
        assert_eq!(*remote_preset().read().unwrap(), None);
        assert!(preset_on(*remote_preset().read().unwrap(), &config().game_build));

        // 0.5.5 (firstjoin.rs, the same global slots): no keys in news.json = launcher.config.json
        let built_in = firstjoin::Setup { url: "https://cdn.coldz.vip/speed/8mb.bin".into(), gb: 6.0, update_hint: false };
        assert_eq!(join_setup(), built_in);
        // news.json wins: another test file, another size, the update line
        apply_feed(&serde_json::json!({ "items": [], "cdnSpeedUrl": "https://cdn2.coldz.vip/speed/8mb.bin", "firstJoinGb": 6.5, "updateHint": true }));
        assert_eq!(join_setup(), firstjoin::Setup { url: "https://cdn2.coldz.vip/speed/8mb.bin".into(), gb: 6.5, update_hint: true });
        // an empty address: no measuring; 0 GB: no first-join line
        apply_feed(&serde_json::json!({ "items": [], "cdnSpeedUrl": "", "firstJoinGb": 0 }));
        assert_eq!(join_setup(), firstjoin::Setup { url: String::new(), gb: 0.0, update_hint: false });
        let off = firstjoin::info(true, &join_setup(), None);
        assert_eq!((off.cdn.state, off.advice.kind), ("off", "none"));
        // bad values in news.json: the built-in ones again
        apply_feed(&serde_json::json!({ "items": [], "cdnSpeedUrl": "http://cdn.coldz.vip/x", "firstJoinGb": "6", "updateHint": 1 }));
        assert_eq!(join_setup(), built_in);
        apply_feed(&serde_json::json!({ "items": [] }));
        assert_eq!(join_setup(), built_in);

        // 0.5.6 (crashreport.rs, the same global slots): launcher.config.json says on; the signed news.json switches the
        // reports off (and the settings card hides its checkbox), only the dump off, another receiver; no key: back
        let built_in = crash_setup();
        assert!(built_in.enabled && built_in.dump);
        apply_feed(&serde_json::json!({ "items": [], "crashReport": { "enabled": false } }));
        assert!(!crash_setup().enabled);
        apply_feed(&serde_json::json!({ "items": [], "crashReport": { "dump": false, "url": "https://coldz.vip/crash/v1/report" } }));
        let s = crash_setup();
        assert!(s.enabled && !s.dump && s.url == "https://coldz.vip/crash/v1/report");
        apply_feed(&serde_json::json!({ "items": [], "crashReport": { "url": "http://plain.example/report", "enabled": "no" } }));
        assert_eq!(crash_setup(), built_in);
        apply_feed(&serde_json::json!({ "items": [] }));
        assert_eq!(crash_setup(), built_in);
    }

    #[test]
    fn reserve_routes_are_cleaned() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let raw = s(&[" 177.3.198.34:30120 ", "177.3.198.34:30120", "relay.example", "cfx.re/join/abc123", "92.118.11.28:28683", "x y:1", "1.2.3.4:0"]);
        assert_eq!(clean_relays(&raw, Some("92.118.11.28:28683")), s(&["177.3.198.34:30120", "relay.example:30120"]));
        let many: Vec<String> = (1..=20).map(|i| format!("10.0.0.{i}:30120")).collect();
        assert_eq!(clean_relays(&many, None).len(), route::MAX_RELAYS);
    }

    /// A one-shot fake server on 127.0.0.1: reads one HTTP request, answers `status` + `body`, returns the request.
    async fn fake_server(status: u16, body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_string();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text[..head_end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + len || n == 0 {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&req).to_string()
        });
        (nick::endpoint("127.0.0.1", addr.port()), task)
    }

    // 0.5.4: what the server asks FiveM for comes from /info.json of the route (gamebuild.rs)
    #[test]
    fn game_build_from_the_server() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let port = |url: &str| url.trim_start_matches("http://").split('/').next().unwrap().to_string();
            let info = r#"{"vars":{"sv_enforceGameBuild":"3258","sv_defaultGameBuild":"3889","sv_pureLevel":"1","sv_poolSizesIncrease":"{\"InteriorProxy\":450,\"PortalInst\":100}"}}"#;
            let (url, server) = fake_server(200, info).await;
            let w = ask_game_build(&port(&url), Duration::from_secs(3)).await.unwrap();
            assert!(server.await.unwrap().starts_with("GET /info.json HTTP/1.1"));
            assert_eq!(w.describe(), "сборка 3258, exe 3889, pure 1, пулы {\"InteriorProxy\":450,\"PortalInst\":100}");

            let (url, _s) = fake_server(404, "{}").await;
            assert_eq!(ask_game_build(&port(&url), Duration::from_secs(3)).await.unwrap_err(), "/info.json: HTTP 404");
            let (url, _s) = fake_server(200, "<html>").await;
            assert_eq!(ask_game_build(&port(&url), Duration::from_secs(3)).await.unwrap_err(), "/info.json не JSON");
            let (url, _s) = fake_server(200, r#"{"vars":{"sv_projectName":"COLDZ"}}"#).await;
            assert_eq!(ask_game_build(&port(&url), Duration::from_secs(3)).await.unwrap_err(), "нет sv_enforceGameBuild");
            // nobody there: an error within the wait
            let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
            let t = Instant::now();
            assert_eq!(ask_game_build(&free.to_string(), Duration::from_secs(2)).await.unwrap_err(), "нет ответа /info.json");
            assert!(t.elapsed() < Duration::from_secs(3));
        });
    }

    // 0.5.4: the «Continue» line on 4 languages, neutral (docs/ПРАВИЛА.md: no long dashes, nothing about ways around
    // blocks), and the window asks for the game build before «play»
    #[test]
    fn game_build_line_in_the_window() {
        let i18n = include_str!("../../ui/i18n.js");
        let lines: Vec<&str> = i18n.lines().filter(|l| l.trim_start().starts_with("launched_build:")).collect();
        assert_eq!(lines.len(), 4, "ru, en, es, pt-BR");
        for l in &lines {
            assert!(l.contains("Continue") && l.contains("FiveM"), "{l}");
            let low = l.to_lowercase();
            for bad in ["—", "vpn", "ускорит", "accelerat", "acelerad", "обход"] {
                assert!(!low.contains(bad), "{bad} in {l}");
            }
        }
        let app = include_str!("../../ui/app.js");
        let ask = app.find("invoke(\"game_build\")").expect("game_build asked");
        let download = app.find("await invoke(\"storage_download\")").expect("storage_download");
        let play = app.find("await invoke(\"play\")").expect("play");
        assert!(ask < play && download < play);
        assert!(app.contains("\"launched_build\""));
        // 0.5.4 (storage.rs): the download texts on 4 languages, every error key has its line
        for key in ["progress", "checking", "button", "cancel", "skip", "paused", "network", "remote", "bad", "space", "disk"] {
            let lines: Vec<&str> = i18n.lines().filter(|l| l.trim_start().starts_with(&format!("storage_{key}:"))).collect();
            assert_eq!(lines.len(), 4, "storage_{key}");
            for l in &lines {
                let low = l.to_lowercase();
                for bad in ["—", "vpn", "ускорит", "accelerat", "acelerad", "обход"] {
                    assert!(!low.contains(bad), "{bad} in {l}");
                }
            }
        }
        for key in ["cancelled", "network", "remote", "bad", "space", "disk"] {
            assert!(app.contains(&format!("{key}: \"storage_")), "{key}");
        }

        // 0.5.5 (firstjoin.rs): the first-join line and the file-server line on 4 languages, simple and neutral: no
        // long dashes, no «VPN», no «ускоритель», nothing about ways around blocks
        let bad_words = ["—", "–", "vpn", "ускорит", "accelerat", "acelerad", "обход", "bypass"];
        for key in ["join_first", "join_first_nospeed", "join_slow", "join_update", "min_one", "min_other", "svc_cdn", "net_cdn_speed", "net_cdn_fail", "net_cdn_none"] {
            let lines: Vec<&str> = i18n.lines().filter(|l| l.trim_start().starts_with(&format!("{key}:"))).collect();
            assert_eq!(lines.len(), 4, "{key}: ru, en, es, pt-BR");
            for l in &lines {
                let low = l.to_lowercase();
                for bad in bad_words {
                    assert!(!low.contains(bad), "{bad} in {l}");
                }
            }
        }
        for l in i18n.lines().filter(|l| l.trim_start().starts_with("join_first")) {
            assert!(l.contains("{g}"), "the size: {l}");
            assert_eq!(l.contains("{m}"), l.trim_start().starts_with("join_first:"), "minutes only with the speed: {l}");
        }
        // the Russian lines as Alexander wrote them (20.11)
        assert!(i18n.contains("join_first: \"Первый вход: игра скачает около {g} ГБ файлов сервера, при вашей скорости это примерно {m}. Не закрывайте игру на экране загрузки.\""));
        assert!(i18n.contains("join_slow: \"Связь с сервером файлов медленная: лучше закрыть торренты и другие загрузки.\""));
        assert!(i18n.contains("join_update: \"Обновления докачаются за минуту-другую.\""));
        for key in ["min_few", "min_many"] {
            assert!(i18n.contains(&format!("{key}: \"{{n}} минут")), "{key}: Russian plural forms");
        }
        assert!(app.contains("invoke(\"join_info\""));
    }

    // 0.5.6 (crashreport.rs): the crash-report texts on 4 languages, simple and neutral; «Играть» never waits for them;
    // the memory snapshot starts unticked
    #[test]
    fn crash_report_texts_in_the_window() {
        let i18n = include_str!("../../ui/i18n.js");
        let bad_words = ["—", "–", "vpn", "ускорит", "accelerat", "acelerad", "обход", "bypass"];
        let keys = [
            "crash_ask", "crash_ask_nohash", "crash_send", "crash_no", "crash_never", "crash_title", "crash_text", "crash_text_nohash",
            "crash_loading", "crash_item_crashometry", "crash_item_log", "crash_item_modules", "crash_item_info", "crash_dump",
            "crash_note", "crash_go", "crash_cancel", "crash_sending", "crash_sent", "crash_sent_nodump", "crash_queued",
            "crash_failed", "pref_crash",
        ];
        for key in keys {
            let lines: Vec<&str> = i18n.lines().filter(|l| l.trim_start().starts_with(&format!("{key}:"))).collect();
            assert_eq!(lines.len(), 4, "{key}: ru, en, es, pt-BR");
            for l in &lines {
                let low = l.to_lowercase();
                for bad in bad_words {
                    assert!(!low.contains(bad), "{bad} in {l}");
                }
            }
        }
        for l in i18n.lines().filter(|l| l.trim_start().starts_with("crash_ask:") || l.trim_start().starts_with("crash_text:")) {
            assert!(l.contains("{h}") && l.contains("{d}"), "the place and the date: {l}");
        }
        for l in i18n.lines().filter(|l| l.trim_start().starts_with("crash_dump:")) {
            assert!(l.contains("{s}"), "the size: {l}");
        }
        assert!(i18n.contains("crash_ask: \"Игра закрылась с ошибкой {h} ({d}). Отправить отчёт разработчикам?\""));
        // every item the card lists has its line (crashreport.rs `preview`)
        for key in ["crashometry", "log", "modules", "info"] {
            assert!(i18n.contains(&format!("crash_item_{key}:")), "{key}");
        }
        let app = include_str!("../../ui/app.js");
        for cmd in ["crash_pending", "crash_preview", "crash_send", "crash_answer", "set_crash_ask"] {
            assert!(app.contains(&format!("invoke(\"{cmd}\"")), "{cmd}");
        }
        // «Играть» does not touch crash reports at all
        let play = app.find("async function play(").expect("play");
        let after = app[play..].find("// ------------------------------------------------------------ news + rules").expect("next section");
        assert!(!app[play..play + after].contains("crash"), "play() must not wait for crash reports");
        // the snapshot only with its own tick, unticked every time the card opens
        assert!(app.contains("$(\"crash-dump\").checked = false;"));
        let html = include_str!("../../ui/index.html");
        assert!(html.contains("<input type=\"checkbox\" id=\"crash-dump\">"), "unticked in the page");
    }

    #[test]
    fn nick_goes_to_the_server_as_json() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (url, server) = fake_server(200, r#"{"ok":true,"nick":"Снег"}"#).await;
            let res = post_nick(&url, "Снег", "abc123").await;
            let req = server.await.unwrap();
            assert_eq!(res, nick::NickResult { status: "ok", nick: "Снег".into(), reason: None });
            assert!(req.starts_with("POST /z_menu/nick HTTP/1.1"), "{req}");
            let body = &req[req.find("\r\n\r\n").unwrap() + 4..];
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(v, serde_json::json!({ "nick": "Снег", "launcher": "abc123" }));

            let (url, server) = fake_server(200, r#"{"ok":false,"reason":"reserved"}"#).await;
            let res = post_nick(&url, "Админ", "abc123").await;
            server.await.unwrap();
            assert_eq!((res.status, res.reason.as_deref()), ("rejected", Some("reserved")));

            let (url, server) = fake_server(429, "Rate limit exceeded.").await;
            let res = post_nick(&url, "x", "abc123").await;
            server.await.unwrap();
            assert_eq!(res.reason.as_deref(), Some("often"));

            // old server without the nick: FXServer answers 404 — FiveM still starts
            let (url, server) = fake_server(404, "Not found.").await;
            assert_eq!(post_nick(&url, "x", "abc123").await.status, "unreachable");
            server.await.unwrap();

            // nobody listens
            let closed = {
                let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                l.local_addr().unwrap().port()
            };
            assert_eq!(post_nick(&nick::endpoint("127.0.0.1", closed), "x", "abc").await.status, "unreachable");
        });
    }

    /// 0.5.0: news.json in the repo is signed with the news key (scripts/sign-news.py): the launcher accepts it; a
    /// changed byte or another key is refused. Fails when news.json was edited without signing it again.
    #[test]
    fn news_signature() {
        let pubkey = &config().news_pubkey;
        assert!(!pubkey.is_empty(), "launcher.config.json newsPubkey");
        let updater_key = include_str!("../tauri.conf.json");
        assert!(!updater_key.contains(pubkey.as_str()), "the news key must not be the updater key");
        let sig = include_str!("../../news/news.json.sig");
        assert!(news_signed(BUNDLED_NEWS.as_bytes(), sig, pubkey), "news/news.json.sig does not match: run python scripts/sign-news.py");
        let mut changed = BUNDLED_NEWS.as_bytes().to_vec();
        let i = changed.iter().position(|b| *b == b'1').unwrap();
        changed[i] = b'2';
        assert!(!news_signed(&changed, sig, pubkey));
        assert!(!news_signed(BUNDLED_NEWS.as_bytes(), "", pubkey));
        assert!(!news_signed(BUNDLED_NEWS.as_bytes(), "bm90IGEgc2lnbmF0dXJl", pubkey));
        // the updater's key does not open the news
        let conf: serde_json::Value = serde_json::from_str(updater_key).unwrap();
        let upd = conf.pointer("/plugins/updater/pubkey").and_then(|k| k.as_str()).unwrap();
        assert!(!news_signed(BUNDLED_NEWS.as_bytes(), sig, upd));
        assert!(check_news(BUNDLED_NEWS.as_bytes().to_vec(), sig, pubkey).is_some());
        assert!(check_news(b"{\"x\":1}".to_vec(), "", "").is_none(), "no items: not news");
        // addresses: GitHub first, coldz.vip after it
        let urls = news_urls();
        assert!(urls[0].starts_with("https://raw.githubusercontent.com/"), "{urls:?}");
        assert!(urls.iter().any(|u| u == "https://coldz.vip/launcher/news.json"), "{urls:?}");
    }

    /// 0.5.0: two addresses: a dead one and a slow good one: the good one wins; a newer signedAt wins over an older
    /// one that came first; a wrong signature never wins.
    #[test]
    fn news_from_the_first_good_address() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            // serves /news.json and /news.json.sig, after `delay_ms`
            async fn site(body: String, sig: String, delay_ms: u64) -> String {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = l.local_addr().unwrap().port();
                tokio::spawn(async move {
                    loop {
                        let Ok((mut c, _)) = l.accept().await else { return };
                        let (body, sig) = (body.clone(), sig.clone());
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 2048];
                            let n = c.read(&mut buf).await.unwrap_or(0);
                            let req = String::from_utf8_lossy(&buf[..n]).to_string();
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                            let text = if req.starts_with("GET /news.json.sig") { sig } else { body };
                            let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
                            let _ = c.write_all(reply.as_bytes()).await;
                        });
                    }
                });
                format!("http://127.0.0.1:{port}/news.json")
            }
            let dead = "http://127.0.0.1:1/news.json".to_string();
            let a = r#"{"items":[],"signedAt":"2026-11-18T10:00:00Z"}"#.to_string();
            let b = r#"{"items":[],"signedAt":"2026-11-18T12:00:00Z"}"#.to_string();
            // no key: any valid file counts (the signature itself is tested above)
            let slow = site(a.clone(), String::new(), 300).await;
            let got = first_signed(vec![dead.clone(), slow.clone()], String::new()).await.unwrap();
            assert_eq!(got.1, slow);
            let newer_late = site(b.clone(), String::new(), 900).await;
            let older_fast = site(a.clone(), String::new(), 50).await;
            let got = first_signed(vec![older_fast, newer_late.clone()], String::new()).await.unwrap();
            assert_eq!((got.1.as_str(), signed_at(&got.0.value).as_str()), (newer_late.as_str(), "2026-11-18T12:00:00Z"));
            // with the real key: an unsigned copy is not taken
            let unsigned = site(a, "bm90IGEgc2lnbmF0dXJl".into(), 10).await;
            assert!(first_signed(vec![unsigned, dead], config().news_pubkey.clone()).await.is_none());
        });
    }

    /// 0.5.0: the server's answers about the restart (0.5.3: the online graph is gone).
    #[test]
    fn restart_and_online_from_the_server() {
        let v = serde_json::json!({ "ok": true, "now": 1_000_000, "step": 300, "max": 48, "restartAt": 1_000_420,
            "points": [[999_600, 5], [999_900, 7], ["x", 1], [999_950]] });
        let i = parse_online(&v).unwrap();
        assert_eq!(i.restart_in, Some(420));
        // no restart, or one in the past (the server clock decides, not the PC)
        assert_eq!(parse_online(&serde_json::json!({ "ok": true, "now": 10, "restartAt": 0, "points": [] })).unwrap().restart_in, None);
        assert_eq!(parse_online(&serde_json::json!({ "ok": true, "now": 10, "restartAt": 5 })).unwrap().restart_in, None);
        assert_eq!(parse_online(&serde_json::json!({ "ok": false })), None);
        // FXServer 404 body and the like
        assert_eq!(parse_online(&serde_json::json!({ "error": "not found" })), None);
        // the older server: /info.json vars (FXServer gives every var as a string)
        let info = serde_json::json!({ "vars": { "coldz_restart_at": "1000120", "sv_maxClients": "48" } });
        assert_eq!(parse_info_restart(&info, 1_000_000), Some(120));
        assert_eq!(parse_info_restart(&serde_json::json!({ "vars": { "coldz_restart_at": "0" } }), 1_000_000), None);
        assert_eq!(parse_info_restart(&serde_json::json!({ "vars": {} }), 1_000_000), None);
    }

    /// 0.5.0: the portable build is told by `portable.txt` next to the exe; data then lives in `data\` beside it.
    #[test]
    fn portable_marker() {
        let d = std::env::temp_dir().join(format!("coldz-portable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join("COLDZ Launcher.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        assert_eq!(portable_dir_of(&exe), None);
        std::fs::write(d.join(PORTABLE_MARK), b"").unwrap();
        assert_eq!(portable_dir_of(&exe), Some(d.join("data")));
        let _ = std::fs::remove_dir_all(&d);
        // the test exe itself is not portable
        assert_eq!(portable_dir(), None);
    }

    #[test]
    fn old_news_gets_bundled_rules() {
        let v = with_rules(serde_json::json!({ "items": [] }));
        assert!(v.get("rules").and_then(|r| r.as_array()).map_or(false, |r| !r.is_empty()));
        let own = with_rules(serde_json::json!({ "items": [], "rules": [{ "title": { "ru": "x" }, "text": { "ru": "y" } }] }));
        assert_eq!(own["rules"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn instagram_and_tiktok_links_per_window_language() {
        // 20.11: Instagram per window language: ru and es — the main account, en — _en, pt-BR — _br; one button each
        let c = config();
        for (lang, url) in [
            ("ru", "https://www.instagram.com/coldz_gta5_dayz/"),
            ("es", "https://www.instagram.com/coldz_gta5_dayz/"),
            ("en", "https://www.instagram.com/coldz_gta5_dayz_en/"),
            ("pt-BR", "https://www.instagram.com/coldz_gta5_dayz_br/"),
        ] {
            let shown: Vec<&Link> = c
                .links
                .iter()
                .filter(|l| l.icon == "instagram" && (l.langs.is_empty() || l.langs.iter().any(|x| x == lang)))
                .collect();
            assert_eq!(shown.len(), 1, "{lang}");
            assert_eq!(shown[0].url, url, "{lang}");
        }
        // 20.11: TikTok the same way: ru and es — @coldz_gta5, en — _en, pt-BR — _br
        for (lang, url) in [
            ("ru", "https://www.tiktok.com/@coldz_gta5"),
            ("es", "https://www.tiktok.com/@coldz_gta5"),
            ("en", "https://www.tiktok.com/@coldz_gta5_en"),
            ("pt-BR", "https://www.tiktok.com/@coldz_gta5_br"),
        ] {
            let shown: Vec<&Link> = c
                .links
                .iter()
                .filter(|l| l.icon == "tiktok" && (l.langs.is_empty() || l.langs.iter().any(|x| x == lang)))
                .collect();
            assert_eq!(shown.len(), 1, "tiktok {lang}");
            assert_eq!(shown[0].url, url, "tiktok {lang}");
        }
        for l in &c.links {
            for x in &l.langs {
                assert!(["ru", "en", "es", "pt-BR"].contains(&x.as_str()), "unknown lang {x}");
            }
        }
        // langs reaches the window (get_config serializes Link)
        let v = serde_json::to_value(&c.links).unwrap();
        assert!(v.as_array().unwrap().iter().any(|l| l["langs"] == serde_json::json!(["en"])));
    }

    // 0.5.4: the remote switch for CitizenFX.ini: news.json `gameBuild.preset` wins, a missing or empty key = the
    // built-in launcher.config.json
    #[test]
    fn game_build_switch() {
        use serde_json::json;
        let on = GameBuildSwitch { preset: true };
        let off = GameBuildSwitch { preset: false };
        // built in: on by default, and the shipped launcher.config.json says on
        assert!(config().game_build.preset);
        let cfg: serde_json::Value = serde_json::from_str(CONFIG_JSON).unwrap();
        assert_eq!(cfg["gameBuild"], json!({ "preset": true }));
        assert_eq!(serde_json::from_value::<GameBuildSwitch>(json!({})).unwrap(), on);
        // news.json says
        assert_eq!(preset_of_feed(&json!({ "gameBuild": { "preset": false } })), Some(false));
        assert_eq!(preset_of_feed(&json!({ "gameBuild": { "preset": true } })), Some(true));
        assert!(!preset_on(Some(false), &on));
        assert!(preset_on(Some(true), &off));
        // no key, an empty object, a null, a string: launcher.config.json decides
        for feed in [json!({}), json!({ "gameBuild": {} }), json!({ "gameBuild": null }), json!({ "gameBuild": { "preset": "no" } }), json!({ "gameBuild": [] })] {
            assert_eq!(preset_of_feed(&feed), None, "{feed}");
            assert!(preset_on(preset_of_feed(&feed), &on));
            assert!(!preset_on(preset_of_feed(&feed), &off));
        }
        // apply_feed keeps it: news_address_overrides_built_in (the same globals, one test at a time)
    }

    #[test]
    fn config_and_news_are_valid() {
        let c = config();
        assert!(c.server_address.trim().is_empty() || valid_address(&c.server_address));
        assert!(!c.owner_email.is_empty());
        for l in &c.links {
            assert!(l.url.is_empty() || l.url.starts_with("https://"), "link must be https: {}", l.url);
        }
        // 0.5.5: the test file on our file server and the first-join size (firstjoin.rs)
        assert_eq!(c.cdn_speed_url, "https://cdn.coldz.vip/speed/8mb.bin");
        assert!(firstjoin::valid_url(&c.cdn_speed_url));
        assert_eq!(c.first_join_gb, 6.0);
        // 0.5.6: crash reports on, an https receiver on our file server, a client key (no secret)
        assert!(c.crash_report.enabled && c.crash_report.dump);
        assert_eq!(c.crash_report.url, "https://cdn.coldz.vip/crash/v1/report");
        assert!(crashreport::valid_url(&c.crash_report.url) && crashreport::valid_key(&c.crash_report.key));
        let news: serde_json::Value = serde_json::from_str(BUNDLED_NEWS).unwrap();
        assert!(valid_news(&news));
        if let Some(a) = news.get("serverAddress").and_then(|a| a.as_str()) {
            assert!(a.trim().is_empty() || valid_address(a), "news.json serverAddress: {a}");
        }
        // 0.3.5: every reserve route (built-in and in news.json) is a plain host:port the launcher accepts
        let news_relays: Vec<String> = news
            .get("relays")
            .and_then(|r| r.as_array())
            .map(|l| l.iter().map(|r| r.as_str().unwrap_or("?").to_string()).collect())
            .unwrap_or_default();
        for list in [&c.relays, &news_relays] {
            assert_eq!(&clean_relays(list, None), list, "relays must be plain host:port: {list:?}");
        }
        for r in news.get("rules").and_then(|r| r.as_array()).into_iter().flatten() {
            assert!(r.pointer("/title/ru").and_then(|t| t.as_str()).is_some(), "rule without ru title");
            assert!(r.pointer("/text/ru").and_then(|t| t.as_str()).is_some(), "rule without ru text");
        }
    }
}
