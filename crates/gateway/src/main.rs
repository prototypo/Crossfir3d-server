//! A TLS gateway in front of a Crossfire server. Players connect here, never to the
//! server: each client message is checked (`filter`) before it is passed on, and the
//! server's answers go back unchanged.
//!
//! Settings (environment):
//! - `GATEWAY_LISTEN`  where players connect (default `0.0.0.0:13328`)
//! - `GATEWAY_SERVER`  the Crossfire server, not reachable from outside (default `127.0.0.1:13327`)
//! - `GATEWAY_CERT`, `GATEWAY_KEY`  PEM certificate chain and private key (required)
//! - `GATEWAY_ALLOW_DM=1`  let `dm` through (off: DMs log in on the server machine)

mod filter;

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_rustls::{rustls, TlsAcceptor};

use filter::{Addresses, Bucket, Verdict, MAX_FRAME};

/// From TCP connect to the first client message (the TLS handshake and `version`).
const HANDSHAKE: Duration = Duration::from_secs(10);
/// No message for this long closes the connection (the client sends `beat` every few seconds).
const IDLE: Duration = Duration::from_secs(600);
/// Why a connection ended when the player simply closed it: a normal quit, not logged.
const LEFT: &str = "player left";

struct Config {
    listen: String,
    server: String,
    allow_dm: bool,
    tls: TlsAcceptor,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn config() -> Result<Config, String> {
    let need = |name: &str| std::env::var(name).map_err(|_| format!("{name} is not set"));
    let (cert, key) = (need("GATEWAY_CERT")?, need("GATEWAY_KEY")?);
    let certs = CertificateDer::pem_file_iter(&cert)
        .and_then(|i| i.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("{cert}: {e}"))?;
    let key = PrivateKeyDer::from_pem_file(&key).map_err(|e| format!("{key}: {e}"))?;
    let tls = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(certs, key).map_err(|e| e.to_string())?;
    Ok(Config {
        listen: env_or("GATEWAY_LISTEN", "0.0.0.0:13328"),
        server: env_or("GATEWAY_SERVER", "127.0.0.1:13327"),
        allow_dm: std::env::var("GATEWAY_ALLOW_DM").is_ok_and(|v| v == "1"),
        tls: TlsAcceptor::from(Arc::new(tls)),
    })
}

#[tokio::main]
async fn main() {
    let config = config().unwrap_or_else(|e| fail(&e));
    let listener = TcpListener::bind(&config.listen).await.unwrap_or_else(|e| fail(&format!("cannot listen on {}: {e}", config.listen)));
    eprintln!("gateway: {} -> {}", config.listen, config.server);
    let (config, addresses) = (Arc::new(config), Arc::new(Mutex::new(Addresses::default())));
    loop {
        let Ok((tcp, peer)) = listener.accept().await else { continue };
        let ip = peer.ip();
        if !lock(&addresses).connect(ip, Instant::now()) {
            eprintln!("{ip}: too many connections");
            continue; // dropping `tcp` closes it
        }
        let (config, addresses) = (config.clone(), addresses.clone());
        tokio::spawn(async move {
            match serve(tcp, peer, &config, &addresses).await {
                Err(why) if why != LEFT => eprintln!("{ip}: closed: {why}"),
                _ => {} // a normal quit, or a port check
            }
            lock(&addresses).disconnect(ip);
        });
    }
}

fn fail(why: &str) -> ! {
    eprintln!("gateway: {why}");
    std::process::exit(2);
}

/// The address table; a panic elsewhere while holding it must not stop the gateway.
fn lock(a: &Mutex<Addresses>) -> std::sync::MutexGuard<'_, Addresses> {
    a.lock().unwrap_or_else(|e| e.into_inner())
}

