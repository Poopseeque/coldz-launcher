//! Route to the COLDZ server (0.3.5, Alexander 16.11.2026).
//!
//! On «Играть» the launcher asks the main server first (`serverAddress` from news.json) for about three seconds. It
//! answers: FiveM connects straight to it, as before. It does not: the launcher asks the reserve routes (`relays` in
//! news.json, built-in defaults in launcher.config.json) and connects to the first one that answers. A reserve route
//! that worked last time (settings.json `lastRoute`) is preferred among the reserve routes; the main server is still
//! asked first every time.
//!
//! A route "answers" = `GET http://<address>/info.json` returns 200 with a JSON object (netcheck.rs
//! `server_answers_within`): the first request FiveM itself makes on every join. All routes are asked at once, so the
//! choice never takes longer than `PROBE_TIMEOUT`; when the main server answers, the reserve routes are not waited for.
//!
//! Player texts (ui/i18n.js `route_main`, `route_reserve`): neutral, «Подключение: основной сервер» /
//! «Подключение: резервный маршрут» (docs/ПРАВИЛА.md: no words about ways around blocks, anywhere).

use serde::Serialize;
use std::time::Duration;

/// How long «Играть» waits for one route (all are asked at once).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// More reserve routes than this in news.json are ignored.
pub const MAX_RELAYS: usize = 8;

/// The route the launcher connects through.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Route {
    /// "main" — the main server answered; "reserve" — the main server did not, a reserve route did;
    /// "none" — nothing answered (FiveM still goes to the main server, the player decides).
    pub kind: &'static str,
    /// `host:port` FiveM connects to.
    pub address: String,
}

impl Route {
    pub fn main(address: &str) -> Self {
        Route { kind: "main", address: address.to_string() }
    }
}

/// What answered: the main server and each reserve route (same order as the list).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answers {
    pub main: bool,
    pub relays: Vec<bool>,
}

impl Answers {
    /// Some reserve route answered. None — there are no reserve routes.
    pub fn reserve(&self) -> Option<bool> {
        (!self.relays.is_empty()).then(|| self.relays.iter().any(|&up| up))
    }
}

/// The order the reserve routes are preferred in: the one that worked last time first, then the list order.
pub fn order(relays: &[String], last: &str) -> Vec<usize> {
    let last = last.trim();
    let first = relays.iter().position(|r| !last.is_empty() && r == last);
    first.into_iter().chain((0..relays.len()).filter(|&i| Some(i) != first)).collect()
}

/// The route from the answers: the main server if it answered, else the preferred reserve route that answered, else
/// "none" (the main server's address).
pub fn choose(primary: &str, relays: &[String], answers: &Answers, last: &str) -> Route {
    if answers.main {
        return Route::main(primary);
    }
    for i in order(relays, last) {
        if answers.relays.get(i).copied().unwrap_or(false) {
            return Route { kind: "reserve", address: relays[i].clone() };
        }
    }
    Route { kind: "none", address: primary.to_string() }
}

fn info_url(address: &str) -> String {
    format!("http://{address}/info.json")
}

async fn answers(address: String, wait: Duration) -> bool {
    crate::netcheck::server_answers_within(&info_url(&address), wait).await
}

/// «Играть»: asks the main server and every reserve route at once, returns as soon as the choice is known (the main
/// server answered, or the preferred reserve route that answers). At most about `wait`.
pub async fn pick(primary: &str, relays: &[String], last: &str, wait: Duration) -> Route {
    let mut tasks: Vec<_> = relays.iter().map(|r| Some(tokio::spawn(answers(r.clone(), wait)))).collect();
    let stop_all = |tasks: &mut Vec<Option<tokio::task::JoinHandle<bool>>>| {
        for t in tasks.iter_mut().filter_map(Option::take) {
            t.abort();
        }
    };
    if answers(primary.to_string(), wait).await {
        stop_all(&mut tasks);
        return Route::main(primary);
    }
    // in preference order: the first one that answers wins (they all run at once, so this waits for the slowest
    // needed one at most)
    for i in order(relays, last) {
        let Some(task) = tasks[i].take() else { continue };
        if task.await.unwrap_or(false) {
            stop_all(&mut tasks);
            return Route { kind: "reserve", address: relays[i].clone() };
        }
    }
    Route { kind: "none", address: primary.to_string() }
}

