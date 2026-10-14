//! `napd`: a server speaking the Napster 2.0 protocol, plus the metaserver
//! (redirector) that the original client contacts first.

pub mod db;
mod guard;
mod session;
pub mod state;
pub mod tls;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use napster_proto::{msg, Message};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

use crate::db::Db;
use crate::state::{Channel, State};

pub const VERSION: &str = concat!("napd ", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone)]
pub struct Config {
    /// Main server address (the original used 8888).
    pub listen: SocketAddr,
    /// Metaserver / redirector address (the original used 8875).
    pub meta_listen: Option<SocketAddr>,
    /// Host the metaserver hands out. Defaults to the local address the
    /// metaserver connection arrived on.
    pub public_host: Option<String>,
    /// Where `db.json` (accounts and bans) lives. `None` keeps it in memory.
    pub data_dir: Option<PathBuf>,
    pub motd: Vec<String>,
    /// Predefined channels.
    pub channels: Vec<String>,
    /// Hard cap on search results per request.
    pub max_results: u32,
    /// Nicks that are made Elite when they log in.
    pub elite: Vec<String>,
    /// How often unsolicited stats (214) are pushed to clients.
    pub stats_interval: Duration,
    /// Encrypted (TLS) port for napster-rs clients. The plain port stays
    /// open for the original 2001 client.
    pub tls_listen: Option<SocketAddr>,
    /// PEM certificate chain and key; a self-signed pair is generated in
    /// `data_dir` when these are absent.
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// Simultaneous connections allowed from one IP address (0 = no limit).
    pub max_conns_per_ip: usize,
    /// Simultaneous connections allowed in total (0 = no limit).
    pub max_connections: usize,
    /// Files one user may share.
    pub max_shares: usize,
    /// Channels one user may be in at once.
    pub max_channels_per_user: usize,
    /// User-created channels allowed on the server.
    pub max_user_channels: usize,
    /// Sustained messages per second allowed per connection (shares exempt).
    pub msg_rate: f64,
    /// Burst of messages allowed above `msg_rate`.
    pub msg_burst: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: ([0, 0, 0, 0], napster_proto::SERVER_PORT).into(),
            meta_listen: Some(([0, 0, 0, 0], napster_proto::METASERVER_PORT).into()),
            public_host: None,
            data_dir: None,
            motd: vec![
                format!("Welcome to {VERSION}."),
                "This server speaks the Napster 2.0 protocol.".into(),
            ],
            channels: [
                "Lobby",
                "Help",
                "Alternative",
                "Classic-Rock",
                "Country",
                "Electronic",
                "Hip-Hop",
                "Jazz",
                "Metal",
                "Pop",
                "Classical",
                "80s",
            ]
            .map(String::from)
            .to_vec(),
            max_results: 100,
            elite: vec![],
            stats_interval: Duration::from_secs(180),
            tls_listen: None,
            tls_cert: None,
            tls_key: None,
            max_conns_per_ip: 8,
            max_connections: 5000,
            max_shares: 20_000,
            max_channels_per_user: 20,
            max_user_channels: 500,
            msg_rate: 20.0,
            msg_burst: 200.0,
        }
    }
}

pub(crate) struct Shared {
    pub config: Config,
    pub state: Mutex<State>,
    guard: Mutex<guard::Guard>,
}

impl Shared {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn guard(&self) -> std::sync::MutexGuard<'_, guard::Guard> {
        self.guard.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A bound but not yet running server.
pub struct Server {
    listener: TcpListener,
    meta: Option<TcpListener>,
    tls: Option<(TcpListener, TlsAcceptor)>,
    tls_fingerprint: Option<String>,
    shared: Arc<Shared>,
}

impl Server {
    pub async fn bind(config: Config) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(config.listen)
            .await
            .with_context(|| format!("binding {}", config.listen))?;
        let meta = match config.meta_listen {
            Some(addr) => Some(TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?),
            None => None,
        };
        if let Some(dir) = &config.data_dir {
            std::fs::create_dir_all(dir)?;
        }
        let (tls, tls_fingerprint) = match config.tls_listen {
            Some(addr) => {
                let setup = tls::setup(
                    config.tls_cert.as_deref(),
                    config.tls_key.as_deref(),
                    config.data_dir.as_deref(),
                    config.public_host.as_deref(),
                )?;
                let l = TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
                (Some((l, TlsAcceptor::from(setup.config))), Some(setup.fingerprint))
            }
            None => (None, None),
        };
        let db = Db::load(config.data_dir.as_ref().map(|d| d.join("db.json")))?;
        let mut state = State { db, ..Default::default() };
        for name in &config.channels {
            state.channels.insert(name.to_lowercase(), Channel::new(name, true));
        }
        let shared = Arc::new(Shared { config, state: Mutex::new(state), guard: Mutex::default() });
        Ok(Self { listener, meta, tls, tls_fingerprint, shared })
    }

