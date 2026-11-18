//! The tunnel to a COLDZ relay for the FiveM sign-in (0.4.0, signin.rs for the why).
//!
//! The launcher listens on a loopback address, port 443. The hosts file sends FiveM's sign-in hosts there
//! (hosts.rs). Every connection is forwarded, byte for byte, inside one TLS connection to the relay
//! (`signin.tunnels[]` in news.json): the relay's certificate is self-signed and **pinned** by its SHA-256, nothing
//! else is trusted, and the connection carries no name of the destination on the outside (the relay is addressed
//! by IP, so there is no SNI). Inside, the first bytes are FiveM's own TLS handshake with the real host name; the
//! relay reads that name (nginx `ssl_preread`), checks it against its own short list (Rockstar and Cfx.re hosts
//! only) and connects to the real host on port 443. The encryption between FiveM and Rockstar stays end to end.
//!
//! The launcher only forwards connections whose handshake names one of the hosts it put into the hosts file
//! (`sni_of`): anything else is dropped at once, so the listener is no use to other programs.

use crate::signin::Tunnel;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

/// Loopback addresses the launcher tries for the listener, in order (127.0.0.1:443 can be taken by another program).
pub const LOCAL: [Ipv4Addr; 2] = [Ipv4Addr::new(127, 0, 0, 1), Ipv4Addr::new(127, 88, 0, 1)];
pub const PORT: u16 = 443;
/// One tunnel connection: name lookup, TCP and the TLS handshake with the relay.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
/// How long a new local connection may take to send its handshake before it is dropped.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// The whole probe of one tunnel («Проверить связь»).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// SHA-256 of a certificate (DER), as the relay's `openssl x509 -fingerprint -sha256` prints it without colons.
pub fn fingerprint(der: &[u8]) -> [u8; 32] {
    let d = ring::digest::digest(&ring::digest::SHA256, der);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

pub fn parse_sha256(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Trusts exactly one certificate: the relay's, by its SHA-256. Names and dates are not looked at (the relay is
/// addressed by IP and its certificate is self-signed on purpose: no CA has to be installed on the player's PC).
#[derive(Debug)]
struct Pinned([u8; 32]);

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity.as_ref()) == self.0 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("relay certificate is not the pinned one".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider().signature_verification_algorithms.supported_schemes()
    }
}

/// Trusts anything: only for the probe's inner handshake with the real host (we ask whether the path works, the
/// answer itself is thrown away; FiveM's own connections are not touched by this).
#[derive(Debug)]
struct AnyCert;

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider().signature_verification_algorithms.supported_schemes()
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    rustls::crypto::CryptoProvider::get_default().cloned().unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()))
}

fn client_config(verifier: Arc<dyn ServerCertVerifier>) -> Arc<ClientConfig> {
    let cfg = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("tls versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Arc::new(cfg)
}

/// The relay as a TLS server name: an IP address (no SNI goes out) or a name.
fn server_name(host: &str) -> Option<ServerName<'static>> {
    ServerName::try_from(host.to_string()).ok()
}

/// One encrypted connection to the relay, its certificate checked against the pin. Err: no answer in
/// `CONNECT_TIMEOUT`, or another certificate.
pub async fn open(tunnel: &Tunnel) -> std::io::Result<TlsStream<TcpStream>> {
    let pin = parse_sha256(&tunnel.sha256).ok_or_else(|| bad("pin"))?;
    let (host, port) = tunnel.address.rsplit_once(':').ok_or_else(|| bad("address"))?;
    let port: u16 = port.parse().map_err(|_| bad("port"))?;
    let name = server_name(host).ok_or_else(|| bad("name"))?;
    let connect = async {
        let tcp = TcpStream::connect((host, port)).await?;
        let _ = tcp.set_nodelay(true);
        let connector = TlsConnector::from(client_config(Arc::new(Pinned(pin))));
        connector.connect(name, tcp).await
    };
    tokio::time::timeout(CONNECT_TIMEOUT, connect).await.map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?
}

fn bad(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, what.to_string())
}

