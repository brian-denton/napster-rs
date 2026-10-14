//! The engine actor: owns the server connection and turns server messages
//! into events and transfer actions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use futures::{SinkExt, StreamExt};
use napster_proto::{ip, msg, quote, Message, NapCodec, SearchResult};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time::timeout;
use tokio_util::codec::{Framed, FramedRead};
use tokio_util::sync::CancellationToken;

use crate::config::ClientConfig;
use crate::library::{self, Library};
use crate::throttle::Throttle;
use crate::transfer;
use crate::types::*;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const PUSH_WAIT: Duration = Duration::from_secs(120);

pub(crate) struct Entry {
    pub info: TransferInfo,
    pub cancel: CancellationToken,
}

#[derive(Default)]
pub(crate) struct Transfers {
    next_id: u64,
    pub map: BTreeMap<u64, Entry>,
}

pub(crate) struct Shared {
    pub config: Mutex<ClientConfig>,
    events: UnboundedSender<Event>,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
    pub library: RwLock<Arc<Library>>,
    pub transfers: Mutex<Transfers>,
    server: Mutex<Option<UnboundedSender<Message>>>,
    pub nick: Mutex<String>,
    /// Port we're actually listening on (0 = firewalled).
    pub data_port: Mutex<u16>,
    /// (nick, filename) -> transfer waiting for a firewalled uploader.
    pub pending_pushes: Mutex<HashMap<(String, String), u64>>,
    /// Nicks we've asked for a direct browse.
    pub pending_browses: Mutex<HashSet<String>>,
    /// Bandwidth shared by all downloads / all uploads.
    pub down_throttle: Throttle,
    pub up_throttle: Throttle,
    /// Is the server we're logged in to on a local network? (Unknown
    /// counts as no, e.g. a host name only the proxy resolved.)
    pub server_local: Mutex<bool>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub fn emit(&self, e: Event) {
        let _ = self.events.send(e);
        if let Some(n) = &self.notify {
            n();
        }
    }

    pub fn send(&self, kind: u16, data: impl Into<String>) -> bool {
        match &*lock(&self.server) {
            Some(tx) => tx.send(Message::new(kind, data)).is_ok(),
            None => false,
        }
    }

    pub fn connected(&self) -> bool {
        lock(&self.server).is_some()
    }

