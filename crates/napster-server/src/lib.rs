//! `napd`: a server speaking the Napster 2.0 protocol, plus the metaserver
//! (redirector) that the original client contacts first.

pub mod db;
mod session;
pub mod state;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use napster_proto::{msg, Message};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

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
        }
    }
}

pub(crate) struct Shared {
    pub config: Config,
    pub state: Mutex<State>,
}

impl Shared {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A bound but not yet running server.
pub struct Server {
    listener: TcpListener,
    meta: Option<TcpListener>,
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
        let db = Db::load(config.data_dir.as_ref().map(|d| d.join("db.json")))?;
        let mut state = State { db, ..Default::default() };
        for name in &config.channels {
            state.channels.insert(name.to_lowercase(), Channel::new(name, true));
        }
        Ok(Self { listener, meta, shared: Arc::new(Shared { config, state: Mutex::new(state) }) })
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
        tracing::info!("{VERSION} listening on {}", self.listener.local_addr()?);
        loop {
            let (stream, peer) = match self.listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let shared = self.shared.clone();
            tokio::spawn(async move {
                if let Err(e) = session::handle(shared, stream, peer).await {
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