/// The host name in a TLS ClientHello (the `server_name` extension), lower case. None: not a ClientHello, or no name.
pub fn sni_of(bytes: &[u8]) -> Option<String> {
    // record: type 0x16, version, length; handshake: type 0x01, length (3), version, random (32)
    if bytes.len() < 43 || bytes[0] != 0x16 || bytes[5] != 0x01 {
        return None;
    }
    let mut i = 43;
    let take = |i: &mut usize, n: usize| -> Option<&[u8]> {
        let s = bytes.get(*i..*i + n)?;
        *i += n;
        Some(s)
    };
    let sid_len = take(&mut i, 1)?[0] as usize;
    take(&mut i, sid_len)?;
    let cs_len = u16::from_be_bytes(take(&mut i, 2)?.try_into().ok()?) as usize;
    take(&mut i, cs_len)?;
    let cm_len = take(&mut i, 1)?[0] as usize;
    take(&mut i, cm_len)?;
    let ext_len = u16::from_be_bytes(take(&mut i, 2)?.try_into().ok()?) as usize;
    let end = i.checked_add(ext_len)?.min(bytes.len());
    while i + 4 <= end {
        let typ = u16::from_be_bytes([bytes[i], bytes[i + 1]]);
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        i += 4;
        if typ == 0 {
            // list length (2), name type (1) = host_name, name length (2), name
            let data = bytes.get(i..i + len)?;
            if data.len() < 5 || data[2] != 0 {
                return None;
            }
            let n = u16::from_be_bytes([data[3], data[4]]) as usize;
            let name = data.get(5..5 + n)?;
            return std::str::from_utf8(name).ok().map(|s| s.trim_end_matches('.').to_ascii_lowercase());
        }
        i += len;
    }
    None
}

