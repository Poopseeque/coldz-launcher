//! Reserve route for the FiveM sign-in (0.4.0, Alexander 18.11.2026: «вход в FiveM из России без VPN» is the first task).
//!
//! What breaks. Players in Russia could start FiveM only this way: turn a VPN on, let FiveM sign in, turn the VPN off,
//! connect with F8. Measured 18.11.2026: the Cfx.re services the launcher already checks (netcheck.rs) answer from
//! Russian networks; what the home providers cut is the **Rockstar sign-in** FiveM does at every start
//! (`scui.rockstargames.com` is the sign-in page FiveM shows, `rgl-prod.ros.rockstargames.com` and
//! `prod.ros.rockstargames.com` are the Rockstar services behind it, `signin.rockstargames.com`,
//! `socialclub.rockstargames.com`). These hosts have been on the Russian DPI-bypass lists since January 2026
//! (zapret, «Обход для Rockstar Games Launcher», reports from MTS and others: «works only with a VPN»); the error
//! players see is «Failed to connect to authentication services». Data-centre nodes in Moscow reach the same hosts,
//! so it is the home providers' filtering, not Rockstar.
//!
//! What the launcher does (only when the player turns it on, the Windows prompt confirms it):
//! 1. netcheck.rs asks the sign-in hosts straight (`check`): they answer: nothing to do.
//! 2. They do not, and a COLDZ relay answers through the tunnel (tunnel.rs): the window offers «Включить резервный
//!    маршрут входа». On the click the launcher
//!    * listens on 127.0.0.1:443 (or 127.88.0.1:443) and forwards every connection, byte for byte, to the relay inside
//!      one encrypted connection whose certificate is pinned (news.json `signin.tunnels[].sha256`); the relay sends
//!      it on to the real host named in the player's own TLS handshake (nginx `ssl_preread`, only the hosts below);
//!      the encryption between FiveM and Rockstar stays end to end, the launcher and the relay never see inside;
//!    * writes the sign-in hosts into `C:\Windows\System32\drivers\etc\hosts` pointing at that local address, between
//!      the two marker lines below (hosts.rs: one Windows administrator prompt), so FiveM's own requests go to the
//!      launcher first;
//!    * removes the lines when the player turns the route off or closes the launcher (one more prompt), and at the
//!      next start repairs or removes a block left behind by a crash.
//! 3. The hosts and the relays come from news.json (`signin`), with built-in defaults in launcher.config.json: a
//!    new host or relay needs no new launcher build.
//!
//! Texts for the player are neutral («резервный маршрут входа», ui/i18n.js): nothing about ways around blocks
//! (docs/ПРАВИЛА.md). The relay side: `relay/tunnel/README.md`.

use serde::{Deserialize, Serialize};

/// First and last line of the launcher's block in the hosts file. Everything between them is ours; the launcher
/// never touches other lines.
pub const BEGIN: &str = "# COLDZ Launcher: begin (резервный маршрут входа FiveM; лаунчер убирает эти строки сам)";
pub const END: &str = "# COLDZ Launcher: end";

pub const MAX_HOSTS: usize = 40;
pub const MAX_TUNNELS: usize = 8;
pub const MAX_CHECKS: usize = 4;