/// «Проверить связь»: asks the main server and every reserve route at once and waits for all of them (the window
/// shows whether a reserve route works even when the main server answers).
pub async fn probe_all(primary: &str, relays: &[String], wait: Duration) -> Answers {
    let tasks: Vec<_> = relays.iter().map(|r| tokio::spawn(answers(r.clone(), wait))).collect();
    let main = answers(primary.to_string(), wait).await;
    let mut up = Vec::with_capacity(tasks.len());
    for t in tasks {
        up.push(t.await.unwrap_or(false));
    }
    Answers { main, relays: up }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn rt() -> tokio::runtime::Runtime {
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    fn list(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    const WARSAW: &str = "177.3.198.34:30120";
    const ALMATY: &str = "46.247.40.34:30120";
    const MAIN: &str = "92.118.11.28:28683";

    #[test]
    fn last_working_reserve_route_goes_first() {
        let relays = list(&[WARSAW, ALMATY]);
        assert_eq!(order(&relays, ""), [0, 1]);
        assert_eq!(order(&relays, WARSAW), [0, 1]);
        assert_eq!(order(&relays, ALMATY), [1, 0]);
        // the main server or a route that is no longer in the list: the list order
        assert_eq!(order(&relays, MAIN), [0, 1]);
        assert_eq!(order(&relays, "1.2.3.4:30120"), [0, 1]);
        assert!(order(&[], ALMATY).is_empty());
    }

    #[test]
    fn chooses_main_then_reserve_in_order() {
        let relays = list(&[WARSAW, ALMATY]);
        let a = |main, r: &[bool]| Answers { main, relays: r.to_vec() };
        // the main server answers: always straight to it, even when the last route was a reserve one
        assert_eq!(choose(MAIN, &relays, &a(true, &[true, true]), ALMATY), Route::main(MAIN));
        assert_eq!(choose(MAIN, &relays, &a(true, &[false, false]), ""), Route::main(MAIN));
        // the main server does not answer: Warsaw, then Almaty
        assert_eq!(choose(MAIN, &relays, &a(false, &[true, true]), "").address, WARSAW);
        assert_eq!(choose(MAIN, &relays, &a(false, &[true, true]), "").kind, "reserve");
        assert_eq!(choose(MAIN, &relays, &a(false, &[false, true]), "").address, ALMATY);
        // Almaty worked last time and answers: Almaty first; it does not answer now: Warsaw
        assert_eq!(choose(MAIN, &relays, &a(false, &[true, true]), ALMATY).address, ALMATY);
        assert_eq!(choose(MAIN, &relays, &a(false, &[true, false]), ALMATY).address, WARSAW);
        // nothing answers: the main address, the player decides («Всё равно играть»)
        assert_eq!(choose(MAIN, &relays, &a(false, &[false, false]), ""), Route { kind: "none", address: MAIN.into() });
        assert_eq!(choose(MAIN, &[], &a(false, &[]), ""), Route { kind: "none", address: MAIN.into() });
        // fewer answers than routes (should not happen): the missing ones count as silent
        assert_eq!(choose(MAIN, &relays, &a(false, &[]), "").kind, "none");

        assert_eq!(a(true, &[]).reserve(), None);
        assert_eq!(a(true, &[false, true]).reserve(), Some(true));
        assert_eq!(a(false, &[false, false]).reserve(), Some(false));
    }

    #[test]
    fn route_json_for_the_window() {
        let v = serde_json::to_value(Route { kind: "reserve", address: WARSAW.into() }).unwrap();
        assert_eq!(v, serde_json::json!({ "kind": "reserve", "address": WARSAW }));
    }

    #[derive(Clone, Copy)]
    enum Fake {
        /// FXServer's /info.json
        Info,
        /// accept and never answer
        Silent,
    }

    /// A fake route on 127.0.0.1, returns `127.0.0.1:<port>`.
    async fn fake(kind: Fake) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
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
                        Fake::Info => {
                            if !got.starts_with(b"GET /info.json HTTP/1.1") {
                                return;
                            }
                            let body = r#"{"vars":{"sv_maxClients":"48"},"server":"FXServer"}"#;
                            let head = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = sock.write_all(head.as_bytes()).await;
                            let _ = sock.write_all(body.as_bytes()).await;
                        }
                        Fake::Silent => tokio::time::sleep(Duration::from_secs(30)).await,
                    }
                });
            }
        });
        format!("127.0.0.1:{port}")
    }

    fn closed() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("127.0.0.1:{}", l.local_addr().unwrap().port())
    }

    #[test]
    fn picks_the_route_that_answers() {
        rt().block_on(async {
            let wait = Duration::from_millis(1200);

            // the main server answers: straight to it, a silent reserve route is not waited for
            let main = fake(Fake::Info).await;
            let silent = fake(Fake::Silent).await;
            let started = Instant::now();
            assert_eq!(pick(&main, &[silent.clone()], "", wait).await, Route::main(&main));
            assert!(started.elapsed() < Duration::from_millis(800), "{:?}", started.elapsed());

            // the main server is down (nobody listens): the first reserve route
            let (r1, r2) = (fake(Fake::Info).await, fake(Fake::Info).await);
            let down = closed();
            assert_eq!(pick(&down, &[r1.clone(), r2.clone()], "", wait).await, Route { kind: "reserve", address: r1.clone() });
            // the second one worked last time: it goes first
            assert_eq!(pick(&down, &[r1.clone(), r2.clone()], &r2, wait).await.address, r2);

            // the main server hangs, the first reserve route hangs too: the second one, within about `wait`
            let hang = fake(Fake::Silent).await;
            let started = Instant::now();
            assert_eq!(pick(&hang, &[silent.clone(), r2.clone()], "", wait).await.address, r2);
            assert!(started.elapsed() < wait + Duration::from_millis(600), "{:?}", started.elapsed());

            // nothing answers: "none", the main address
            assert_eq!(pick(&down, &[closed(), silent.clone()], "", wait).await, Route { kind: "none", address: down.clone() });
            assert_eq!(pick(&down, &[], "", wait).await.kind, "none");

            // the connection check waits for everything
            let all = probe_all(&main, &[r1.clone(), down.clone(), silent.clone()], wait).await;
            assert_eq!(all, Answers { main: true, relays: vec![true, false, false] });
        });
    }

    /// Manual check against the real reserve routes (16.11 brief): the main address is unreachable (10.255.255.1:1,
    /// a black hole), so the chooser must pick Warsaw. Not in `npm test` (it asks the real routes, which lead to our
    /// live server): `cargo test --manifest-path src-tauri/Cargo.toml real_routes -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_routes_pick_warsaw_when_main_is_unreachable() {
        rt().block_on(async {
            let relays = list(&[WARSAW, ALMATY]);
            let started = Instant::now();
            let route = pick("10.255.255.1:1", &relays, "", PROBE_TIMEOUT).await;
            eprintln!("route {route:?} in {:?}", started.elapsed());
            assert_eq!(route, Route { kind: "reserve", address: WARSAW.into() });
            assert!(started.elapsed() < PROBE_TIMEOUT + Duration::from_millis(1500));
            let all = probe_all("10.255.255.1:1", &relays, PROBE_TIMEOUT).await;
            eprintln!("answers {all:?}");
            assert_eq!(all, Answers { main: false, relays: vec![true, true] });
            // Almaty worked last time: Almaty
            assert_eq!(pick("10.255.255.1:1", &relays, ALMATY, PROBE_TIMEOUT).await.address, ALMATY);
        });
    }
}