    /// Address of the TLS port, if enabled.
    pub fn tls_addr(&self) -> Option<SocketAddr> {
        self.tls.as_ref().map(|(l, _)| l.local_addr().expect("bound"))
    }

    /// SHA-256 fingerprint of the TLS certificate, if enabled.
    pub fn tls_fingerprint(&self) -> Option<&str> {
        self.tls_fingerprint.as_deref()
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("bound")
    }

    pub fn meta_addr(&self) -> Option<SocketAddr> {
        self.meta.as_ref().map(|m| m.local_addr().expect("bound"))
    }

    /// Serve until the task is dropped.
    pub async fn run(self) -> anyhow::Result<()> {
        let port = self.local_addr().port();
        if let Some(meta) = self.meta {
            let public_host = self.shared.config.public_host.clone();
            tokio::spawn(run_metaserver(meta, public_host, port));
        }
        tokio::spawn(stats_loop(self.shared.clone()));
        if let Some((listener, acceptor)) = self.tls {
            tracing::info!(
                "TLS listening on {} (certificate SHA-256 {})",
                listener.local_addr()?,
                self.tls_fingerprint.as_deref().unwrap_or("?")
            );
            let shared = self.shared.clone();
            tokio::spawn(async move {
                loop {
                    let Some((stream, peer, slot)) = accept(&shared, &listener).await else { continue };
                    let (shared, acceptor) = (shared.clone(), acceptor.clone());
                    tokio::spawn(async move {
                        let tls = match tokio::time::timeout(Duration::from_secs(15), acceptor.accept(stream)).await {
                            Ok(Ok(s)) => s,
                            Ok(Err(e)) => return tracing::debug!("{peer}: TLS handshake failed: {e}"),
                            Err(_) => return tracing::debug!("{peer}: TLS handshake timed out"),
                        };
                        if let Err(e) = session::handle(shared, tls, peer, slot).await {
                            tracing::debug!("{peer}: {e:#}");
                        }
                    });
                }
            });
        }
        tracing::info!("{VERSION} listening on {}", self.listener.local_addr()?);
        loop {
            let Some((stream, peer, slot)) = accept(&self.shared, &self.listener).await else { continue };
            let shared = self.shared.clone();
            tokio::spawn(async move {
                if let Err(e) = session::handle(shared, stream, peer, slot).await {
                    tracing::debug!("{peer}: {e:#}");
                }
            });
        }
    }

    /// Convenience for tests: run in the background.
    pub fn spawn(self) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        tokio::spawn(self.run())
    }
}

/// Accept one connection, applying the per-address limits.
async fn accept(shared: &Arc<Shared>, listener: &TcpListener) -> Option<(TcpStream, SocketAddr, guard::ConnSlot)> {
    let (stream, peer) = match listener.accept().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("accept failed: {e}");
            tokio::time::sleep(Duration::from_millis(100)).await;
            return None;
        }
    };
    let Some(slot) = guard::admit(shared, peer.ip()) else {
        tracing::info!("{peer}: refused, too many connections");
        return None;
    };
    let _ = stream.set_nodelay(true);
    Some((stream, peer, slot))
}

/// The redirector: answers each connection with `<ip>:<port>\n` and closes.
/// The b10 client also understands `WAIT <seconds>` (server busy).
async fn run_metaserver(listener: TcpListener, public_host: Option<String>, port: u16) {
    if let Ok(a) = listener.local_addr() {
        tracing::info!("metaserver listening on {a}");
    }
    loop {
        let Ok((mut stream, peer)) = listener.accept().await else { continue };
        let host = public_host.clone();
        tokio::spawn(async move {
            let host = host.unwrap_or_else(|| advertised_ip(&stream).to_string());
            tracing::debug!("metaserver: sending {peer} to {host}:{port}");
            let _ = stream.write_all(format!("{host}:{port}\n").as_bytes()).await;
            let _ = stream.shutdown().await;
        });
    }
}

fn advertised_ip(stream: &TcpStream) -> IpAddr {
    match stream.local_addr().map(|a| a.ip()) {
        Ok(IpAddr::V6(v6)) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        Ok(ip) => ip,
        Err(_) => IpAddr::from([127, 0, 0, 1]),
    }
}

async fn stats_loop(shared: Arc<Shared>) {
    let mut tick = tokio::time::interval(shared.config.stats_interval);
    tick.tick().await;
    loop {
        tick.tick().await;
        let state = shared.lock();
        let stats = Message::new(msg::SERVER_STATS, state.stats());
        for s in state.sessions.values() {
            s.send(stats.clone());
        }
    }
}
