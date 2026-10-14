//! Server-to-server linking: several napd servers acting as one network.
//!
//! Each server links directly to every other one (a full mesh); nothing is
//! relayed, so there are no loops. A link is a TLS connection whose
//! certificate must match the fingerprint configured for that peer, and
//! both sides then prove knowledge of the shared link password with HMAC
//! challenge/response (the password itself never crosses the wire).
//!
//! What's shared: who is online (nicks are unique network-wide), search,
//! downloads (incl. firewalled push), browsing, private messages, pings,
//! whois, hot-list notices, chat channels, and network-wide stats.
//! Accounts, bans and moderation stay per server.
//!
//! Link messages use the normal frame format with types 10000 and up.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use futures::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use napster_proto::{ip, msg, quote, Args, Message, NapCodec, SearchQuery, SearchResult};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::unbounded_channel;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::codec::Framed;

use crate::state::{Level, State};
use crate::Shared;

pub const HELLO: u16 = 10000;
pub const AUTH: u16 = 10001;
/// `<nick> <ip> <data-port> <speed> <shared> <level> <login-ms> "<client>"`
pub const USER_ON: u16 = 10010;
/// `<nick>`
pub const USER_OFF: u16 = 10011;
/// `<nick> <shared>`
pub const USER_SHARES: u16 = 10012;
/// `<to-nick> <type> <data>`: deliver a client message to a local user.
pub const DELIVER: u16 = 10020;
/// `<from-nick> <type> <data>`: a remote user's request about a local user.
pub const FORWARD: u16 = 10021;
/// `<id> <max> <query>`
pub const SEARCH: u16 = 10030;
/// `<id> <201 payload>`
pub const SEARCH_RESULT: u16 = 10031;
/// `<id>`
pub const SEARCH_END: u16 = 10032;
/// `<channel> <nick>`
pub const CHAN_JOIN: u16 = 10040;
pub const CHAN_PART: u16 = 10041;
/// `<channel> <nick> <text>`
pub const CHAN_SAY: u16 = 10042;
/// `<channel> <nick> "<text>"`
pub const CHAN_EMOTE: u16 = 10043;
/// `<channel> <topic>`
pub const CHAN_TOPIC: u16 = 10044;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY: Duration = Duration::from_secs(10);
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(5);

static NEXT_LINK: AtomicU64 = AtomicU64::new(1);
static NEXT_SEARCH: AtomicU64 = AtomicU64::new(1);

/// A configured outgoing link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPeer {
    pub host: String,
    pub port: u16,
    /// SHA-256 fingerprint of the peer's TLS certificate (`AB:CD:...`).
    pub fingerprint: String,
}

impl std::str::FromStr for LinkPeer {
    type Err = anyhow::Error;
    /// `host:port=FINGERPRINT`
    fn from_str(s: &str) -> anyhow::Result<Self> {
        let (addr, fp) = s.split_once('=').context("expected host:port=FINGERPRINT")?;
        let (host, port) = addr.rsplit_once(':').context("expected host:port=FINGERPRINT")?;
        let fp = fp.trim().to_ascii_uppercase();
        if fp.split(':').count() != 32 {
            bail!("the fingerprint should be 32 hex bytes separated by colons (as napd prints it)");
        }
        Ok(Self { host: host.trim_matches(['[', ']']).to_owned(), port: port.parse()?, fingerprint: fp })
    }
}

/// A user logged in on another server.
#[derive(Debug, Clone)]
pub struct RemoteUser {
    pub nick: String,
    pub link: u64,
    pub server: String,
    pub ip: Ipv4Addr,
    pub data_port: u16,
    pub speed: u32,
    pub shared: u32,
    pub level: Level,
    pub login_ms: u64,
    pub client: String,
}

/// An established link, as seen from the state.
pub struct LinkHandle {
    pub name: String,
    pub tx: tokio::sync::mpsc::UnboundedSender<Message>,
}