/// One group of hosts that fail together (the Rockstar sign-in; the Cfx.re services as a second group when they
/// turn out to be cut somewhere): `check` — addresses the launcher asks straight and through the tunnel (any HTTP
/// answer counts), `hosts` — the names the hosts file points at the launcher while the route is on.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: String,
    #[serde(default)]
    pub check: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// One relay entrance: `host:port` of the TLS tunnel and the SHA-256 of its certificate (DER, 64 hex digits).
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tunnel {
    pub address: String,
    pub sha256: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Setup {
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub tunnels: Vec<Tunnel>,
}

/// A DNS name the hosts file and the relay accept: lower-case letters, digits, dots and hyphens, at least one dot.
pub fn valid_host(h: &str) -> bool {
    let h = h.trim();
    h.len() <= 253
        && h.contains('.')
        && h.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// `host:port` of a tunnel: an IPv4 address or a name, port 1..65535.
pub fn valid_tunnel_address(a: &str) -> bool {
    let Some((host, port)) = a.trim().rsplit_once(':') else { return false };
    let port_ok = port.parse::<u16>().map(|p| p != 0).unwrap_or(false);
    port_ok && (host.parse::<std::net::Ipv4Addr>().is_ok() || valid_host(host))
}

pub fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_check(url: &str) -> bool {
    url.starts_with("https://")
        && reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(valid_host)).unwrap_or(false)
}

/// Only sane entries, lower case, without repeats, capped: a typo in news.json must never reach the hosts file.
pub fn clean(raw: &Setup) -> Setup {
    let mut groups: Vec<Group> = Vec::new();
    let mut seen_hosts: Vec<String> = Vec::new();
    for g in &raw.groups {
        let id: String = g.id.trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(24).collect();
        if id.is_empty() || groups.iter().any(|x| x.id == id) {
            continue;
        }
        let check: Vec<String> =
            g.check.iter().map(|c| c.trim().to_string()).filter(|c| valid_check(c)).take(MAX_CHECKS).collect();
        let mut hosts = Vec::new();
        for h in g.hosts.iter().map(|h| h.trim().to_ascii_lowercase()) {
            if seen_hosts.len() >= MAX_HOSTS {
                break;
            }
            if valid_host(&h) && !seen_hosts.contains(&h) {
                seen_hosts.push(h.clone());
                hosts.push(h);
            }
        }
        if check.is_empty() || hosts.is_empty() {
            continue;
        }
        groups.push(Group { id, check, hosts });
    }
    let mut tunnels: Vec<Tunnel> = Vec::new();
    for t in &raw.tunnels {
        let address = t.address.trim().to_ascii_lowercase();
        let sha256 = t.sha256.trim().to_ascii_lowercase();
        if valid_tunnel_address(&address) && valid_sha256(&sha256) && !tunnels.iter().any(|x| x.address == address) {
            tunnels.push(Tunnel { address, sha256 });
        }
        if tunnels.len() >= MAX_TUNNELS {
            break;
        }
    }
    Setup { groups, tunnels }
}

/// Every host of every group, in order.
pub fn all_hosts(s: &Setup) -> Vec<String> {
    s.groups.iter().flat_map(|g| g.hosts.iter().cloned()).collect()
}

/// The hosts of the groups whose straight check failed (`failed` — group ids). No group failed (the player turned
/// the route on by hand, or the check is old): every host, so the route works whatever is cut.
pub fn hosts_to_route(s: &Setup, failed: &[String]) -> Vec<String> {
    let picked: Vec<String> =
        s.groups.iter().filter(|g| failed.contains(&g.id)).flat_map(|g| g.hosts.iter().cloned()).collect();
    if picked.is_empty() {
        all_hosts(s)
    } else {
        picked
    }
}

/// The launcher's block for the hosts file: `<ip> <host>` per line between the markers, Windows line ends.
pub fn block(ip: &str, hosts: &[String]) -> String {
    let mut out = String::new();
    out.push_str(BEGIN);
    out.push_str("\r\n");
    for h in hosts.iter().filter(|h| valid_host(h)) {
        out.push_str(ip);
        out.push(' ');
        out.push_str(h);
        out.push_str("\r\n");
    }
    out.push_str(END);
    out.push_str("\r\n");
    out
}

/// A line the elevated helper agrees to write: one of the markers or `<loopback ipv4> <host>` (hosts.rs reads the
/// block from a file the launcher wrote; it still checks every line, so nothing else can get into the hosts file
/// through it).
pub fn valid_block_line(line: &str) -> bool {
    let line = line.trim_end_matches('\r');
    if line == BEGIN || line == END {
        return true;
    }
    let mut parts = line.split(' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(ip), Some(host), None) => {
            ip.parse::<std::net::Ipv4Addr>().map(|a| a.is_loopback()).unwrap_or(false) && valid_host(host)
        }
        _ => false,
    }
}

/// The hosts file without the launcher's block (and without a half block: BEGIN with no END takes the rest of the
/// file, END with no BEGIN is dropped alone). Other lines stay byte for byte.
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\r', '\n']);
        if inside {
            if bare == END {
                inside = false;
            }
            continue;
        }
        if bare == BEGIN {
            inside = true;
            continue;
        }
        if bare == END {
            continue;
        }
        out.push_str(line);
    }
    out
}

/// The hosts file with the launcher's block replaced (Some) or removed (None).
pub fn with_block(text: &str, block: Option<&str>) -> String {
    let mut out = strip(text);
    if let Some(b) = block {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str("\r\n");
        }
        out.push_str(b);
    }
    out
}

