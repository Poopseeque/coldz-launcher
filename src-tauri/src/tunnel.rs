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
//!
//! 0.5.7, long connections (a player got through the sign-in and FiveM stopped at its menu): the launcher has no idle
//! limit of its own (the relay's is 15 minutes without a byte, `proxy_timeout`); the connection to the relay keeps
//! itself alive on quiet routers (`KEEPALIVE`); a program that has only finished sending still gets the rest of the
//! answer (`shuttle`: nginx drops both directions as soon as it hears the end of one, so the end is told to it last); the
//! handshake is read until the name is there (`hello_name`: it may come in pieces or be longer than 2 KB); a relay that
//! stops answering is replaced by the next one without touching the hosts file (`Forward::connect`).

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
/// The longest handshake the launcher looks for a name in: one TLS record (16 KB) and its header.
const HELLO_MAX: usize = 16 * 1024 + 5;
/// Quiet time after which the connection to the relay asks whether it is still there, and how often it repeats.
const KEEPALIVE: Duration = Duration::from_secs(30);
const KEEPALIVE_EVERY: Duration = Duration::from_secs(10);
/// One side has finished: how long the other may stay silent before the connection is closed.
const LINGER: Duration = Duration::from_secs(20);
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
        // home routers and mobile networks forget a quiet connection after a few minutes
        let keep = socket2::TcpKeepalive::new().with_time(KEEPALIVE).with_interval(KEEPALIVE_EVERY);
        let _ = socket2::SockRef::from(&tcp).set_tcp_keepalive(&keep);
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
    bind_first(&LOCAL.map(|ip| SocketAddr::from((ip, PORT))))
}