/// The listener on port 443 of the first free loopback address. Err: both are taken.
pub fn bind_local() -> std::io::Result<(std::net::TcpListener, IpAddr)> {
    let mut last = std::io::Error::from(std::io::ErrorKind::AddrInUse);
    for ip in LOCAL {
        match std::net::TcpListener::bind(SocketAddr::from((ip, PORT))) {
            Ok(l) => {
                l.set_nonblocking(true)?;
                return Ok((l, IpAddr::V4(ip)));
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// What the listener forwards: the relay and the hosts it may be asked for.
#[derive(Debug)]
pub struct Forward {
    pub tunnel: Tunnel,
    pub hosts: Vec<String>,
    /// 0.5.0: what went through (net-check.log).
    pub stats: std::sync::Mutex<Stats>,
}

impl Forward {
    pub fn new(tunnel: Tunnel, hosts: Vec<String>) -> Self {
        Forward { tunnel, hosts, stats: Default::default() }
    }

    pub fn stats(&self) -> Stats {
        self.stats.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn note(&self, f: impl FnOnce(&mut Stats)) {
        f(&mut self.stats.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

/// Forwards every connection on `listener` to the relay (see the module doc). Runs until the task is aborted.
pub async fn serve(listener: TcpListener, forward: Arc<Forward>) {
    loop {
        let Ok((conn, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let f = forward.clone();
        tokio::spawn(async move {
            let _ = pipe(conn, &f).await;
        });
    }
}

async fn pipe(mut local: TcpStream, f: &Forward) -> std::io::Result<()> {
    let _ = local.set_nodelay(true);
    // the first bytes without taking them: FiveM's handshake names the host; a name we did not redirect is dropped
    let mut head = [0u8; 2048];
    let n = tokio::time::timeout(HELLO_TIMEOUT, local.peek(&mut head))
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    let Some(name) = sni_of(&head[..n]) else {
        f.note(|s| s.refuse("no name"));
        return Err(bad("no name"));
    };
    if !f.hosts.iter().any(|h| *h == name) {
        f.note(|s| s.refuse("not ours"));
        return Err(bad("not ours"));
    }
    let started = std::time::Instant::now();
    let mut remote = match open(&f.tunnel).await {
        Ok(r) => r,
        Err(e) => {
            f.note(|s| s.refuse("relay"));
            return Err(e);
        }
    };
    f.note(|s| s.connections += 1);
    let (up, down) = tokio::io::copy_bidirectional(&mut local, &mut remote).await.unwrap_or((0, 0));
    let _ = remote.shutdown().await;
    let ms = started.elapsed().as_millis() as u64;
    f.note(|s| {
        s.up += up;
        s.down += down;
        s.last = Some((name.clone(), up, down, ms));
    });
    Ok(())
}

/// Where the volume check reads from through the tunnel: FiveM's own update storage (allowed on the relay). Since
/// November 2026 some Russian providers «freeze» connections to foreign networks after a few dozen packets, so a tunnel
/// that answers one small page can still be useless: the check reads the current build manifest (~99 KB) through it.
pub const VOLUME_HOST: &str = "cfx-content.akamaized.net";
const VOLUME_HEADS: &str = "/updates/heads/fivereborn/production";

/// The answer of one tunnel: the HTTP status of `HEAD /` at the sign-in host and how many bytes of the manifest came
/// through within the time (`ok` only when the bytes are past the throttling mark, netcheck `STALL_BYTES`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Answer {
    pub status: u16,
    pub bytes: u64,
}

impl Answer {
    pub fn ok(&self) -> bool {
        self.bytes >= crate::netcheck::STALL_BYTES
    }
}

/// 0.5.0: what one tunnel probe did, for net-check.log: how far it went and why it stopped. `bytes` counts the body
/// bytes of the manifest that came through, also when the time ran out in the middle (a «frozen» tunnel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trace {
    /// HTTP status of `HEAD /` at the sign-in host, None: it did not answer.
    pub status: Option<u16>,
    pub bytes: u64,
    pub ms: u32,
    /// Why the probe stopped (None: the manifest came through past the throttling mark): "relay" (no TLS connection
    /// to the relay, or another certificate), "host_tls" (the real host did not finish its handshake through the
    /// relay), "write" / "read" (the connection broke), "http" (not an HTTP answer), "manifest" (no build manifest in
    /// the update heads), "timeout" (the time ran out, see `bytes`), "slow" (the manifest ended below the mark).
    pub stop: Option<&'static str>,
}

impl Trace {
    /// Ok once the sign-in host answered through the tunnel (`Answer::ok` then says whether the volume came too).
    pub fn answer(&self) -> Result<Answer, &'static str> {
        match self.status {
            Some(status) if matches!(self.stop, None | Some("slow") | Some("timeout")) => {
                Ok(Answer { status, bytes: self.bytes })
            }
            _ => Err(self.stop.unwrap_or("http")),
        }
    }
}

/// One HTTP/1.1 request through the tunnel with its own inner TLS to `host`: returns the status, the headers (lower
/// case names) and how many body bytes came before `want` was reached or the stream ended. Body bytes are also added
/// to `progress` as they come, so a caller that gives up on time still knows how far it got.
async fn request(
    tunnel: &Tunnel,
    host: &str,
    path: &str,
    head_only: bool,
    want: u64,
    progress: &std::sync::atomic::AtomicU64,
) -> Result<(u16, Vec<(String, String)>, u64), &'static str> {
    use std::sync::atomic::Ordering;
    let outer = open(tunnel).await.map_err(|_| "relay")?;
    let name = server_name(host).ok_or("host")?;
    let connector = TlsConnector::from(client_config(Arc::new(AnyCert)));
    let mut inner = connector.connect(name, outer).await.map_err(|_| "host_tls")?;
    let req = format!(
        "{} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: COLDZ-Launcher/{}\r\nConnection: close\r\n\r\n",
        if head_only { "HEAD" } else { "GET" },
        env!("CARGO_PKG_VERSION")
    );
    inner.write_all(req.as_bytes()).await.map_err(|_| "write")?;
    // the head: up to the empty line
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 64 * 1024 {
            return Err("http");
        }
        match inner.read(&mut chunk).await {
            Ok(0) => return Err("http"),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return Err("read"),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.strip_prefix("HTTP/1."))
        .and_then(|s| s.get(2..5))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or("http")?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let mut bytes = (buf.len() - head_end) as u64;
    if !head_only {
        progress.fetch_add(bytes, Ordering::Relaxed);
        while bytes < want {
            match inner.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    bytes += n as u64;
                    progress.fetch_add(n as u64, Ordering::Relaxed);
                }
                Err(_) => break,
            }
        }
    }
    let _ = inner.shutdown().await;
    Ok((status, headers, bytes))
}

/// Asks `host` through the tunnel (`HEAD /`, any status = the path works) and then reads the FiveM build manifest
/// through it (`VOLUME_HOST`): the answer says whether the tunnel really carries data, not just one small page.
/// 0.5.0: every step is traced (`Trace`), so the log says where a tunnel stopped and how many bytes came before.
pub async fn probe_traced(tunnel: &Tunnel, host: &str, limit: Duration) -> Trace {
    use std::sync::atomic::{AtomicU64, Ordering};
    let started = std::time::Instant::now();
    let progress = AtomicU64::new(0);
    let status = std::sync::Mutex::new(None::<u16>);
    let run = async {
        let (st, _, _) = request(tunnel, host, "/", true, 0, &progress).await?;
        *status.lock().unwrap_or_else(|e| e.into_inner()) = Some(st);
        let path = format!("{VOLUME_HEADS}?time={}", crate::netcheck::unix_now());
        let (_, headers, _) = request(tunnel, VOLUME_HOST, &path, true, 0, &progress).await?;
        let manifest = headers
            .iter()
            .find(|(k, _)| k == "x-amz-meta-branch-manifest")
            .and_then(|(_, v)| crate::netcheck::object_url(v))
            .and_then(|u| u.strip_prefix("https://cfx-content.akamaized.net").map(str::to_string))
            .ok_or("manifest")?;
        let (_, _, bytes) = request(tunnel, VOLUME_HOST, &manifest, false, crate::netcheck::VOLUME_BYTES, &progress).await?;
        Ok::<u64, &'static str>(bytes)
    };
    let result = tokio::time::timeout(limit, run).await;
    let status = *status.lock().unwrap_or_else(|e| e.into_inner());
    let bytes = progress.load(Ordering::Relaxed);
    let ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    let stop = match result {
        Err(_) => Some("timeout"),
        Ok(Err(e)) => Some(e),
        Ok(Ok(b)) if b < crate::netcheck::STALL_BYTES => Some("slow"),
        Ok(Ok(_)) => None,
    };
    Trace { status, bytes, ms, stop }
}

/// `probe_traced` with the usual time (`PROBE_TIMEOUT`). Ok: the host answered through the tunnel (see `Answer::ok`
/// for the volume). Err: the relay does not answer, its certificate is wrong, or the host did not answer in time.
pub async fn probe_full(tunnel: &Tunnel, host: &str) -> Result<Answer, &'static str> {
    probe_traced(tunnel, host, PROBE_TIMEOUT).await.answer()
}

// ---------------------------------------------------------------- what the forwarder did (0.5.0, net-check.log)

/// Counters of the running route, written to net-check.log at the connection check and when the route stops: how
/// many connections went through, how many bytes, and why the others did not.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Stats {
    pub connections: u64,
    pub up: u64,
    pub down: u64,
    /// (reason, count): "no name" / "not ours" (dropped by the launcher), "relay" (the relay did not answer).
    pub refused: Vec<(&'static str, u64)>,
    /// The last connection that ended: host, bytes up, bytes down, how long (ms).
    pub last: Option<(String, u64, u64, u64)>,
}

impl Stats {
    fn refuse(&mut self, why: &'static str) {
        match self.refused.iter_mut().find(|(w, _)| *w == why) {
            Some((_, n)) => *n += 1,
            None => self.refused.push((why, 1)),
        }
    }

    /// One line for the log.
    pub fn line(&self, tunnel: &str) -> String {
        let refused = if self.refused.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = self.refused.iter().map(|(w, n)| format!("{w} {n}")).collect();
            format!("; не пропущено: {}", parts.join(", "))
        };
        let last = self
            .last
            .as_ref()
            .map(|(h, u, d, ms)| format!("; последнее: {h}, {u}/{d} байт за {ms} мс"))
            .unwrap_or_default();
        format!(
            "работа маршрута через {tunnel}: соединений {}, отправлено {} байт, получено {} байт{refused}{last}",
            self.connections, self.up, self.down
        )
    }
}

/// Asks `host` through the tunnel: the HTTP status of `HEAD /` (any status = the path works). Only the test with a
/// real relay uses it now (`real_tunnel`); the launcher asks `probe_traced`.
/// Err: the relay does not answer, its certificate is wrong, or the host did not answer within `PROBE_TIMEOUT`.
#[allow(dead_code)]
pub async fn probe(tunnel: &Tunnel, host: &str) -> Result<u16, &'static str> {
    let run = async {
        let outer = open(tunnel).await.map_err(|_| "relay")?;
        let name = server_name(host).ok_or("host")?;
        let connector = TlsConnector::from(client_config(Arc::new(AnyCert)));
        let mut inner = connector.connect(name, outer).await.map_err(|_| "host_tls")?;
        let req = format!("HEAD / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: COLDZ-Launcher/{}\r\nConnection: close\r\n\r\n", env!("CARGO_PKG_VERSION"));
        inner.write_all(req.as_bytes()).await.map_err(|_| "write")?;
        let mut buf = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        while buf.len() < 256 {
            match inner.read(&mut byte).await {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(byte[0]);
                    if buf.ends_with(b"\r\n") {
                        break;
                    }
                }
                Err(_) => return Err("read"),
            }
        }
        let line = String::from_utf8_lossy(&buf);
        let status = line.strip_prefix("HTTP/1.").and_then(|s| s.get(2..5)).and_then(|s| s.parse::<u16>().ok());
        let _ = inner.shutdown().await;
        status.ok_or("http")
    };
    tokio::time::timeout(PROBE_TIMEOUT, run).await.map_err(|_| "timeout")?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rustls::pki_types::PrivateKeyDer;
    use tokio_rustls::TlsAcceptor;

    pub const CERT: &[u8] = include_bytes!("../tests/fixtures/relay_test.crt.der");
    const KEY: &[u8] = include_bytes!("../tests/fixtures/relay_test.key.der");

    pub fn rt() -> tokio::runtime::Runtime {
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    pub fn hex(d: &[u8; 32]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn acceptor() -> TlsAcceptor {
        let key = PrivateKeyDer::Pkcs8(KEY.to_vec().into());
        let cfg = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(CERT.to_vec())], key)
            .unwrap();
        TlsAcceptor::from(Arc::new(cfg))
    }

    /// A relay for tests: accepts the pinned TLS, then does `inner` with the plain inside stream.
    pub async fn relay<F, Fut>(inner: F) -> (String, tokio::task::JoinHandle<()>)
    where
        F: Fn(tokio_rustls::server::TlsStream<TcpStream>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let acc = acceptor();
        let inner = Arc::new(inner);
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = l.accept().await else { break };
                let acc = acc.clone();
                let inner = inner.clone();
                tokio::spawn(async move {
                    if let Ok(tls) = acc.accept(tcp).await {
                        inner(tls).await;
                    }
                });
            }
        });
        (addr, task)
    }

    /// A ClientHello (TLS 1.2 shape) naming `host`, followed by nothing else.
    pub fn client_hello(host: &str) -> Vec<u8> {
        let name = host.as_bytes();
        let mut sni = Vec::new();
        sni.extend_from_slice(&((name.len() + 3) as u16).to_be_bytes()); // list length
        sni.push(0); // host_name
        sni.extend_from_slice(&(name.len() as u16).to_be_bytes());
        sni.extend_from_slice(name);
        let mut ext = Vec::new();
        ext.extend_from_slice(&0u16.to_be_bytes()); // server_name
        ext.extend_from_slice(&(sni.len() as u16).to_be_bytes());
        ext.extend_from_slice(&sni);
        let mut body = vec![0x03, 0x03];
        body.extend_from_slice(&[7u8; 32]); // random
        body.push(0); // session id
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&[0x13, 0x01]); // one cipher suite
        body.push(1);
        body.push(0); // compression null
        body.extend_from_slice(&(ext.len() as u16).to_be_bytes());
        body.extend_from_slice(&ext);
        let mut hs = vec![0x01];
        hs.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        hs.extend_from_slice(&body);
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn fingerprint_and_hex() {
        let fp = fingerprint(CERT);
        assert_eq!(parse_sha256(&hex(&fp)), Some(fp));
        assert_eq!(parse_sha256(&hex(&fp).to_uppercase()), Some(fp));
        assert_eq!(parse_sha256("12"), None);
        assert_eq!(parse_sha256(&"zz".repeat(32)), None);
    }

    #[test]
    fn reads_sni() {
        assert_eq!(sni_of(&client_hello("scui.rockstargames.com")), Some("scui.rockstargames.com".into()));
        assert_eq!(sni_of(&client_hello("SCUI.RockstarGames.com.")), Some("scui.rockstargames.com".into()));
        assert_eq!(sni_of(b"GET / HTTP/1.1\r\n"), None);
        assert_eq!(sni_of(&client_hello("x.y")[..20]), None);
        assert_eq!(sni_of(&[]), None);
    }

    #[test]
    fn forwards_only_named_hosts_to_the_pinned_relay() {
        rt().block_on(async {
            // the relay echoes whatever comes inside, in upper case, so the test can see the bytes went through
            let (addr, relay_task) = relay(|mut tls| async move {
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = tls.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let up: Vec<u8> = buf[..n].iter().map(|b| b.to_ascii_uppercase()).collect();
                    if tls.write_all(&up).await.is_err() {
                        break;
                    }
                }
            })
            .await;
            let tunnel = Tunnel { address: addr.clone(), sha256: hex(&fingerprint(CERT)) };
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let local = listener.local_addr().unwrap();
            let fwd = Arc::new(Forward::new(tunnel.clone(), vec!["scui.rockstargames.com".into()]));
            let serve_task = tokio::spawn(serve(listener, fwd));

            // our host: the bytes come back through the relay
            let mut c = TcpStream::connect(local).await.unwrap();
            let hello = client_hello("scui.rockstargames.com");
            c.write_all(&hello).await.unwrap();
            c.write_all(b"ping").await.unwrap();
            let mut got = vec![0u8; hello.len() + 4];
            tokio::time::timeout(Duration::from_secs(5), c.read_exact(&mut got)).await.unwrap().unwrap();
            assert_eq!(&got[hello.len()..], b"PING");
            drop(c);

            // another host: dropped without an answer
            let mut other = TcpStream::connect(local).await.unwrap();
            other.write_all(&client_hello("example.com")).await.unwrap();
            let mut one = [0u8; 1];
            let n = tokio::time::timeout(Duration::from_secs(5), other.read(&mut one)).await.unwrap().unwrap_or(0);
            assert_eq!(n, 0);

            // a wrong pin: the launcher refuses the relay
            let wrong = Tunnel { address: addr, sha256: "ab".repeat(32) };
            assert!(open(&wrong).await.is_err());
            assert!(open(&tunnel).await.is_ok());

            serve_task.abort();
            relay_task.abort();
        });
    }

    /// A real relay: `COLDZ_TUNNEL=177.3.198.34:8443 COLDZ_TUNNEL_SHA256=<64 hex> cargo test real_tunnel -- --ignored --nocapture`
    /// (relay/tunnel/README.md). Asks scui.rockstargames.com through it: Rockstar answers 403 to a bare HEAD.
    #[test]
    #[ignore]
    fn real_tunnel() {
        let (Ok(address), Ok(sha256)) = (std::env::var("COLDZ_TUNNEL"), std::env::var("COLDZ_TUNNEL_SHA256")) else {
            eprintln!("COLDZ_TUNNEL and COLDZ_TUNNEL_SHA256 are not set");
            return;
        };
        rt().block_on(async {
            let t = Tunnel { address, sha256 };
            for host in ["scui.rockstargames.com", "rgl-prod.ros.rockstargames.com", "lambda.fivem.net"] {
                let r = probe(&t, host).await;
                eprintln!("{host} through {}: {r:?}", t.address);
                assert!(r.is_ok(), "{host}: {r:?}");
            }
            let a = probe_full(&t, "scui.rockstargames.com").await;
            eprintln!("volume through {}: {a:?}", t.address);
            assert!(matches!(a, Ok(x) if x.ok()), "{a:?}");
        });
    }

    /// A "real host" behind the mock relay that answers like Cfx.re: the heads with a manifest header, the manifest
    /// of `size` bytes, 403 to anything else.
    async fn cfx_like(acc: tokio_rustls::TlsAcceptor, outer: tokio_rustls::server::TlsStream<TcpStream>, size: usize) {
        let Ok(mut inner) = acc.accept(outer).await else { return };
        let mut buf = vec![0u8; 2048];
        let n = inner.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        let manifest = "ab".repeat(20);
        if req.contains("/updates/heads/") {
            let _ = inner
                .write_all(format!("HTTP/1.1 200 OK\r\nx-amz-meta-branch-manifest: {manifest}\r\nContent-Length: 0\r\n\r\n").as_bytes())
                .await;
        } else if req.contains(&format!("/updates/ab/ab/{manifest}")) {
            let body = vec![b'x'; size];
            let _ = inner.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n\r\n").as_bytes()).await;
            let _ = inner.write_all(&body).await;
        } else {
            let _ = inner.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await;
        }
        let _ = inner.shutdown().await;
    }

    #[test]
    fn probe_full_needs_volume_not_just_a_status() {
        rt().block_on(async {
            // a tunnel that carries data: the manifest comes through whole
            let acc = acceptor();
            let (addr, t1) = relay(move |outer| cfx_like(acc.clone(), outer, 70 * 1024)).await;
            let tunnel = Tunnel { address: addr, sha256: hex(&fingerprint(CERT)) };
            let a = probe_full(&tunnel, "scui.rockstargames.com").await.unwrap();
            assert_eq!(a.status, 403);
            assert!(a.bytes >= crate::netcheck::VOLUME_BYTES, "{a:?}");
            assert!(a.ok());
            t1.abort();

            // a «frozen» tunnel: the status answers, the manifest stops after 10 KB: not ok
            let acc = acceptor();
            let (addr, t2) = relay(move |outer| cfx_like(acc.clone(), outer, 10 * 1024)).await;
            let tunnel = Tunnel { address: addr, sha256: hex(&fingerprint(CERT)) };
            let a = probe_full(&tunnel, "scui.rockstargames.com").await.unwrap();
            assert_eq!(a.status, 403);
            assert!(!a.ok(), "{a:?}");
            t2.abort();
        });
    }

    /// 0.5.0: a relay that «freezes» in the middle of the manifest: the trace keeps how far it got and says
    /// «timeout»; a relay that does not answer at all says «relay» (net-check.log, relay number and stop reason).
    #[test]
    fn trace_keeps_the_bytes_of_a_frozen_tunnel() {
        rt().block_on(async {
            let acc = acceptor();
            let (addr, t) = relay(move |outer| {
                let acc = acc.clone();
                async move {
                    let Ok(mut inner) = acc.accept(outer).await else { return };
                    let mut buf = vec![0u8; 2048];
                    let n = inner.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let manifest = "ab".repeat(20);
                    if req.contains("/updates/heads/") {
                        let head = format!("HTTP/1.1 200 OK\r\nx-amz-meta-branch-manifest: {manifest}\r\nContent-Length: 0\r\n\r\n");
                        let _ = inner.write_all(head.as_bytes()).await;
                    } else if req.contains("/updates/ab/ab/") {
                        let _ = inner.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 99000\r\n\r\n").await;
                        let _ = inner.write_all(&vec![b'x'; 20 * 1024]).await;
                        let _ = inner.flush().await;
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    } else {
                        let _ = inner.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await;
                    }
                    let _ = inner.shutdown().await;
                }
            })
            .await;
            let tunnel = Tunnel { address: addr, sha256: hex(&fingerprint(CERT)) };
            let tr = probe_traced(&tunnel, "scui.rockstargames.com", Duration::from_millis(2500)).await;
            assert_eq!((tr.status, tr.stop), (Some(403), Some("timeout")), "{tr:?}");
            assert_eq!(tr.bytes, 20 * 1024, "{tr:?}");
            assert!(tr.ms >= 2400, "{tr:?}");
            // the host answered: Ok, but not «ok» (frozen)
            assert!(matches!(tr.answer(), Ok(a) if !a.ok()), "{tr:?}");
            t.abort();

            let dead = Tunnel { address: "127.0.0.1:1".into(), sha256: "ab".repeat(32) };
            let tr = probe_traced(&dead, "scui.rockstargames.com", Duration::from_secs(3)).await;
            assert_eq!((tr.status, tr.stop, tr.bytes), (None, Some("relay"), 0));
            assert_eq!(tr.answer(), Err("relay"));
        });
    }

    #[test]
    fn probe_reads_the_status_through_two_layers() {
        rt().block_on(async {
            let acc = acceptor();
            let (addr, relay_task) = relay(move |outer| {
                let acc = acc.clone();
                async move {
                    // the "real host": the same test certificate, answers 403 to anything
                    if let Ok(mut inner) = acc.accept(outer).await {
                        let mut buf = vec![0u8; 1024];
                        let _ = inner.read(&mut buf).await;
                        let _ = inner.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await;
                        let _ = inner.shutdown().await;
                    }
                }
            })
            .await;
            let tunnel = Tunnel { address: addr.clone(), sha256: hex(&fingerprint(CERT)) };
            assert_eq!(probe(&tunnel, "scui.rockstargames.com").await, Ok(403));
            let wrong = Tunnel { address: addr, sha256: "ab".repeat(32) };
            assert_eq!(probe(&wrong, "scui.rockstargames.com").await, Err("relay"));
            let dead = Tunnel { address: "127.0.0.1:1".into(), sha256: "ab".repeat(32) };
            assert_eq!(probe(&dead, "scui.rockstargames.com").await, Err("relay"));
            relay_task.abort();
        });
    }
}