/// The block now in the hosts file: the loopback address it points at and the hosts. None: no block.
pub fn installed(text: &str) -> Option<(String, Vec<String>)> {
    let mut inside = false;
    let mut ip: Option<String> = None;
    let mut hosts = Vec::new();
    for line in text.lines() {
        let bare = line.trim_end_matches('\r');
        if bare == BEGIN {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if bare == END {
            break;
        }
        if let Some((a, h)) = bare.split_once(' ') {
            if valid_block_line(bare) {
                ip.get_or_insert_with(|| a.to_string());
                hosts.push(h.to_string());
            }
        }
    }
    inside.then_some((ip.unwrap_or_default(), hosts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Setup {
        Setup {
            groups: vec![
                Group {
                    id: "rockstar".into(),
                    check: vec!["https://scui.rockstargames.com/".into()],
                    hosts: vec!["scui.rockstargames.com".into(), "Signin.RockstarGames.com ".into()],
                },
                Group {
                    id: "cfx".into(),
                    check: vec!["https://lambda.fivem.net/".into()],
                    hosts: vec!["lambda.fivem.net".into(), "policy-live.fivem.net".into()],
                },
            ],
            tunnels: vec![Tunnel { address: "177.3.198.34:8443".into(), sha256: "ab".repeat(32) }],
        }
    }

    #[test]
    fn hosts_and_addresses() {
        assert!(valid_host("scui.rockstargames.com"));
        assert!(valid_host("rsg.sc"));
        assert!(!valid_host("localhost"));
        assert!(!valid_host("Scui.rockstargames.com"));
        assert!(!valid_host("-a.b"));
        assert!(!valid_host("a..b"));
        assert!(!valid_host("a.b/c"));
        assert!(valid_tunnel_address("177.3.198.34:8443"));
        assert!(valid_tunnel_address("relay.coldz.vip:443"));
        assert!(!valid_tunnel_address("177.3.198.34"));
        assert!(!valid_tunnel_address("177.3.198.34:0"));
        assert!(!valid_tunnel_address("http://x:1"));
        assert!(valid_sha256(&"ab".repeat(32)));
        assert!(!valid_sha256("ab"));
    }

    #[test]
    fn clean_drops_junk_and_lowercases() {
        let mut raw = setup();
        raw.groups.push(Group { id: "bad".into(), check: vec!["http://x.y/".into()], hosts: vec!["x.y".into()] });
        raw.groups.push(Group {
            id: "dup".into(),
            check: vec!["https://a.b/".into()],
            hosts: vec!["scui.rockstargames.com".into()],
        });
        raw.tunnels.push(Tunnel { address: "nope".into(), sha256: "ab".repeat(32) });
        raw.tunnels.push(Tunnel { address: "1.2.3.4:443".into(), sha256: "zz".into() });
        let c = clean(&raw);
        assert_eq!(c.groups.len(), 2);
        assert_eq!(c.groups[0].hosts, ["scui.rockstargames.com", "signin.rockstargames.com"]);
        assert_eq!(c.tunnels.len(), 1);
        assert_eq!(all_hosts(&c).len(), 4);
        assert_eq!(hosts_to_route(&c, &["cfx".to_string()]), ["lambda.fivem.net", "policy-live.fivem.net"]);
        // nothing failed: everything, so a route turned on by hand covers whatever is cut
        assert_eq!(hosts_to_route(&c, &[]).len(), 4);
        assert!(clean(&Setup::default()).groups.is_empty());
    }

    #[test]
    fn block_round_trip() {
        let c = clean(&setup());
        let b = block("127.0.0.1", &all_hosts(&c));
        assert!(b.starts_with(BEGIN));
        assert!(b.ends_with(&format!("{END}\r\n")));
        assert!(b.lines().all(valid_block_line));
        assert_eq!(installed(&b), Some(("127.0.0.1".to_string(), all_hosts(&c))));

        let original = "# Copyright (c) 1993-2009 Microsoft Corp.\r\n#\r\n127.0.0.1 localhost\r\n";
        let with = with_block(original, Some(&b));
        assert!(with.starts_with(original));
        assert_eq!(strip(&with), original);
        assert_eq!(with_block(&with, None), original);
        // replaced, not doubled
        let again = with_block(&with, Some(&block("127.88.0.1", &["rsg.sc".to_string()])));
        assert_eq!(again.matches(BEGIN).count(), 1);
        assert_eq!(installed(&again), Some(("127.88.0.1".to_string(), vec!["rsg.sc".to_string()])));
        // a file without a final line end gets one before the block
        assert!(with_block("127.0.0.1 localhost", Some(&b)).contains("localhost\r\n# COLDZ"));
        assert_eq!(installed(original), None);
    }

    #[test]
    fn strip_handles_broken_blocks() {
        let half = format!("a\r\n{BEGIN}\r\n127.0.0.1 x.y\r\n");
        assert_eq!(strip(&half), "a\r\n");
        let tail_only = format!("a\r\n{END}\r\nb\r\n");
        assert_eq!(strip(&tail_only), "a\r\nb\r\n");
        assert_eq!(installed(&half), Some(("127.0.0.1".to_string(), vec!["x.y".to_string()])));
    }

    #[test]
    fn block_lines() {
        assert!(valid_block_line("127.0.0.1 scui.rockstargames.com"));
        assert!(valid_block_line("127.88.0.1 rsg.sc\r"));
        assert!(!valid_block_line("8.8.8.8 scui.rockstargames.com"));
        assert!(!valid_block_line("127.0.0.1 scui.rockstargames.com extra"));
        assert!(!valid_block_line("127.0.0.1\tscui.rockstargames.com"));
        assert!(!valid_block_line("# something else"));
    }
}
