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
#[derive(Clone, Debug)]
pub struct Forward {
    pub tunnel: Tunnel,
    pub hosts: Vec<String>,
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
    let name = sni_of(&head[..n]).ok_or_else(|| bad("no name"))?;
    if !f.hosts.iter().any(|h| *h == name) {
        return Err(bad("not ours"));
    }
    let mut remote = open(&f.tunnel).await?;
    let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
    let _ = remote.shutdown().await;
    Ok(())
}

/// Asks `host` through the tunnel: the HTTP status of `HEAD /` (any status = the path works). Err: the relay does
/// not answer, its certificate is wrong, or the host did not answer through it within `PROBE_TIMEOUT`.
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
            let fwd = Arc::new(Forward { tunnel: tunnel.clone(), hosts: vec!["scui.rockstargames.com".into()] });
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
