//! Nickname on the COLDZ server (13.11.2026, Alexander: "set a nickname in the launcher that will then be the
//! nickname on the server"; "it can be changed any time, any nick — we are not an RP server").
//!
//! The launcher cannot reliably change the FiveM nickname itself: the "Nickname" field of FiveM's own menu lives in
//! FiveM's browser storage and always wins over `cl_nickname` in `%APPDATA%\CitizenFX\fivem.cfg`, and the query part
//! of `fivem://connect/<address>?...` never reaches the server (FiveM reads only `streamerMode` from it). So the
//! launcher tells OUR server directly, right before starting FiveM:
//!
//!   POST http://<server address>/z_menu/nick   {"nick": "...", "launcher": "<id of this install>"}
//!
//! The server (winter-server, resources/[z]/z_menu/server/nick.lua) checks the same rules plus staff words
//! ("admin", "создатель"...), answers at once, keeps the nick for 10 minutes for the player's IP and gives it to the
//! character when the player from that IP enters. An empty nick means "use my FiveM name".

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Same as the server (z_menu config `nick.maxBytes`, `nickMaxBytes`): 40 bytes of UTF-8 —
/// 20 Cyrillic or 40 Latin letters.
pub const MAX_BYTES: usize = 40;

/// Path of the server endpoint (resource z_menu, `SetHttpHandler`, path `/nick`).
pub const ENDPOINT_PATH: &str = "/z_menu/nick";

/// Latin and Cyrillic letters, digits, space, '-' and '_' (z_menu/shared/nick.lua `allowed`).
fn allowed(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c == ' '
        || c == '-'
        || c == '_'
        || ('\u{410}'..='\u{44F}').contains(&c)
        || c == 'Ё'
        || c == 'ё'
}

fn letter_or_digit(c: char) -> bool {
    allowed(c) && !matches!(c, ' ' | '-' | '_')
}

/// Runs of spaces become one space, spaces at the ends are cut.
pub fn normalize(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The nick as the server will see it, or why it is not allowed: "chars" (other symbols, or no letter/digit at all)
/// or "long" (more than 40 bytes). An empty result is fine: "use my FiveM name". Staff words are checked by the server.
pub fn check(raw: &str) -> Result<String, &'static str> {
    let nick = normalize(raw);
    if nick.is_empty() {
        return Ok(nick);
    }
    if !nick.chars().all(allowed) || !nick.chars().any(letter_or_digit) {
        return Err("chars");
    }
    if nick.len() > MAX_BYTES {
        return Err("long");
    }
    Ok(nick)
}

/// `http://host:port/z_menu/nick`
pub fn endpoint(host: &str, port: u16) -> String {
    format!("http://{host}:{port}{ENDPOINT_PATH}")
}

// ---------------------------------------------------------------- what the launcher remembers

/// `<app config dir>\settings.json`: the last nick and a random id of this install (the server uses it only to tell
/// two launchers behind one IP apart).
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub nick: String,
    pub launcher_id: String,
}

pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(s).unwrap_or_default())
}

/// 32 hex characters. Not a secret: only tells launchers apart, so std's randomly keyed hasher is enough.
pub fn new_launcher_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut out = String::with_capacity(32);
    for part in 0..2u8 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u32(std::process::id());
        h.write_u8(part);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

// ---------------------------------------------------------------- the server's answer

/// What the UI gets after "Play".
#[derive(Serialize, Debug, PartialEq, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NickResult {
    /// "ok" — the server took the nick; "rejected" — it did not (see `reason`), FiveM is not started;
    /// "unreachable" — no answer (old server, no internet): FiveM starts anyway with the nick the player had.
    pub status: &'static str,
    /// The nick as sent ("" = FiveM name).
    pub nick: String,
    /// "chars" | "long" | "reserved" | "often" | "bad" | "empty"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl NickResult {
    pub fn unreachable(nick: &str) -> Self {
        NickResult { status: "unreachable", nick: nick.to_string(), reason: None }
    }
}

const REASONS: [&str; 6] = ["chars", "long", "reserved", "often", "bad", "empty"];

/// HTTP status + JSON body of the server -> result. Unknown reasons become "bad", any other status — "unreachable"
/// (404: the server has no nickname support yet).
pub fn interpret(status: u16, body: Option<&serde_json::Value>, sent: &str) -> NickResult {
    let rejected = |reason: &str| NickResult {
        status: "rejected",
        nick: sent.to_string(),
        reason: Some(if REASONS.contains(&reason) { reason } else { "bad" }.to_string()),
    };
    match (status, body) {
        (429, _) => rejected("often"),
        (200, Some(b)) if b.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
            NickResult { status: "ok", nick: sent.to_string(), reason: None }
        }
        (200, Some(b)) => rejected(b.get("reason").and_then(|r| r.as_str()).unwrap_or("bad")),
        _ => NickResult::unreachable(sent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn same_rules_as_the_server() {
        for ok in ["Снег", "John_Doe-1", "Вася Pupkin", "ЁжЁ", "X", "7", "Observer"] {
            assert_eq!(check(ok), Ok(ok.to_string()), "{ok}");
        }
        assert_eq!(check("  Вася   Пупкин  "), Ok("Вася Пупкин".into()));
        assert_eq!(check("a\tb"), Ok("a b".into()));
        assert_eq!(check("   "), Ok(String::new()));
        for bad in ["★Ivan★", "中文", "---___", "a.b", "<b>", "Ник!"] {
            assert_eq!(check(bad), Err("chars"), "{bad}");
        }
        assert_eq!(check(&"Ж".repeat(20)), Ok("Ж".repeat(20)));
        assert_eq!(check(&"Ж".repeat(21)), Err("long"));
        assert_eq!(check(&"a".repeat(40)), Ok("a".repeat(40)));
        assert_eq!(check(&"a".repeat(41)), Err("long"));
    }

    #[test]
    fn endpoint_url() {
        assert_eq!(endpoint("92.118.11.28", 28683), "http://92.118.11.28:28683/z_menu/nick");
    }

    #[test]
    fn server_answers() {
        let ok = interpret(200, Some(&json!({ "ok": true, "nick": "Снег" })), "Снег");
        assert_eq!(ok, NickResult { status: "ok", nick: "Снег".into(), reason: None });
        let reserved = interpret(200, Some(&json!({ "ok": false, "reason": "reserved" })), "Админ");
        assert_eq!(reserved.status, "rejected");
        assert_eq!(reserved.reason.as_deref(), Some("reserved"));
        let weird = interpret(200, Some(&json!({ "ok": false, "reason": "<script>" })), "x");
        assert_eq!(weird.reason.as_deref(), Some("bad"));
        assert_eq!(interpret(429, None, "x").reason.as_deref(), Some("often"));
        assert_eq!(interpret(404, Some(&json!({ "ok": false, "reason": "path" })), "x").status, "unreachable");
        assert_eq!(interpret(500, None, "x").status, "unreachable");
        assert_eq!(interpret(200, None, "x").status, "unreachable");
    }

    #[test]
    fn settings_round_trip_and_ids() {
        let dir = std::env::temp_dir().join(format!("coldz-nick-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        assert_eq!(load(&path), Settings::default());
        let s = Settings { nick: "Снег".into(), launcher_id: new_launcher_id() };
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(load(&path), Settings::default());
        let _ = std::fs::remove_dir_all(&dir);

        let (a, b) = (new_launcher_id(), new_launcher_id());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