/// One player's connection: TLS from the player, plain TCP to the server.
async fn serve(tcp: TcpStream, peer: SocketAddr, config: &Config, addresses: &Mutex<Addresses>) -> Result<(), String> {
    let _ = tcp.set_nodelay(true);
    let deadline = Instant::now() + HANDSHAKE;
    let tls = timeout(HANDSHAKE, config.tls.accept(tcp)).await.map_err(|_| "slow TLS handshake")?.map_err(|e| {
        // Closed before saying anything: a port check (the health check every 30 s).
        if e.kind() == std::io::ErrorKind::UnexpectedEof { LEFT.to_string() } else { format!("TLS: {e}") }
    })?;
    let (mut from_player, mut to_player) = tokio::io::split(tls);
    // The server is only contacted once the player has said something valid.
    let first = timeout(deadline.saturating_duration_since(Instant::now()), read_frame(&mut from_player))
        .await
        .map_err(|_| "no first message")??;
    pass(&first, peer.ip(), config.allow_dm, addresses)?;
    let upstream = timeout(Duration::from_secs(5), TcpStream::connect(&config.server))
        .await
        .map_err(|_| "server slow")?
        .map_err(|e| format!("server: {e}"))?;
    let _ = upstream.set_nodelay(true);
    let (mut from_server, mut to_server) = upstream.into_split();
    write_frame(&mut to_server, &first).await?;
    // The server's answers go back as they are.
    let down = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut from_server, &mut to_player).await;
        let _ = to_player.shutdown().await;
    });
    let result = relay(&mut from_player, &mut to_server, peer.ip(), config.allow_dm, addresses).await;
    down.abort();
    result
}

/// Checks and passes on the player's messages until something ends the connection.
async fn relay(
    from_player: &mut (impl AsyncReadExt + Unpin),
    to_server: &mut (impl AsyncWriteExt + Unpin),
    ip: IpAddr,
    allow_dm: bool,
    addresses: &Mutex<Addresses>,
) -> Result<(), String> {
    let mut bucket = Bucket::new(Instant::now());
    loop {
        let msg = timeout(IDLE, read_frame(from_player)).await.map_err(|_| "idle")??;
        if !bucket.take(Instant::now()) {
            return Err("too many messages".into());
        }
        pass(&msg, ip, allow_dm, addresses)?;
        write_frame(to_server, &msg).await?;
    }
}

/// Whether the gateway passes this message on; Err says why the connection ends.
fn pass(msg: &[u8], ip: IpAddr, allow_dm: bool, addresses: &Mutex<Addresses>) -> Result<(), String> {
    match filter::check(msg, allow_dm) {
        Verdict::Pass => Ok(()),
        Verdict::Login if lock(addresses).login(ip, Instant::now()) => Ok(()),
        Verdict::Login => Err("too many password tries".into()),
        Verdict::Refuse(why) => Err(why.into()),
    }
}

/// One client message: a 2-byte big-endian length, then that many bytes (1 to MAX_FRAME).
async fn read_frame(r: &mut (impl AsyncReadExt + Unpin)) -> Result<Vec<u8>, String> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await.map_err(|_| LEFT)?;
    let len = u16::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(format!("message of {len} bytes"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.map_err(|_| LEFT)?;
    Ok(buf)
}

/// Writes one message (at most MAX_FRAME bytes, so its length fits the 2 bytes).
async fn write_frame(w: &mut (impl AsyncWriteExt + Unpin), payload: &[u8]) -> Result<(), String> {
    let mut buf = Vec::with_capacity(payload.len() + 2);
    buf.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf).await.map_err(|e| format!("server: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_are_checked_and_passed_on() {
        let mut input = Vec::new();
        for msg in [&b"version 1023 1029 test"[..], b"beat", b"hello"] {
            input.extend_from_slice(&(msg.len() as u16).to_be_bytes());
            input.extend_from_slice(msg);
        }
        let mut out = Vec::new();
        let addresses = Mutex::new(Addresses::default());
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let r = relay(&mut input.as_slice(), &mut out, ip, false, &addresses).await;
        assert_eq!(r, Err("unknown command".to_string()));
        assert_eq!(out, b"\x00\x16version 1023 1029 test\x00\x04beat");
    }

    #[tokio::test]
    async fn oversize_and_empty_frames_end_the_connection() {
        let mut big = vec![0x10, 0x01];
        big.extend(vec![b'a'; 4097]);
        assert_eq!(read_frame(&mut big.as_slice()).await, Err("message of 4097 bytes".into()));
        assert_eq!(read_frame(&mut &b"\x00\x00"[..]).await, Err("message of 0 bytes".into()));
        assert_eq!(read_frame(&mut &b"\x00\x05ab"[..]).await, Err(LEFT.into()));
    }
}