/// A search waiting on other servers.
pub struct PendingSearch {
    pub origin: String,
    pub remaining: usize,
    pub waiting: HashSet<u64>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl State {
    /// Send to every link.
    pub fn link_broadcast(&self, m: &Message) {
        for l in self.links.values() {
            let _ = l.tx.send(m.clone());
        }
    }

    pub fn remote_user(&self, nick: &str) -> Option<&RemoteUser> {
        self.remote.get(&nick.to_lowercase())
    }

    fn link_send(&self, link: u64, m: Message) -> bool {
        self.links.get(&link).is_some_and(|l| l.tx.send(m).is_ok())
    }

    /// Deliver a client message to `nick` wherever on the network they are.
    pub fn deliver(&self, nick: &str, m: Message) -> bool {
        if self.send(nick, m.clone()) {
            return true;
        }
        match self.remote_user(nick) {
            Some(r) => self.link_send(r.link, Message::new(DELIVER, format!("{} {} {}", r.nick, m.kind, m.data))),
            None => false,
        }
    }

    /// Hand a request about remote user `target` to their server.
    pub fn forward(&self, target: &str, from: &str, m: &Message) -> bool {
        match self.remote_user(target) {
            Some(r) => self.link_send(r.link, Message::new(FORWARD, format!("{} {} {}", from, m.kind, m.data))),
            None => false,
        }
    }

    /// `<channel> <nick> <shared> <speed>` for a local or remote user.
    pub fn channel_entry_for(&self, nick_key: &str, channel: &str) -> Option<String> {
        if let Some(s) = self.sessions.get(nick_key) {
            return Some(s.channel_entry(channel));
        }
        self.remote.get(nick_key).map(|r| format!("{} {} {} {}", channel, r.nick, r.shared, r.speed))
    }

    /// Announce a local user to every link.
    pub fn announce_user(&self, nick: &str) {
        if let Some(s) = self.session(nick) {
            self.link_broadcast(&user_on(s));
        }
    }

    /// Finish a search waiting on other servers.
    pub fn finish_search(&mut self, id: u64) {
        if let Some(p) = self.searches.remove(&id) {
            self.send(&p.origin, Message::empty(msg::SEARCH_END));
        }
    }
}

fn user_on(s: &crate::state::Session) -> Message {
    Message::new(
        USER_ON,
        format!(
            "{} {} {} {} {} {} {} {}",
            s.nick,
            ip::encode(s.ip),
            s.data_port,
            s.link_speed,
            s.files.len(),
            s.level as u8,
            s.login_ms,
            quote(&s.client)
        ),
    )
}

fn level_from(n: u8) -> Level {
    match n {
        0 => Level::Leech,
        2 => Level::Moderator,
        3 => Level::Admin,
        4 => Level::Elite,
        _ => Level::User,
    }
}

// ------------------------------------------------------------ handshake

type HmacSha256 = Hmac<Sha256>;

fn proof(secret: &str, nonce: &str, name: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(format!("napster-link|{nonce}|{name}").as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn verify_proof(secret: &str, nonce: &str, name: &str, hex: &str) -> bool {
    let Ok(bytes) = (0..hex.len())
        .step_by(2)
        .map(|i| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(()))
        .collect::<Result<Vec<u8>, ()>>()
    else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(format!("napster-link|{nonce}|{name}").as_bytes());
    mac.verify_slice(&bytes).is_ok()
}

fn nonce() -> String {
    format!("{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>())
}

async fn recv<S: AsyncRead + AsyncWrite + Unpin>(f: &mut Framed<S, NapCodec>, kind: u16) -> anyhow::Result<Message> {
    let m = tokio::time::timeout(HANDSHAKE_TIMEOUT, f.next())
        .await
        .map_err(|_| anyhow!("link handshake timed out"))?
        .ok_or_else(|| anyhow!("peer closed the link during the handshake"))??;
    if m.kind != kind {
        bail!("unexpected message {} during link handshake", m.kind);
    }
    Ok(m)
}

/// Run the handshake as the side that accepted the connection. Returns the
/// peer's server name.
async fn handshake_accept<S: AsyncRead + AsyncWrite + Unpin>(
    f: &mut Framed<S, NapCodec>,
    my_name: &str,
    secret: &str,
) -> anyhow::Result<String> {
    let mine = nonce();
    f.send(Message::new(HELLO, format!("{my_name} {mine}"))).await?;
    let hello = recv(f, HELLO).await?;
    let mut a = hello.args();
    let (peer, theirs) = (a.str().unwrap_or("").to_owned(), a.str().unwrap_or("").to_owned());
    let auth = recv(f, AUTH).await?;
    if !verify_proof(secret, &mine, &peer, auth.data.trim()) {
        bail!("{peer} failed link authentication (wrong link password?)");
    }
    f.send(Message::new(AUTH, proof(secret, &theirs, my_name))).await?;
    Ok(peer)
}

/// Run the handshake as the side that connected.
async fn handshake_connect<S: AsyncRead + AsyncWrite + Unpin>(
    f: &mut Framed<S, NapCodec>,
    my_name: &str,
    secret: &str,
) -> anyhow::Result<String> {
    let hello = recv(f, HELLO).await?;
    let mut a = hello.args();
    let (peer, theirs) = (a.str().unwrap_or("").to_owned(), a.str().unwrap_or("").to_owned());
    let mine = nonce();
    f.send(Message::new(HELLO, format!("{my_name} {mine}"))).await?;
    f.send(Message::new(AUTH, proof(secret, &theirs, my_name))).await?;
    let auth = recv(f, AUTH).await?;
    if !verify_proof(secret, &mine, &peer, auth.data.trim()) {
        bail!("{peer} failed link authentication (wrong link password?)");
    }
    Ok(peer)
}

// ------------------------------------------------------------- listeners

pub(crate) async fn accept_loop(shared: Arc<Shared>, listener: TcpListener, acceptor: TlsAcceptor) {
    loop {
        let Ok((tcp, peer)) = listener.accept().await else { continue };
        let (shared, acceptor) = (shared.clone(), acceptor.clone());
        tokio::spawn(async move {
            let r: anyhow::Result<()> = async {
                let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await??;
                let mut f = Framed::new(tls, NapCodec);
                let secret = shared.config.link_password.clone().context("no link password configured")?;
                let name = handshake_accept(&mut f, &shared.config.server_name, &secret).await?;
                run(shared, f, name).await
            }
            .await;
            if let Err(e) = r {
                tracing::warn!("link from {peer}: {e:#}");
            }
        });
    }
}

pub(crate) async fn connect_loop(shared: Arc<Shared>, peer: LinkPeer) {
    loop {
        let r: anyhow::Result<()> = async {
            let tcp =
                tokio::time::timeout(HANDSHAKE_TIMEOUT, TcpStream::connect((peer.host.as_str(), peer.port))).await??;
            tcp.set_nodelay(true)?;
            let connector = TlsConnector::from(crate::tls::pinned_client_config(&peer.fingerprint)?);
            let name = rustls::pki_types::ServerName::try_from(peer.host.clone())
                .map_err(|_| anyhow!("bad host name {}", peer.host))?;
            let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, connector.connect(name, tcp))
                .await?
                .with_context(|| format!("TLS to {}:{} (is the fingerprint right?)", peer.host, peer.port))?;
            let mut f = Framed::new(tls, NapCodec);
            let secret = shared.config.link_password.clone().context("no link password configured")?;
            let name = handshake_connect(&mut f, &shared.config.server_name, &secret).await?;
            run(shared.clone(), f, name).await
        }
        .await;
        match r {
            Ok(()) => tracing::info!("link to {}:{} closed; reconnecting", peer.host, peer.port),
            Err(e) => tracing::info!("link to {}:{}: {e:#}; retrying in {}s", peer.host, peer.port, RETRY.as_secs()),
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// An authenticated link: register it, sync, then process messages.
async fn run<S>(shared: Arc<Shared>, f: Framed<S, NapCodec>, name: String) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if name.is_empty() || name.eq_ignore_ascii_case(&shared.config.server_name) {
        bail!("peer uses our own server name ({name}); every server needs a unique --server-name");
    }
    let id = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = unbounded_channel::<Message>();
    let (mut sink, mut stream) = f.split();
    {
        let mut st = shared.lock();
        if st.links.values().any(|l| l.name.eq_ignore_ascii_case(&name)) {
            bail!("already linked to {name}");
        }
        // our view of the world for the new peer
        for s in st.sessions.values() {
            let _ = tx.send(user_on(s));
        }
        for ch in st.channels.values() {
            for m in ch.members.iter().filter(|m| st.sessions.contains_key(*m)) {
                let nick = &st.sessions[m].nick;
                let _ = tx.send(Message::new(CHAN_JOIN, format!("{} {}", ch.name, nick)));
            }
            if !ch.permanent && ch.members.iter().any(|m| st.sessions.contains_key(m)) {
                let _ = tx.send(Message::new(CHAN_TOPIC, format!("{} {}", ch.name, ch.topic)));
            }
        }
        st.links.insert(id, LinkHandle { name: name.clone(), tx });
    }
    tracing::info!("linked with server {name}");
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });
    let result = async {
        while let Some(m) = stream.next().await {
            handle(&shared, id, &name, m?);
        }
        Ok(())
    }
    .await;
    writer.abort();
    unlink(&shared, id, &name);
    tracing::info!("unlinked from server {name}");
    result
}

/// Forget everything a dropped link told us.
fn unlink(shared: &Shared, id: u64, name: &str) {
    let mut st = shared.lock();
    st.links.remove(&id);
    let gone: Vec<String> = st.remote.iter().filter(|(_, r)| r.link == id).map(|(k, _)| k.clone()).collect();
    for key in gone {
        remove_remote(&mut st, &key);
    }
    let finished: Vec<u64> = st
        .searches
        .iter_mut()
        .filter_map(|(sid, p)| (p.waiting.remove(&id) && p.waiting.is_empty()).then_some(*sid))
        .collect();
    for sid in finished {
        st.finish_search(sid);
    }
    tracing::debug!("cleaned up after link {name}");
}

/// A remote user left (or their server went away).
fn remove_remote(st: &mut State, key: &str) {
    let Some(r) = st.remote.remove(key) else { return };
    let channels: Vec<String> =
        st.channels.iter().filter(|(_, c)| c.members.contains(&key.to_owned())).map(|(k, _)| k.clone()).collect();
    for ch in channels {
        remote_part(st, &ch, key);
    }
    let off = Message::new(msg::USER_SIGNOFF, r.nick.clone());
    for s in st.sessions.values() {
        if s.hotlist.contains(key) {
            s.send(off.clone());
        }
    }
}

fn remote_part(st: &mut State, ch_key: &str, nick_key: &str) {
    let Some(ch) = st.channels.get_mut(ch_key) else { return };
    let Some(pos) = ch.members.iter().position(|m| m == nick_key) else { return };
    ch.members.remove(pos);
    let name = ch.name.clone();
    let empty = ch.members.is_empty() && !ch.permanent;
    let nick = st.remote.get(nick_key).map(|r| (r.nick.clone(), r.shared, r.speed));
    let entry = match nick {
        Some((n, shared, speed)) => format!("{name} {n} {shared} {speed}"),
        None => format!("{name} {nick_key} 0 0"),
    };
    st.broadcast_channel(&name, None, &Message::new(msg::CHANNEL_PART, entry));
    if empty {
        st.channels.remove(ch_key);
    }
}

// --------------------------------------------------------------- handlers

fn handle(shared: &Shared, link: u64, server: &str, m: Message) {
    let mut a = m.args();
    match m.kind {
        USER_ON => {
            let Some(nick) = a.str().map(String::from) else { return };
            let ip = a.str().and_then(ip::parse).unwrap_or(Ipv4Addr::UNSPECIFIED);
            let data_port = a.num().unwrap_or(0);
            let speed = a.num().unwrap_or(0);
            let shared_n = a.num().unwrap_or(0);
            let level = level_from(a.num().unwrap_or(1));
            let login_ms = a.num().unwrap_or(0);
            let client = a.str().unwrap_or("").to_owned();
            let user = RemoteUser {
                nick: nick.clone(),
                link,
                server: server.to_owned(),
                ip,
                data_port,
                speed,
                shared: shared_n,
                level,
                login_ms,
                client,
            };
            user_on_remote(shared, user);
        }
        USER_OFF => {
            let key = a.str().unwrap_or("").to_lowercase();
            let mut st = shared.lock();
            if st.remote.get(&key).is_some_and(|r| r.link == link) {
                remove_remote(&mut st, &key);
            }
        }
        USER_SHARES => {
            let key = a.str().unwrap_or("").to_lowercase();
            let n: u32 = a.num().unwrap_or(0);
            if let Some(r) = shared.lock().remote.get_mut(&key).filter(|r| r.link == link) {
                r.shared = n;
            }
        }
        DELIVER => {
            let to = a.str().unwrap_or("").to_owned();
            let Some(kind) = a.num::<u16>() else { return };
            let data = a.rest().to_owned();
            let st = shared.lock();
            let Some(s) = st.session(&to) else { return };
            // the recipient's ignore list applies to private messages
            if kind == msg::PRIVATE_MESSAGE {
                let from = Args::new(&data).str().unwrap_or("").to_lowercase();
                if s.ignored.contains(&from) {
                    return;
                }
            }
            s.send(Message::new(kind, data));
        }
        FORWARD => {
            let from = a.str().unwrap_or("").to_owned();
            let Some(kind) = a.num::<u16>() else { return };
            let data = a.rest().to_owned();
            forwarded(shared, &from, kind, &data);
        }
        SEARCH => {
            let Some(id) = a.str().map(String::from) else { return };
            let max: usize = a.num().unwrap_or(0);
            let payload = a.rest().to_owned();
            let st = shared.lock();
            let reply = |m: Message| st.link_send(link, m);
            if let Ok(q) = SearchQuery::parse(&payload) {
                let words = q.words();
                let mut n = 0;
                'outer: for s in st.sessions.values() {
                    for f in &s.files {
                        if n >= max {
                            break 'outer;
                        }
                        if q.matches(&words, f, s.link_speed) {
                            let r = SearchResult {
                                file: f.clone(),
                                nick: s.nick.clone(),
                                ip: s.ip,
                                link_speed: s.link_speed,
                            };
                            reply(Message::new(SEARCH_RESULT, format!("{id} {}", r.to_payload())));
                            n += 1;
                        }
                    }
                }
            }
            reply(Message::new(SEARCH_END, id));
        }
        SEARCH_RESULT => {
            let Some(id) = a.num::<u64>() else { return };
            let payload = a.rest().to_owned();
            let mut st = shared.lock();
            let Some(p) = st.searches.get_mut(&id) else { return };
            if p.remaining > 0 {
                p.remaining -= 1;
                let origin = p.origin.clone();
                st.send(&origin, Message::new(msg::SEARCH_RESULT, payload));
            }
        }
        SEARCH_END => {
            let Some(id) = a.num::<u64>() else { return };
            let mut st = shared.lock();
            let done = st.searches.get_mut(&id).is_some_and(|p| {
                p.waiting.remove(&link);
                p.waiting.is_empty()
            });
            if done {
                st.finish_search(id);
            }
        }
        CHAN_JOIN => {
            let channel = a.str().unwrap_or("").to_owned();
            let key = a.str().unwrap_or("").to_lowercase();
            let mut st = shared.lock();
            if !st.remote.get(&key).is_some_and(|r| r.link == link) || channel.is_empty() {
                return;
            }
            let ch_key = channel.to_lowercase();
            let ch = st.channels.entry(ch_key.clone()).or_insert_with(|| crate::state::Channel::new(&channel, false));
            if ch.members.contains(&key) {
                return;
            }
            ch.members.push(key.clone());
            let name = ch.name.clone();
            if let Some(entry) = st.channel_entry_for(&key, &name) {
                st.broadcast_channel(&name, None, &Message::new(msg::CHANNEL_JOIN, entry));
            }
        }
        CHAN_PART => {
            let ch_key = a.str().unwrap_or("").to_lowercase();
            let key = a.str().unwrap_or("").to_lowercase();
            let mut st = shared.lock();
            if st.remote.get(&key).is_some_and(|r| r.link == link) {
                remote_part(&mut st, &ch_key, &key);
            }
        }
        CHAN_SAY | CHAN_EMOTE => {
            let channel = a.str().unwrap_or("").to_owned();
            let nick = a.str().unwrap_or("").to_owned();
            let text = a.rest().to_owned();
            let st = shared.lock();
            if !st.remote.get(&nick.to_lowercase()).is_some_and(|r| r.link == link) {
                return;
            }
            let Some(name) = st.channels.get(&channel.to_lowercase()).map(|c| c.name.clone()) else { return };
            let kind = if m.kind == CHAN_SAY { msg::CHANNEL_MESSAGE } else { msg::EMOTE };
            st.broadcast_channel(&name, Some(&nick), &Message::new(kind, format!("{name} {nick} {text}")));
        }
        CHAN_TOPIC => {
            let channel = a.str().unwrap_or("").to_owned();
            let topic = a.rest().to_owned();
            let mut st = shared.lock();
            if let Some(ch) = st.channels.get_mut(&channel.to_lowercase()) {
                ch.topic = topic;
                let out = Message::new(msg::CHANNEL_TOPIC, format!("{} {}", ch.name, ch.topic));
                let name = ch.name.clone();
                st.broadcast_channel(&name, None, &out);
            }
        }
        other => tracing::debug!("link {server}: unknown message {other}"),
    }
}

/// A user on `link`'s server logged in.
fn user_on_remote(shared: &Shared, user: RemoteUser) {
    let key = user.nick.to_lowercase();
    let mut st = shared.lock();
    // Nick collision (two servers accepted the same nick at once): the
    // earlier login wins, ties broken by server name. Both servers apply
    // the same rule, so they agree on who stays.
    let me = &shared.config.server_name;
    if let Some(local) = st.session(&key) {
        if (local.login_ms, me.as_str()) <= (user.login_ms, user.server.as_str()) {
            return;
        }
        local.send(Message::new(
            msg::SERVER_ERROR,
            format!("Disconnected: {} is also logged in on server {}.", user.nick, user.server),
        ));
        local.kill.cancel();
    }
    if let Some(other) = st.remote.get(&key) {
        if other.link != user.link && (other.login_ms, other.server.as_str()) <= (user.login_ms, user.server.as_str()) {
            return;
        }
    }
    let on = Message::new(msg::USER_SIGNON, format!("{} {}", user.nick, user.speed));
    st.remote.insert(key.clone(), user);
    for s in st.sessions.values() {
        if s.hotlist.contains(&key) {
            s.send(on.clone());
        }
    }
}

/// A remote user's request about one of our local users.
fn forwarded(shared: &Shared, from: &str, kind: u16, data: &str) {
    let st = shared.lock();
    let Some(req) = st.remote_user(from).cloned() else { return };
    let mut a = Args::new(data);
    let target = a.str().unwrap_or("").to_owned();
    match kind {
        msg::DOWNLOAD_REQUEST => {
            let file = a.str().unwrap_or("").to_owned();
            match st.session(&target) {
                Some(up) if up.files.iter().any(|f| f.filename == file) => {
                    up.send(Message::new(msg::UPLOAD_REQUEST, format!("{} {} {}", req.nick, quote(&file), req.speed)));
                }
                _ => {
                    st.deliver(&req.nick, Message::new(msg::DOWNLOAD_ERROR, format!("{} {}", target, quote(&file))));
                }
            }
        }
        msg::PUSH_REQUEST => {
            let file = a.str().unwrap_or("").to_owned();
            let found =
                st.session(&target).and_then(|up| up.files.iter().find(|f| f.filename == file).map(|f| (up, f)));
            match found {
                Some(_) if req.data_port == 0 => {
                    st.deliver(
                        &req.nick,
                        Message::new(
                            msg::SERVER_ERROR,
                            format!("Unable to download from {target}: you are both behind firewalls (data port 0)."),
                        ),
                    );
                }
                Some((up, f)) => up.send(Message::new(
                    msg::PUSH_ACK,
                    format!(
                        "{} {} {} {} {} {}",
                        req.nick,
                        ip::encode(req.ip),
                        req.data_port,
                        quote(&f.filename),
                        f.md5,
                        req.speed
                    ),
                )),
                None => {
                    st.deliver(&req.nick, Message::new(msg::DOWNLOAD_ERROR, format!("{} {}", target, quote(&file))));
                }
            }
        }
        msg::BROWSE => match st.session(&target) {
            Some(s) => {
                for f in &s.files {
                    st.deliver(&req.nick, Message::new(msg::BROWSE_RESULT, format!("{} {}", s.nick, f.to_share())));
                }
                st.deliver(&req.nick, Message::new(msg::BROWSE_END, format!("{} {}", s.nick, ip::encode(s.ip))));
            }
            None => {
                st.deliver(&req.nick, Message::new(msg::USER_SIGNOFF, target));
            }
        },
        msg::DIRECT_BROWSE => {
            let fail = |why: &str| {
                st.deliver(&req.nick, Message::new(msg::DIRECT_BROWSE_ERROR, format!("{} {}", target, quote(why))));
            };
            let Some(t) = st.session(&target) else { return fail("User is not currently online.") };
            if t.files.is_empty() {
                return fail("User is not sharing any files.");
            }
            if t.data_port == 0 && req.data_port == 0 {
                return fail("Both you and the user are firewalled.");
            }
            let data = if t.data_port == 0 {
                format!("{} {} {}", req.nick, ip::encode(req.ip), req.data_port)
            } else {
                req.nick.clone()
            };
            t.send(Message::new(msg::DIRECT_BROWSE, data));
        }
        other => tracing::debug!("unexpected forwarded message {other} from {from}"),
    }
}

/// Fan a search out to every linked server. Returns false if there's
/// nobody to ask (so the caller ends the search itself).
pub(crate) fn start_search(
    shared: &Arc<Shared>,
    st: &mut State,
    origin: &str,
    remaining: usize,
    payload: &str,
) -> bool {
    if st.links.is_empty() || remaining == 0 {
        return false;
    }
    let id = NEXT_SEARCH.fetch_add(1, Ordering::Relaxed);
    st.searches.insert(
        id,
        PendingSearch { origin: origin.to_lowercase(), remaining, waiting: st.links.keys().copied().collect() },
    );
    st.link_broadcast(&Message::new(SEARCH, format!("{id} {remaining} {payload}")));
    let shared = shared.clone();
    tokio::spawn(async move {
        tokio::time::sleep(SEARCH_TIMEOUT).await;
        shared.lock().finish_search(id);
    });
    true
}

/// Keep other servers' view of our users' share counts current.
pub(crate) async fn share_sync_loop(shared: Arc<Shared>) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        let mut st = shared.lock();
        if st.links.is_empty() {
            continue;
        }
        let mut updates = vec![];
        for s in st.sessions.values_mut() {
            let n = s.files.len() as u32;
            if n != s.announced_shares {
                s.announced_shares = n;
                updates.push(Message::new(USER_SHARES, format!("{} {n}", s.nick)));
            }
        }
        for m in &updates {
            st.link_broadcast(m);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_proofs() {
        let p = proof("secret", "n1", "alpha");
        assert!(verify_proof("secret", "n1", "alpha", &p));
        assert!(!verify_proof("wrong", "n1", "alpha", &p));
        assert!(!verify_proof("secret", "n2", "alpha", &p));
        assert!(!verify_proof("secret", "n1", "beta", &p));
        assert!(!verify_proof("secret", "n1", "alpha", "zz"));
    }

    #[test]
    fn parse_peer() {
        let fp = (0..32).map(|i| format!("{i:02x}")).collect::<Vec<_>>().join(":");
        let p: LinkPeer = format!("example.org:8890={fp}").parse().unwrap();
        assert_eq!((p.host.as_str(), p.port), ("example.org", 8890));
        assert_eq!(p.fingerprint, fp.to_uppercase());
        assert!("example.org:8890".parse::<LinkPeer>().is_err());
        assert!("example.org:8890=AB:CD".parse::<LinkPeer>().is_err());
    }
}