fn bind_first(addresses: &[SocketAddr]) -> std::io::Result<(std::net::TcpListener, IpAddr)> {
    let mut last = std::io::Error::from(std::io::ErrorKind::AddrInUse);
    for a in addresses {
        match std::net::TcpListener::bind(a) {
            Ok(l) => {
                l.set_nonblocking(true)?;
                return Ok((l, a.ip()));
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// What the listener forwards: the relays (the one in use and the others, for when it stops answering) and the hosts
/// it may be asked for.
#[derive(Debug)]
pub struct Forward {
    tunnels: Vec<Tunnel>,
    current: std::sync::atomic::AtomicUsize,
    pub hosts: Vec<String>,
    /// 0.5.0: what went through (net-check.log).
    pub stats: std::sync::Mutex<Stats>,
}

impl Forward {
    /// `tunnels`: the chosen relay first, then the others of news.json.
    pub fn new(tunnels: Vec<Tunnel>, hosts: Vec<String>) -> Self {
        Forward { tunnels, current: Default::default(), hosts, stats: Default::default() }
    }

    fn index(&self) -> usize {
        self.current.load(std::sync::atomic::Ordering::Relaxed).min(self.tunnels.len().saturating_sub(1))
    }

    /// The relay in use.
    pub fn address(&self) -> &str {
        self.tunnels.get(self.index()).map(|t| t.address.as_str()).unwrap_or_default()
    }

    /// The connection check found this relay carrying data while the one in use does not (lib.rs `signin_check`).
    pub fn prefer(&self, address: &str) {
        if let Some(i) = self.tunnels.iter().position(|t| t.address == address) {
            self.current.store(i, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// 0.5.7: a connection to the relay in use, or to the first other one that answers (it is then the one in use).
    async fn connect(&self) -> std::io::Result<TlsStream<TcpStream>> {
        let first = self.index();
        let mut last = bad("no relay");
        for i in (first..self.tunnels.len()).chain(0..first) {
            match open(&self.tunnels[i]).await {
                Ok(s) => {
                    if i != first {
                        self.current.store(i, std::sync::atomic::Ordering::Relaxed);
                        self.note(|s| s.switched += 1);
                    }
                    return Ok(s);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
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

/// The host name in the handshake of a new local connection, without taking the bytes. None: not a handshake, or it has
/// no name.
async fn hello_name(local: &TcpStream) -> Option<String> {
    let mut head = vec![0u8; HELLO_MAX];
    loop {
        let n = local.peek(&mut head).await.ok()?;
        if n == 0 || head[0] != 0x16 {
            return None;
        }
        if let Some(name) = sni_of(&head[..n]) {
            return Some(name);
        }
        // the whole record is here and has no name in it
        if n >= 5 && n >= (5 + u16::from_be_bytes([head[3], head[4]]) as usize).min(head.len()) {
            return None;
        }
        // the rest is still coming (peek does not wait for it)
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

async fn pipe(local: TcpStream, f: &Forward) -> std::io::Result<()> {
    let _ = local.set_nodelay(true);
    // FiveM's handshake names the host; a name we did not redirect is dropped
    let name = tokio::time::timeout(HELLO_TIMEOUT, hello_name(&local))
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?;
    let Some(name) = name else {
        f.note(|s| s.refuse("no name"));
        return Err(bad("no name"));
    };
    if !f.hosts.iter().any(|h| *h == name) {
        f.note(|s| s.refuse("not ours"));
        return Err(bad("not ours"));
    }
    let started = std::time::Instant::now();
    let remote = match f.connect().await {
        Ok(r) => r,
        Err(e) => {
            f.note(|s| s.refuse("relay"));
            return Err(e);
        }
    };
    f.note(|s| s.connections += 1);
    let done = shuttle(local, remote, LINGER).await;
    let ms = started.elapsed().as_millis() as u64;
    f.note(|s| {
        s.up += done.up;
        s.down += done.down;
        s.longest = s.longest.max(ms);
        s.ended(done.first);
        s.last = Some((name.clone(), done.up, done.down, ms));
    });
    Ok(())
}

/// How one forwarded connection went.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Done {
    up: u64,
    down: u64,
    /// Who ended it: "program" (FiveM finished), "server" (the other side finished), "break" (an error: the network, the
    /// relay, a reset).
    first: &'static str,
}

/// Both directions until both have ended. There is no idle limit while both are open (presence and push connections stay
/// quiet for minutes). Once one side has ended, the other may go on as long as something comes at least every `linger`.
async fn shuttle<L, R>(local: L, remote: R, linger: Duration) -> Done
where
    L: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use std::sync::atomic::{AtomicU64, Ordering};
    let (mut lr, mut lw) = tokio::io::split(local);
    let (mut rr, mut rw) = tokio::io::split(remote);
    let (up, down) = (AtomicU64::new(0), AtomicU64::new(0));
    let first = {
        let sending = copy(&mut lr, &mut rw, &up);
        let coming = copy(&mut rr, &mut lw, &down);
        tokio::pin!(sending, coming);
        tokio::select! {
            r = &mut sending => {
                if r.is_ok() {
                    // FiveM has nothing more to send, the answer may still be on its way. The relay is told nothing yet:
                    // nginx would close both directions.
                    rest(coming, &down, linger).await;
                }
                if r.is_ok() { "program" } else { "break" }
            }
            r = &mut coming => if r.is_ok() { "server" } else { "break" }
        }
    };
    if first == "server" {
        // FiveM hears the end at once; what it still says has nobody to go to, but is read: closing a socket with unread
        // bytes in it resets it, and Windows may then throw away the answer FiveM has not read yet
        let _ = lw.shutdown().await;
        let mut buf = vec![0u8; 4096];
        while let Ok(Ok(1..)) = tokio::time::timeout(linger, lr.read(&mut buf)).await {}
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = lw.shutdown().await;
        let _ = rw.shutdown().await;
    })
    .await;
    Done { up: up.load(Ordering::Relaxed), down: down.load(Ordering::Relaxed), first }
}

/// Runs the direction that is left until it ends or nothing has come for `linger`.
async fn rest<F: std::future::Future>(mut left: std::pin::Pin<&mut F>, bytes: &std::sync::atomic::AtomicU64, linger: Duration) {
    let mut seen = bytes.load(std::sync::atomic::Ordering::Relaxed);
    while tokio::time::timeout(linger, left.as_mut()).await.is_err() {
        let now = bytes.load(std::sync::atomic::Ordering::Relaxed);
        if now == seen {
            return;
        }
        seen = now;
    }
}

/// Bytes from one side to the other until the end (Ok) or an error. Every piece is flushed: the outer TLS holds nothing
/// back.
async fn copy<A, B>(from: &mut A, to: &mut B, count: &std::sync::atomic::AtomicU64) -> std::io::Result<()>
where
    A: tokio::io::AsyncRead + Unpin,
    B: tokio::io::AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            // the relay closed without the TLS goodbye: the end of the stream, like any other
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        to.write_all(&buf[..n]).await?;
        to.flush().await?;
        count.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    }
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
    /// 0.5.7: the longest connection (ms), and who ended them: FiveM, the other side, a break.
    pub longest: u64,
    pub by_program: u64,
    pub by_server: u64,
    pub broken: u64,
    /// 0.5.7: how many times another relay took over.
    pub switched: u64,
}

impl Stats {
    fn refuse(&mut self, why: &'static str) {
        match self.refused.iter_mut().find(|(w, _)| *w == why) {
            Some((_, n)) => *n += 1,
            None => self.refused.push((why, 1)),
        }
    }

    fn ended(&mut self, by: &'static str) {
        match by {
            "program" => self.by_program += 1,
            "server" => self.by_server += 1,
            _ => self.broken += 1,
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
        let ends = if self.by_program + self.by_server + self.broken == 0 {
            String::new()
        } else {
            format!(
                "; закончили: игра {}, сервер {}, обрыв {}, самое долгое {} с",
                self.by_program,
                self.by_server,
                self.broken,
                self.longest / 1000
            )
        };
        let switched = if self.switched > 0 { format!("; смен релея {}", self.switched) } else { String::new() };
        format!(
            "работа маршрута через {tunnel}: соединений {}, отправлено {} байт, получено {} байт{ends}{switched}{refused}{last}",
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
            let fwd = Arc::new(Forward::new(vec![tunnel.clone()], vec!["scui.rockstargames.com".into()]));
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

    /// (г) port 443 is taken: the second loopback address, then an error («порт 443 занят»).
    #[test]
    fn the_local_port_is_taken() {
        let a = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let b = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (taken1, taken2) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let free = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        assert_eq!(bind_first(&[taken1, free]).unwrap().0.local_addr().unwrap(), free);
        assert!(bind_first(&[taken1, taken2]).is_err());
        assert_eq!(LOCAL.map(|ip| ip.is_loopback()), [true, true]);
        assert_eq!(PORT, 443);
    }

    /// 0.5.7: the relay stops answering: the next one takes over, nothing else changes.
    #[test]
    fn another_relay_takes_over() {
        rt().block_on(async {
            let (addr, relay_task) = relay(|mut tls| async move {
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = tls.read(&mut buf).await {
                    if n == 0 || tls.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            })
            .await;
            let pin = hex(&fingerprint(CERT));
            let dead = Tunnel { address: "127.0.0.1:1".into(), sha256: pin.clone() };
            let live = Tunnel { address: addr.clone(), sha256: pin };
            let fwd = Arc::new(Forward::new(vec![dead, live], vec!["rsg.sc".into()]));
            assert_eq!(fwd.address(), "127.0.0.1:1");
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let local = listener.local_addr().unwrap();
            let serve_task = tokio::spawn(serve(listener, fwd.clone()));
            let mut c = TcpStream::connect(local).await.unwrap();
            let hello = client_hello("rsg.sc");
            c.write_all(&hello).await.unwrap();
            let mut got = vec![0u8; hello.len()];
            tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut got)).await.unwrap().unwrap();
            assert_eq!(got, hello);
            assert_eq!(fwd.address(), addr);
            assert_eq!(fwd.stats().switched, 1);
            // the check can also choose one that is in the list
            fwd.prefer("127.0.0.1:1");
            assert_eq!(fwd.address(), "127.0.0.1:1");
            fwd.prefer("8.8.8.8:1");
            assert_eq!(fwd.address(), "127.0.0.1:1");
            serve_task.abort();
            relay_task.abort();
        });
    }

    /// 0.5.7: the handshake in pieces, and one whose name lies past 2 KB (the old limit).
    #[test]
    fn handshake_in_pieces_and_long() {
        rt().block_on(async {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            let hello = client_hello("scui.rockstargames.com");
            // a padding extension of 6 KB before the name
            let long = {
                let name_at = hello.len() - ("scui.rockstargames.com".len() + 9);
                let mut pad = vec![0x00, 0x15];
                pad.extend_from_slice(&6000u16.to_be_bytes());
                pad.extend_from_slice(&[0u8; 6000]);
                let mut h = hello[..name_at].to_vec();
                h.extend_from_slice(&pad);
                h.extend_from_slice(&hello[name_at..]);
                let grow = pad.len();
                let rec = u16::from_be_bytes([h[3], h[4]]) as usize + grow;
                h[3..5].copy_from_slice(&(rec as u16).to_be_bytes());
                let hs = u32::from_be_bytes([0, h[6], h[7], h[8]]) as usize + grow;
                h[6..9].copy_from_slice(&(hs as u32).to_be_bytes()[1..]);
                let ext = u16::from_be_bytes([h[name_at - 2], h[name_at - 1]]) as usize + grow;
                h[name_at - 2..name_at].copy_from_slice(&(ext as u16).to_be_bytes());
                h
            };
            assert!(long.len() > 6000);
            assert_eq!(sni_of(&long).as_deref(), Some("scui.rockstargames.com"));
            assert_eq!(sni_of(&long[..2048]), None);
            for bytes in [hello.clone(), long] {
                let sender = tokio::spawn(async move {
                    let mut c = TcpStream::connect(addr).await.unwrap();
                    c.set_nodelay(true).unwrap();
                    for piece in bytes.chunks(bytes.len() / 4 + 1) {
                        c.write_all(piece).await.unwrap();
                        tokio::time::sleep(Duration::from_millis(40)).await;
                    }
                    tokio::time::sleep(Duration::from_millis(300)).await;
                });
                let (conn, _) = l.accept().await.unwrap();
                let name = tokio::time::timeout(HELLO_TIMEOUT, hello_name(&conn)).await.unwrap();
                assert_eq!(name.as_deref(), Some("scui.rockstargames.com"));
                sender.await.unwrap();
            }
            // not a handshake: refused at once
            let mut c = TcpStream::connect(addr).await.unwrap();
            c.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
            let (conn, _) = l.accept().await.unwrap();
            assert_eq!(tokio::time::timeout(Duration::from_secs(2), hello_name(&conn)).await.unwrap(), None);
        });
    }

    /// 0.5.7: long connections. A quiet one stays open; a program that has finished sending still gets the whole answer
    /// and only then does the relay hear the end (like nginx, which closes both directions on it); the other side's end
    /// reaches the program at once.
    #[test]
    fn long_connections_and_half_closed_ones() {
        rt().block_on(async {
            let linger = Duration::from_millis(400);
            // quiet for longer than `linger` with both sides open, then data both ways
            let (mut app, local) = tokio::io::duplex(64 * 1024);
            let (remote, mut server) = tokio::io::duplex(64 * 1024);
            let task = tokio::spawn(shuttle(local, remote, linger));
            tokio::time::sleep(linger * 3).await;
            assert!(!task.is_finished());
            app.write_all(b"ping").await.unwrap();
            let mut four = [0u8; 4];
            server.read_exact(&mut four).await.unwrap();
            server.write_all(b"pong").await.unwrap();
            app.read_exact(&mut four).await.unwrap();
            assert_eq!(&four, b"pong");
            // the program finishes sending; the answer comes slowly after that
            app.shutdown().await.unwrap();
            for _ in 0..4 {
                tokio::time::sleep(linger / 2).await;
                server.write_all(b"more").await.unwrap();
                app.read_exact(&mut four).await.unwrap();
                assert_eq!(&four, b"more");
            }
            drop(server);
            let done = tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
            assert_eq!(done, Done { up: 4, down: 20, first: "program" });

            // the program is gone and the other side says nothing: closed after `linger`
            let (app, local) = tokio::io::duplex(1024);
            let (remote, _server) = tokio::io::duplex(1024);
            let task = tokio::spawn(shuttle(local, remote, linger));
            drop(app);
            let done = tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
            assert_eq!(done.first, "program");

            // the other side ends: the program hears it at once
            let (mut app, local) = tokio::io::duplex(1024);
            let (remote, mut server) = tokio::io::duplex(1024);
            let task = tokio::spawn(shuttle(local, remote, Duration::from_secs(30)));
            server.write_all(b"bye").await.unwrap();
            drop(server);
            let mut all = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), app.read_to_end(&mut all)).await.unwrap().unwrap();
            assert_eq!(all, b"bye");
            app.write_all(b"too late").await.unwrap();
            drop(app);
            let done = tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
            assert_eq!((done.down, done.first), (3, "server"));
        });
    }

    /// 0.5.7: through the whole chain (local TCP, TLS to the relay): the answer after the program's FIN is whole.
    #[test]
    fn answer_after_the_programs_fin_comes_through() {
        rt().block_on(async {
            let (addr, relay_task) = relay(|mut tls| async move {
                let mut buf = vec![0u8; 4096];
                let n = tls.read(&mut buf).await.unwrap_or(0);
                // an answer of 300 KB in pieces, then the end; the relay gives up when it hears the end of the program
                for _ in 0..30 {
                    if tls.write_all(&vec![b'z'; 10 * 1024]).await.is_err() {
                        return;
                    }
                    let _ = tls.flush().await;
                    let mut one = [0u8; 1];
                    if let Ok(Ok(0)) = tokio::time::timeout(Duration::from_millis(10), tls.read(&mut one)).await {
                        return;
                    }
                }
                let _ = (n, tls.shutdown().await);
            })
            .await;
            let tunnel = Tunnel { address: addr, sha256: hex(&fingerprint(CERT)) };
            let fwd = Arc::new(Forward::new(vec![tunnel], vec!["rsg.sc".into()]));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let local = listener.local_addr().unwrap();
            let serve_task = tokio::spawn(serve(listener, fwd.clone()));
            let mut c = TcpStream::connect(local).await.unwrap();
            c.write_all(&client_hello("rsg.sc")).await.unwrap();
            c.shutdown().await.unwrap();
            let mut all = Vec::new();
            tokio::time::timeout(Duration::from_secs(15), c.read_to_end(&mut all)).await.unwrap().unwrap();
            assert_eq!(all.len(), 300 * 1024);
            tokio::time::sleep(Duration::from_millis(200)).await;
            let s = fwd.stats();
            assert_eq!((s.connections, s.broken), (1, 0), "{s:?}");
            assert!(s.line("x").contains("закончили: игра 1, сервер 0, обрыв 0"), "{}", s.line("x"));
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
    pub async fn cfx_like(acc: tokio_rustls::TlsAcceptor, outer: tokio_rustls::server::TlsStream<TcpStream>, size: usize) {
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