    pub fn library(&self) -> Arc<Library> {
        self.library.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn my_nick(&self) -> String {
        lock(&self.nick).clone()
    }

    pub fn config(&self) -> ClientConfig {
        lock(&self.config).clone()
    }

    /// A server on the internet has no business sending us to machines on
    /// our own network (a malicious server could use clients to probe
    /// LANs), so peers at private addresses are only allowed when the
    /// server itself is local.
    pub fn check_peer(&self, addr: std::net::SocketAddr) -> anyhow::Result<()> {
        if !*lock(&self.server_local) && is_local_ip(addr.ip()) {
            anyhow::bail!("refusing to connect to private address {} for a server on the internet", addr.ip());
        }
        if addr.port() == 0 || addr.ip().is_unspecified() || addr.ip().is_multicast() {
            anyhow::bail!("refusing to connect to invalid address {addr}");
        }
        Ok(())
    }

    /// Open a connection to a peer, through the proxy if transfers use it.
    pub async fn dial_peer(&self, addr: std::net::SocketAddr) -> std::io::Result<TcpStream> {
        let proxy = self.config().active_proxy().filter(|p| p.transfers).cloned();
        match proxy {
            Some(p) => crate::proxy::connect(&p, &addr.ip().to_string(), addr.port()).await,
            None => TcpStream::connect(addr).await,
        }
    }

    /// Current (download, upload) caps in bytes/second; 0 = unlimited.
    pub fn rate_limits(&self) -> (u64, u64) {
        lock(&self.config).rate_limits()
    }

    pub fn new_transfer(
        &self,
        direction: Direction,
        nick: &str,
        filename: &str,
        size: u64,
        md5: &str,
        state: TransferState,
    ) -> (u64, CancellationToken) {
        let mut t = lock(&self.transfers);
        t.next_id += 1;
        let id = t.next_id;
        let cancel = CancellationToken::new();
        let info = TransferInfo {
            id,
            direction,
            nick: nick.to_owned(),
            filename: filename.to_owned(),
            size,
            offset: 0,
            transferred: 0,
            state,
            started: None,
            saved_to: None,
            md5: md5.to_owned(),
        };
        t.map.insert(id, Entry { info: info.clone(), cancel: cancel.clone() });
        drop(t);
        self.emit(Event::Transfer(info));
        (id, cancel)
    }

    /// Mutate a transfer and publish the new state.
    pub fn update(&self, id: u64, f: impl FnOnce(&mut TransferInfo)) {
        let mut t = lock(&self.transfers);
        let Some(e) = t.map.get_mut(&id) else { return };
        // don't resurrect a cancelled transfer from a racing task
        if e.info.state == TransferState::Cancelled {
            return;
        }
        f(&mut e.info);
        let info = e.info.clone();
        drop(t);
        self.emit(Event::Transfer(info));
    }

    pub fn transfer(&self, id: u64) -> Option<TransferInfo> {
        lock(&self.transfers).map.get(&id).map(|e| e.info.clone())
    }

    pub fn fail(&self, id: u64, why: impl Into<String>) {
        let why = why.into();
        self.update(id, |t| {
            if !t.state.is_finished() {
                t.state = TransferState::Failed(why);
            }
        });
    }

    fn count_active(&self, dir: Direction) -> usize {
        lock(&self.transfers)
            .map
            .values()
            .filter(|e| {
                e.info.direction == dir
                    && matches!(
                        e.info.state,
                        TransferState::Requesting
                            | TransferState::Connecting
                            | TransferState::WaitingForPush
                            | TransferState::Transferring
                    )
            })
            .count()
    }

    pub fn active_uploads(&self) -> usize {
        self.count_active(Direction::Upload)
    }

    /// Start queued downloads while there are free slots.
    pub fn pump_downloads(&self) {
        if !self.connected() {
            return;
        }
        let max = self.config().max_downloads.max(1) as usize;
        let active = self.count_active(Direction::Download);
        let queued: Vec<(u64, String, String)> = lock(&self.transfers)
            .map
            .values()
            .filter(|e| e.info.direction == Direction::Download && e.info.state == TransferState::Queued)
            .map(|e| (e.info.id, e.info.nick.clone(), e.info.filename.clone()))
            .collect();
        for (id, nick, filename) in queued.into_iter().take(max.saturating_sub(active)) {
            self.update(id, |t| t.state = TransferState::Requesting);
            self.send(msg::DOWNLOAD_REQUEST, format!("{} {}", nick, quote(&filename)));
        }
    }

    /// Find a download in `state` for `nick`/`filename`.
    fn find_download(&self, nick: &str, filename: &str, states: &[TransferState]) -> Option<u64> {
        lock(&self.transfers)
            .map
            .values()
            .find(|e| {
                e.info.direction == Direction::Download
                    && e.info.nick.eq_ignore_ascii_case(nick)
                    && e.info.filename == filename
                    && states.contains(&e.info.state)
            })
            .map(|e| e.info.id)
    }
}

/// Handle to a running engine.
#[derive(Clone)]
pub struct Client {
    tx: UnboundedSender<Command>,
}

impl Client {
    /// Spawn the engine on the current tokio runtime. `notify` is called
    /// after every event (GUIs use it to request a repaint).
    pub fn start(
        config: ClientConfig,
        notify: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> (Client, UnboundedReceiver<Event>) {
        let (tx, rx) = unbounded_channel();
        let (etx, erx) = unbounded_channel();
        let shared = Arc::new(Shared {
            config: Mutex::new(config),
            events: etx,
            notify,
            library: RwLock::new(Arc::new(Library::default())),
            transfers: Mutex::new(Transfers::default()),
            server: Mutex::new(None),
            nick: Mutex::new(String::new()),
            data_port: Mutex::new(0),
            pending_pushes: Mutex::new(HashMap::new()),
            pending_browses: Mutex::new(HashSet::new()),
            down_throttle: Throttle::default(),
            up_throttle: Throttle::default(),
            server_local: Mutex::new(true),
        });
        tokio::spawn(Actor::new(shared, rx).run());
        (Client { tx }, erx)
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

/// A server connection, plain or TLS.
trait ServerIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> ServerIo for T {}

type ServerReader = FramedRead<tokio::io::ReadHalf<Box<dyn ServerIo>>, NapCodec>;

struct Actor {
    shared: Arc<Shared>,
    cmds: UnboundedReceiver<Command>,
    reader: Option<ServerReader>,
    listener: Option<(u16, CancellationToken)>,
    scan_cancel: Option<CancellationToken>,
    /// accumulating 408s until 409
    channel_users: HashMap<String, Vec<ChannelUser>>,
    /// accumulating 618s until 617
    channel_list: Vec<(String, u32, String)>,
}

impl Actor {
    fn new(shared: Arc<Shared>, cmds: UnboundedReceiver<Command>) -> Self {
        Self {
            shared,
            cmds,
            reader: None,
            listener: None,
            scan_cancel: None,
            channel_users: HashMap::new(),
            channel_list: Vec::new(),
        }
    }

    async fn run(mut self) {
        self.rescan();
        if self.shared.config().check_for_updates && auto_check_due() {
            self.check_update(false);
        }
        loop {
            tokio::select! {
                cmd = self.cmds.recv() => match cmd {
                    Some(c) => self.command(c).await,
                    None => break,
                },
                m = next_msg(&mut self.reader) => match m {
                    Some(Ok(m)) => self.server_message(m),
                    Some(Err(e)) => self.disconnected(&format!("{e}")),
                    None => self.disconnected("connection closed by server"),
                },
            }
        }
        self.disconnected("client shut down");
        if let Some((_, c)) = self.listener.take() {
            c.cancel();
        }
    }

    // ------------------------------------------------------------ commands

    async fn command(&mut self, c: Command) {
        let s = self.shared.clone();
        match c {
            Command::Connect => {
                if self.reader.is_some() {
                    s.emit(Event::Info("You are already connected to the Napster Server!".into()));
                    return;
                }
                s.emit(Event::State(ConnState::Connecting));
                if let Err(e) = self.connect().await {
                    s.emit(Event::Error(format!("{e:#}")));
                    s.emit(Event::State(ConnState::Disconnected));
                }
            }
            Command::Disconnect => self.disconnected("disconnected by user"),
            Command::SetConfig(cfg) => {
                let cfg = *cfg;
                let old = std::mem::replace(&mut *lock(&s.config), cfg.clone());
                if old.shared_dirs != cfg.shared_dirs || old.extensions != cfg.extensions {
                    self.rescan();
                }
                if old.link_speed != cfg.link_speed {
                    s.send(msg::CHANGE_LINK_SPEED, cfg.link_speed.to_string());
                }
                if old.data_port != cfg.data_port && self.reader.is_some() {
                    self.start_listener();
                    let port = *lock(&s.data_port);
                    s.send(msg::CHANGE_DATA_PORT, port.to_string());
                }
                s.pump_downloads();
            }
            Command::Rescan => self.rescan(),
            Command::Search(q) => {
                if !s.send(msg::SEARCH, q.to_payload()) {
                    s.emit(Event::Error("Not connected to Server!".into()));
                    s.emit(Event::SearchEnd);
                }
            }
            Command::Download { nick, file } => {
                if !crate::is_audio_file(&file.filename) {
                    s.emit(Event::Error(format!(
                        "Refusing to download {:?}: Napster only downloads audio files ({}).",
                        file.basename(),
                        crate::AUDIO_EXTENSIONS.join(", ")
                    )));
                    return;
                }
                if nick.eq_ignore_ascii_case(&s.my_nick()) {
                    s.emit(Event::Error("You can't download from yourself.".into()));
                    return;
                }
                let dup = lock(&s.transfers).map.values().any(|e| {
                    e.info.direction == Direction::Download
                        && e.info.nick.eq_ignore_ascii_case(&nick)
                        && e.info.filename == file.filename
                        && !e.info.state.is_finished()
                });
                if dup {
                    return;
                }
                s.new_transfer(Direction::Download, &nick, &file.filename, file.size, &file.md5, TransferState::Queued);
                s.pump_downloads();
            }
            Command::CancelTransfer(id) => {
                let mut t = lock(&s.transfers);
                if let Some(e) = t.map.get_mut(&id) {
                    if !e.info.state.is_finished() {
                        e.cancel.cancel();
                        e.info.state = TransferState::Cancelled;
                        let info = e.info.clone();
                        drop(t);
                        lock(&s.pending_pushes).retain(|_, v| *v != id);
                        s.emit(Event::Transfer(info));
                        s.pump_downloads();
                    }
                }
            }
            Command::ClearFinished => {
                let removed: Vec<u64> = {
                    let mut t = lock(&s.transfers);
                    let ids: Vec<u64> =
                        t.map.values().filter(|e| e.info.state.is_finished()).map(|e| e.info.id).collect();
                    for id in &ids {
                        t.map.remove(id);
                    }
                    ids
                };
                for id in removed {
                    s.emit(Event::TransferRemoved(id));
                }
            }
            Command::Browse(nick) => {
                if nick.eq_ignore_ascii_case(&s.my_nick()) {
                    s.emit(Event::Error("That's you! Your own files are in the Library tab.".into()));
                    s.emit(Event::BrowseEnd { nick });
                    return;
                }
                lock(&s.pending_browses).insert(nick.to_lowercase());
                if !s.send(msg::DIRECT_BROWSE, nick) {
                    s.emit(Event::Error("Not connected to Server!".into()));
                }
            }
            Command::Whois(nick) => {
                s.send(msg::WHOIS, nick);
            }
            Command::Ping(nick) => {
                s.send(msg::PING, nick);
            }
            Command::PingServer => {
                s.send(msg::SERVER_PING, now_millis().to_string());
            }
            Command::PrivateMessage { to, text } => {
                if !s.send(msg::PRIVATE_MESSAGE, format!("{to} {text}")) {
                    s.emit(Event::Error("Not connected to Server!".into()));
                }
            }
            Command::Join(ch) => {
                s.send(msg::JOIN_CHANNEL, ch);
            }
            Command::Part(ch) => {
                s.send(msg::PART_CHANNEL, ch);
            }
            Command::Say { channel, text } => {
                s.send(msg::CHANNEL_SAY, format!("{channel} {text}"));
            }
            Command::Emote { channel, text } => {
                s.send(msg::EMOTE, format!("{channel} {}", quote(&text)));
            }
            Command::SetTopic { channel, topic } => {
                s.send(msg::CHANNEL_TOPIC, format!("{channel} {topic}"));
            }
            Command::ListChannels => {
                self.channel_list.clear();
                s.send(msg::LIST_CHANNELS, "");
            }
            Command::HotlistAdd(nick) => {
                let mut cfg = lock(&s.config);
                if !cfg.hotlist.iter().any(|n| n.eq_ignore_ascii_case(&nick)) {
                    cfg.hotlist.push(nick.clone());
                }
                drop(cfg);
                s.send(msg::HOTLIST_ADD, nick);
            }
            Command::HotlistRemove(nick) => {
                lock(&s.config).hotlist.retain(|n| !n.eq_ignore_ascii_case(&nick));
                s.send(msg::HOTLIST_REMOVE, nick);
            }
            Command::Ignore(nick) => {
                s.send(msg::IGNORE_ADD, nick);
            }
            Command::Unignore(nick) => {
                s.send(msg::IGNORE_REMOVE, nick);
            }
            Command::Raw(kind, data) => {
                if !s.send(kind, data) {
                    s.emit(Event::Error("Not connected to Server!".into()));
                }
            }
            Command::CheckForUpdate { manual } => self.check_update(manual),
            Command::InstallUpdate(info) => {
                let s = s.clone();
                let proxy = s.config().active_proxy().cloned();
                tokio::task::spawn_blocking(move || {
                    let progress = |msg: &str| s.emit(Event::UpdateProgress(msg.to_owned()));
                    match crate::update::install(&info, proxy.as_ref(), progress) {
                        Ok(relaunch) => s.emit(Event::UpdateInstalled { version: info.version, relaunch }),
                        Err(e) => s.emit(Event::UpdateFailed(format!("{e:#}"))),
                    }
                });
            }
        }
    }

    fn check_update(&self, manual: bool) {
        let s = self.shared.clone();
        let proxy = s.config().active_proxy().cloned();
        tokio::task::spawn_blocking(move || match crate::update::check(proxy.as_ref()).inspect(|_| mark_checked()) {
            Ok(Some(info)) => s.emit(Event::UpdateAvailable(info)),
            Ok(None) if manual => s.emit(Event::UpToDate),
            Ok(None) => {}
            Err(e) if manual => s.emit(Event::UpdateFailed(format!("{e:#}"))),
            Err(e) => tracing::info!("update check failed: {e:#}"),
        });
    }

    fn rescan(&mut self) {
        if let Some(c) = self.scan_cancel.take() {
            c.cancel();
        }
        let cancel = CancellationToken::new();
        self.scan_cancel = Some(cancel.clone());
        let shared = self.shared.clone();
        let cfg = shared.config();
        tokio::spawn(async move {
            let s2 = shared.clone();
            let lib = tokio::task::spawn_blocking(move || {
                let mut last = std::time::Instant::now();
                library::scan(&cfg.shared_dirs, &cfg.extensions, cfg.cache_library, |done, total| {
                    if done == total || last.elapsed() > Duration::from_millis(250) {
                        last = std::time::Instant::now();
                        s2.emit(Event::LibraryScanning { done, total });
                    }
                })
            })
            .await;
            let Ok(lib) = lib else { return };
            if cancel.is_cancelled() {
                return;
            }
            let lib = Arc::new(lib);
            *shared.library.write().unwrap_or_else(|e| e.into_inner()) = lib.clone();
            shared.emit(Event::Library(lib.files().to_vec()));
            if shared.connected() {
                shared.send(msg::UNSHARE_ALL, "");
                share_all(&shared, &lib);
            }
        });
    }

    fn start_listener(&mut self) {
        let want = self.shared.config().data_port;
        if let Some((port, c)) = &self.listener {
            if *port == want {
                return;
            }
            c.cancel();
        }
        self.listener = None;
        *lock(&self.shared.data_port) = 0;
        if want == 0 {
            return;
        }
        match std::net::TcpListener::bind((self.shared.config().bind_host.as_str(), want)).and_then(|l| {
            l.set_nonblocking(true)?;
            tokio::net::TcpListener::from_std(l)
        }) {
            Ok(l) => {
                let actual = l.local_addr().map(|a| a.port()).unwrap_or(want);
                let cancel = CancellationToken::new();
                *lock(&self.shared.data_port) = actual;
                tokio::spawn(transfer::listen(self.shared.clone(), l, cancel.clone()));
                self.listener = Some((want, cancel));
            }
            Err(e) => {
                tracing::warn!("bind {want}: {e}");
                self.shared.emit(Event::Info(format!("Unable to bind() to port {want}! Going into firewall mode.")));
            }
        }
    }

    // ---------------------------------------------------------- connection

    async fn connect(&mut self) -> anyhow::Result<()> {
        let cfg = self.shared.config();
        if !napster_proto::valid_nick(&cfg.nick) {
            bail!("Invalid user name {:?}. Set one in Preferences.", cfg.nick);
        }
        if cfg.password.is_empty() {
            bail!("Set a password in Preferences.");
        }
        self.start_listener();
        let port = *lock(&self.shared.data_port);

        let proxy = cfg.active_proxy();
        let (host, server_port) = if cfg.use_metaserver {
            self.shared.emit(Event::Status("Finding optimal Server...".into()));
            resolve_metaserver(&cfg.server, proxy).await?
        } else {
            split_host_port(&cfg.server)?
        };
        self.shared.emit(Event::Status(if proxy.is_some() {
            "Connecting to Server through proxy...".into()
        } else {
            "Connecting to Server...".into()
        }));
        let (tcp, server_ip) = dial(proxy, &host, server_port).await.context("Failed connecting to Server...")?;
        let stream: Box<dyn ServerIo> = if cfg.tls {
            if cfg.use_metaserver {
                bail!("Encrypted connections need a direct server address, not a metaserver.");
            }
            let path = cfg.known_servers.clone().unwrap_or_else(crate::tls::KnownServers::default_path);
            let mut known = crate::tls::KnownServers::load(&path);
            let c = timeout(CONNECT_TIMEOUT, crate::tls::connect(tcp, &host, server_port, &mut known))
                .await
                .map_err(|_| anyhow!("TLS handshake with the server timed out"))??;
            if let Some(fp) = c.newly_pinned {
                self.shared.emit(Event::Info(format!(
                    "First encrypted connection to {}: trusting its certificate (SHA-256 {fp}). \
                     You'll be warned if it ever changes.",
                    cfg.server
                )));
            }
            Box::new(c.stream)
        } else {
            Box::new(tcp)
        };
        *lock(&self.shared.server_local) = server_ip.is_some_and(is_local_ip);
        let mut framed = Framed::new(stream, NapCodec);
        // exactly the b10 login layout: nick pass port "client" speed build flag
        framed
            .send(Message::new(
                msg::LOGIN,
                format!(
                    "{} {} {} {} {} {} 0",
                    cfg.nick,
                    cfg.password,
                    port,
                    quote(crate::CLIENT_INFO),
                    cfg.link_speed,
                    napster_proto::CLIENT_BUILD
                ),
            ))
            .await?;
        let email = loop {
            let m = timeout(CONNECT_TIMEOUT, framed.next())
                .await
                .map_err(|_| anyhow!("Login timed out"))?
                .ok_or_else(|| anyhow!("Server closed the connection during login"))??;
            match m.kind {
                msg::LOGIN_ACK => break m.data,
                msg::ERROR => bail!("Unable to login: {}", m.data),
                other => tracing::debug!("pre-login message {other}: {}", m.data),
            }
        };

        let parts = framed.into_parts();
        let (r, w) = tokio::io::split(parts.io);
        let mut reader = FramedRead::new(r, NapCodec);
        *reader.read_buffer_mut() = parts.read_buf;
        let (tx, mut rx) = unbounded_channel::<Message>();
        tokio::spawn(async move {
            let mut writer = tokio_util::codec::FramedWrite::new(w, NapCodec);
            while let Some(m) = rx.recv().await {
                if writer.send(m).await.is_err() {
                    break;
                }
            }
        });
        *lock(&self.shared.server) = Some(tx);
        *lock(&self.shared.nick) = cfg.nick.clone();
        self.reader = Some(reader);
        tracing::info!("logged in as {} ({email}), data port {port}", cfg.nick);

        let s = &self.shared;
        s.emit(Event::State(ConnState::Online { nick: cfg.nick.clone() }));
        for nick in &cfg.hotlist {
            s.send(msg::HOTLIST_INIT, nick.clone());
        }
        share_all(s, &s.library());
        for ch in &cfg.auto_join {
            s.send(msg::JOIN_CHANNEL, ch.clone());
        }
        s.pump_downloads();
        Ok(())
    }

    fn disconnected(&mut self, why: &str) {
        let was = lock(&self.shared.server).take().is_some();
        self.reader = None;
        if !was {
            return;
        }
        // downloads waiting on the server go back in the queue
        let ids: Vec<u64> = lock(&self.shared.transfers)
            .map
            .values()
            .filter(|e| matches!(e.info.state, TransferState::Requesting | TransferState::WaitingForPush))
            .map(|e| e.info.id)
            .collect();
        for id in ids {
            self.shared.update(id, |t| t.state = TransferState::Queued);
        }
        lock(&self.shared.pending_pushes).clear();
        self.shared.emit(Event::State(ConnState::Disconnected));
        self.shared.emit(Event::Status(format!("Disconnected ({why}).")));
    }

    // ----------------------------------------------------- server messages

    fn server_message(&mut self, m: Message) {
        let s = self.shared.clone();
        let mut a = m.args();
        match m.kind {
            msg::SEARCH_RESULT => match SearchResult::parse(&m.data) {
                Some(r) => s.emit(Event::SearchResult(r)),
                None => tracing::debug!("bad search result: {}", m.data),
            },
            msg::SEARCH_END => s.emit(Event::SearchEnd),
            msg::BROWSE_RESULT => {
                if let Some(r) = SearchResult::parse_browse(&m.data) {
                    s.emit(Event::BrowseResult(r));
                }
            }
            msg::BROWSE_END => {
                let nick = a.str().unwrap_or("").to_owned();
                lock(&s.pending_browses).remove(&nick.to_lowercase());
                s.emit(Event::BrowseEnd { nick });
            }
            msg::USER_SIGNOFF => {
                // also the reply to browsing someone who isn't online
                let nick = a.str().unwrap_or("").to_owned();
                if lock(&s.pending_browses).remove(&nick.to_lowercase()) {
                    s.emit(Event::Error(format!("User {nick} is not online.")));
                    s.emit(Event::BrowseEnd { nick: nick.clone() });
                }
                s.emit(Event::HotlistOffline { nick });
            }
            msg::USER_SIGNON => {
                let nick = a.str().unwrap_or("").to_owned();
                let link_speed = a.num().unwrap_or(0);
                s.emit(Event::HotlistOnline { nick, link_speed });
            }
            msg::HOTLIST_ACK => {}
            msg::HOTLIST_ERROR => s.emit(Event::Error(format!("Unable to add {} to your hot list.", m.data))),
            msg::SERVER_STATS => {
                let users = a.num().unwrap_or(0);
                let files = a.num().unwrap_or(0);
                let gigabytes = a.num().unwrap_or(0);
                s.emit(Event::Stats { users, files, gigabytes });
            }
            msg::MOTD => s.emit(Event::Motd(m.data.clone())),
            msg::SERVER_ERROR | msg::ERROR => s.emit(Event::Error(m.data.clone())),
            msg::PRIVATE_MESSAGE => {
                let from = a.str().unwrap_or("").to_owned();
                let text = a.rest().to_owned();
                s.emit(Event::PrivateMessage { from, text });
            }

            // ---- downloads
            msg::DOWNLOAD_ACK => self.download_ack(&m),
            msg::DOWNLOAD_ERROR => {
                let nick = a.str().unwrap_or("").to_owned();
                let file = a.str().unwrap_or("").to_owned();
                if let Some(id) = s.find_download(&nick, &file, &[TransferState::Requesting]) {
                    s.fail(id, "file is no longer available");
                    s.pump_downloads();
                }
            }
            msg::QUEUE_LIMIT_NOTIFY => {
                let nick = a.str().unwrap_or("").to_owned();
                let file = a.str().unwrap_or("").to_owned();
                let _size: u64 = a.num().unwrap_or(0);
                let limit: u32 = a.num().unwrap_or(0);
                if let Some(id) = s.find_download(&nick, &file, &[TransferState::Requesting]) {
                    s.fail(id, format!("remote queue full ({limit} max)"));
                    s.pump_downloads();
                }
            }
            msg::UPLOAD_ACCEPT_FAILED => {
                let nick = a.str().unwrap_or("").to_owned();
                let file = a.str().unwrap_or("").to_owned();
                if let Some(id) = s.find_download(&nick, &file, &[TransferState::Requesting]) {
                    s.fail(id, "remote user did not accept");
                    s.pump_downloads();
                }
            }

            // ---- uploads
            msg::UPLOAD_REQUEST => {
                let nick = a.str().unwrap_or("").to_owned();
                let file = a.str().unwrap_or("").to_owned();
                let max = s.config().max_uploads;
                if s.library().get(&file).is_none() {
                    return;
                }
                if max == 0 || s.active_uploads() >= max as usize {
                    s.send(msg::QUEUE_LIMIT, format!("{} {} {}", nick, quote(&file), max));
                } else {
                    s.send(msg::UPLOAD_ACCEPT, format!("{} {}", nick, quote(&file)));
                }
            }
            msg::PUSH_ACK => {
                // we're firewalled: connect out to the downloader
                let nick = a.str().unwrap_or("").to_owned();
                let ip = a.str().and_then(ip::parse);
                let port: u16 = a.num().unwrap_or(0);
                let file = a.str().unwrap_or("").to_owned();
                if let Some(ip) = ip {
                    tokio::spawn(transfer::push_upload(s.clone(), nick, SocketAddr::from((ip, port)), file));
                }
            }
            msg::DATA_PORT_ERROR => {
                s.emit(Event::Info(format!("{} could not connect to your data port.", a.str().unwrap_or("?"))))
            }

            // ---- direct browse
            msg::DIRECT_BROWSE => {
                let requester = a.str().unwrap_or("").to_owned();
                let ip = a.str().and_then(ip::parse);
                let port: Option<u16> = a.num();
                if s.library().is_empty() {
                    return;
                }
                match (ip, port) {
                    (Some(ip), Some(port)) if port != 0 => {
                        tokio::spawn(transfer::push_list(s.clone(), SocketAddr::from((ip, port))));
                    }
                    _ => {
                        s.send(msg::DIRECT_BROWSE_ACCEPT, requester);
                    }
                }
            }
            msg::DIRECT_BROWSE_ACCEPT => {
                let nick = a.str().unwrap_or("").to_owned();
                let ip = a.str().and_then(ip::parse);
                let port: u16 = a.num().unwrap_or(0);
                match ip {
                    Some(ip) if port != 0 => {
                        tokio::spawn(transfer::fetch_list(s.clone(), nick, SocketAddr::from((ip, port))));
                    }
                    _ => {
                        // they'll push it to us (SENDLIST) if they can
                    }
                }
            }
            msg::DIRECT_BROWSE_ERROR => {
                let nick = a.str().unwrap_or("").to_owned();
                let why = a.str().unwrap_or("").to_owned();
                if lock(&s.pending_browses).contains(&nick.to_lowercase()) {
                    s.emit(Event::Info(format!(
                        "Direct browse failed ({why}). Requesting a Server list for user: {nick} ..."
                    )));
                    s.send(msg::BROWSE, nick);
                }
            }

            // ---- channels
            msg::JOIN_ACK => {
                let ch = m.data.trim().to_owned();
                self.channel_users.insert(ch.to_lowercase(), Vec::new());
                s.emit(Event::Joined { channel: ch });
            }
            msg::PART_CHANNEL => s.emit(Event::Parted { channel: m.data.trim().to_owned() }),
            msg::CHANNEL_USER | msg::CHANNEL_USER_ENTRY => {
                let ch = a.str().unwrap_or("").to_owned();
                if let Some(user) = channel_user(&mut a) {
                    self.channel_users.entry(ch.to_lowercase()).or_default().push(user);
                }
            }
            msg::CHANNEL_USER_END | msg::LIST_CHANNEL_USERS => {
                let ch = m.data.trim().to_owned();
                let users = self.channel_users.remove(&ch.to_lowercase()).unwrap_or_default();
                s.emit(Event::ChannelUsers { channel: ch, users });
            }
            msg::CHANNEL_JOIN => {
                let channel = a.str().unwrap_or("").to_owned();
                if let Some(user) = channel_user(&mut a) {
                    s.emit(Event::UserJoined { channel, user });
                }
            }
            msg::CHANNEL_PART => {
                let channel = a.str().unwrap_or("").to_owned();
                let nick = a.str().unwrap_or("").to_owned();
                s.emit(Event::UserParted { channel, nick });
            }
            msg::CHANNEL_MESSAGE => {
                let channel = a.str().unwrap_or("").to_owned();
                let nick = a.str().unwrap_or("").to_owned();
                let text = a.rest().to_owned();
                s.emit(Event::ChannelMessage { channel, nick, text });
            }
            msg::EMOTE => {
                let channel = a.str().unwrap_or("").to_owned();
                let nick = a.str().unwrap_or("").to_owned();
                let text = a.str().unwrap_or("").to_owned();
                s.emit(Event::ChannelEmote { channel, nick, text });
            }
            msg::CHANNEL_TOPIC => {
                let channel = a.str().unwrap_or("").to_owned();
                let topic = a.rest().to_owned();
                s.emit(Event::Topic { channel, topic });
            }
            msg::CHANNEL_LIST_ENTRY => {
                let name = a.str().unwrap_or("").to_owned();
                let users = a.num().unwrap_or(0);
                let topic = a.rest().to_owned();
                self.channel_list.push((name, users, topic));
            }
            msg::LIST_CHANNELS => {
                s.emit(Event::ChannelList(std::mem::take(&mut self.channel_list)));
            }
            msg::CHANNEL_INVITE => {
                let from = a.str().unwrap_or("").to_owned();
                let channel = a.str().unwrap_or("").to_owned();
                s.emit(Event::ChannelInvite { from, channel });
            }

            // ---- users
            msg::WHOIS_RESPONSE => s.emit(Event::Whois(parse_whois(&m.data))),
            msg::WHOWAS => {
                let nick = a.str().unwrap_or("").to_owned();
                let level = a.str().unwrap_or("").to_owned();
                let last: u64 = a.num().unwrap_or(0);
                s.emit(Event::Whois(vec![
                    ("User".into(), nick),
                    ("Status".into(), "OFFLINE".into()),
                    ("User Level".into(), level),
                    ("Last Online".into(), format_ago(last)),
                ]));
            }
            msg::LINK_SPEED => s.emit(Event::Info(format!(
                "{}'s link speed: {}",
                a.str().unwrap_or("?"),
                napster_proto::link_speed_name(a.num().unwrap_or(0))
            ))),
            msg::PING => {
                // someone pinged us; answer so they see a pong
                let from = a.str().unwrap_or("").to_owned();
                s.send(msg::PONG, from);
            }
            msg::PONG => s.emit(Event::Pong { nick: a.str().unwrap_or("").to_owned() }),
            msg::SERVER_PING => {
                if let Ok(sent) = m.data.trim().parse::<u64>() {
                    s.emit(Event::ServerPong { millis: now_millis().saturating_sub(sent) });
                }
            }
            msg::IGNORE_ADD => s.emit(Event::Info(format!("Ignoring user {}'s channel and private messages!", m.data))),
            msg::IGNORE_REMOVE => s.emit(Event::Info(format!("Unignored user {}.", m.data))),
            msg::IGNORE_ALREADY => s.emit(Event::Info(format!("User {} is already on your ignore list!", m.data))),
            msg::IGNORE_NOT_IGNORED => s.emit(Event::Info(format!("User {} is not ignored.", m.data))),
            msg::OP_SAY => {
                let from = a.str().unwrap_or("").to_owned();
                s.emit(Event::Announcement { from: format!("[ops] {from}"), text: a.rest().to_owned() });
            }
            msg::ANNOUNCE => {
                let from = a.str().unwrap_or("").to_owned();
                s.emit(Event::Announcement { from, text: a.rest().to_owned() });
            }
            msg::LOGIN_ATTEMPT => s.emit(Event::Info("Someone attempted to log in with your user name.".into())),
            msg::SET_DATA_PORT => s.emit(Event::Info(format!("Server requested data port {}", m.data))),
            // Deliberately ignored: auto-upgrade (5) downloaded and ran an
            // exe, and connection/listen tests (900/901) make us dial out.
            msg::AUTO_UPGRADE | msg::CONNECTION_TEST | msg::LISTEN_TEST => {
                tracing::warn!("ignoring server request {}: {}", m.kind, m.data);
            }
            other => tracing::debug!("unhandled server message {other}: {}", m.data),
        }
    }

    /// 204 `<nick> <ip> <port> "<file>" <md5> <speed>`
    fn download_ack(&mut self, m: &Message) {
        let s = &self.shared;
        let mut a = m.args();
        let nick = a.str().unwrap_or("").to_owned();
        let ip = a.str().and_then(ip::parse);
        let port: u16 = a.num().unwrap_or(0);
        let file = a.str().unwrap_or("").to_owned();
        let Some(id) = s.find_download(&nick, &file, &[TransferState::Requesting]) else { return };
        let Some(ip) = ip else { return s.fail(id, "server sent a bad address") };
        if port == 0 {
            if *lock(&s.data_port) == 0 {
                s.fail(id, "both you and the other user are firewalled");
                s.pump_downloads();
                return;
            }
            lock(&s.pending_pushes).insert((nick.to_lowercase(), file.clone()), id);
            s.update(id, |t| t.state = TransferState::WaitingForPush);
            s.send(msg::PUSH_REQUEST, format!("{} {}", nick, quote(&file)));
            let s2 = s.clone();
            tokio::spawn(async move {
                tokio::time::sleep(PUSH_WAIT).await;
                if s2.transfer(id).is_some_and(|t| t.state == TransferState::WaitingForPush) {
                    lock(&s2.pending_pushes).retain(|_, v| *v != id);
                    s2.fail(id, "Timed-out waiting for data through firewall");
                    s2.pump_downloads();
                }
            });
        } else {
            tokio::spawn(transfer::download(s.clone(), id, nick, SocketAddr::from((ip, port)), file));
        }
    }
}

async fn next_msg(reader: &mut Option<ServerReader>) -> Option<Result<Message, napster_proto::codec::CodecError>> {
    match reader {
        Some(r) => r.next().await,
        None => std::future::pending().await,
    }
}

fn share_all(s: &Shared, lib: &Library) {
    for f in lib.files() {
        s.send(msg::SHARE_FILE, f.info.to_share());
    }
    s.emit(Event::Status(format!("Online ({}): Sharing {} files.", s.my_nick(), lib.len())));
}

fn channel_user(a: &mut napster_proto::Args<'_>) -> Option<ChannelUser> {
    Some(ChannelUser { nick: a.str()?.to_owned(), shared: a.num().unwrap_or(0), link_speed: a.num().unwrap_or(0) })
}

/// Ask a metaserver for a server: it answers `ip:port` or `WAIT <secs>`.
/// `host:port` or `[v6]:port` -> (host, port)
pub(crate) fn split_host_port(s: &str) -> anyhow::Result<(String, u16)> {
    let (host, port) =
        s.trim().rsplit_once(':').ok_or_else(|| anyhow!("Server address {s} needs a port (host:port)"))?;
    let port = port.parse().map_err(|_| anyhow!("Bad port in server address {s}"))?;
    Ok((host.trim_start_matches('[').trim_end_matches(']').to_owned(), port))
}

/// Connect directly or through the proxy. Returns the stream and the
/// server's IP when we know it (the proxy resolves names itself).
async fn dial(
    proxy: Option<&crate::proxy::ProxyConfig>,
    host: &str,
    port: u16,
) -> anyhow::Result<(TcpStream, Option<std::net::IpAddr>)> {
    let tcp = match proxy {
        Some(p) => {
            let s = timeout(CONNECT_TIMEOUT, crate::proxy::connect(p, host, port))
                .await
                .map_err(|_| anyhow!("timed out connecting through the proxy"))??;
            return Ok((s, host.parse().ok()));
        }
        None => {
            let addr = tokio::net::lookup_host((host, port))
                .await
                .with_context(|| format!("Unable to resolve Server address {host}!"))?
                .next()
                .ok_or_else(|| anyhow!("Unable to resolve Server address {host}!"))?;
            let s = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await.map_err(|_| anyhow!("timed out"))??;
            (s, Some(addr.ip()))
        }
    };
    tcp.0.set_nodelay(true)?;
    Ok(tcp)
}

async fn resolve_metaserver(meta: &str, proxy: Option<&crate::proxy::ProxyConfig>) -> anyhow::Result<(String, u16)> {
    let (meta_host, meta_port) = split_host_port(meta)?;
    for attempt in 0..5 {
        let (mut stream, _) =
            dial(proxy, &meta_host, meta_port).await.context("Failed opening socket to an optimal Server...")?;
        let mut buf = vec![0u8; 99];
        let n =
            timeout(CONNECT_TIMEOUT, stream.read(&mut buf)).await.map_err(|_| anyhow!("metaserver timed out"))??;
        let reply = String::from_utf8_lossy(&buf[..n]).trim().to_owned();
        if let Some(wait) = reply.strip_prefix("WAIT ") {
            let secs: u64 = wait.trim().parse().unwrap_or(5);
            tracing::info!("server busy, waiting {secs}s ({attempt})");
            tokio::time::sleep(Duration::from_secs(secs.min(60))).await;
            continue;
        }
        return split_host_port(&reply).context("Failed resolving an optimal Server...");
    }
    bail!("Server busy, try again later")
}

fn parse_whois(data: &str) -> Vec<(String, String)> {
    let mut a = napster_proto::Args::new(data);
    let mut out = Vec::new();
    let mut push = |k: &str, v: Option<String>| {
        if let Some(v) = v {
            out.push((k.to_owned(), v));
        }
    };
    push("User", a.str().map(String::from));
    push("User Level", a.str().map(String::from));
    push("Time Connected", a.num::<u64>().map(format_duration));
    push("Channels", a.str().map(String::from));
    push("Status", a.str().map(String::from));
    push("Number Shared", a.str().map(String::from));
    let downloads = a.str().map(String::from);
    let uploads = a.str().map(String::from);
    if let (Some(d), Some(u)) = (downloads, uploads) {
        push("Current Transfers", Some(format!("{d} Incoming Transfers, {u} Outgoing Transfers")));
    }
    push("Connection", a.num::<u32>().map(|n| napster_proto::link_speed_name(n).to_owned()));
    push("Client Version", a.str().map(String::from));
    push("Total Downloads", a.str().map(String::from));
    push("Total Uploads", a.str().map(String::from));
    push("Address", a.str().map(String::from));
    push("Connecting Port", a.str().map(String::from));
    push("Data Port", a.str().map(String::from));
    push("Email", a.str().map(String::from));
    out
}

fn format_duration(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn format_ago(epoch: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if epoch == 0 || epoch > now {
        return "unknown".into();
    }
    format!("{} ago", format_duration(now - epoch))
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Automatic update checks run at most once a day (manual ones always run).
const AUTO_CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

fn last_check_path() -> std::path::PathBuf {
    crate::ClientConfig::default_path().with_file_name("last_update_check")
}

fn auto_check_due() -> bool {
    let last = std::fs::read_to_string(last_check_path()).ok().and_then(|s| s.trim().parse::<u64>().ok());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    last.is_none_or(|t| now.saturating_sub(t) >= AUTO_CHECK_EVERY.as_secs())
}

fn mark_checked() {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let p = last_check_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, now.to_string());
}

/// Loopback, private (RFC 1918 / unique-local), link-local or CGNAT.
pub(crate) fn is_local_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| is_local_ip(std::net::IpAddr::V4(v4)))
        }
    }
}
