//! One client connection: login, then message dispatch.

use std::collections::{BTreeSet, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::bail;
use futures::{SinkExt, StreamExt};
use napster_proto::codec::CodecError;
use napster_proto::{ip, msg, quote, valid_nick, Message, NapCodec, SearchQuery, SearchResult};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;

use crate::db::{now_secs, write_snapshot, Account, Ban};
use crate::guard::ConnSlot;
use crate::state::{Channel, Level, Session, State};
use crate::{Shared, VERSION};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(60);
const LINGER: Duration = Duration::from_secs(2);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Serve one connection (plain TCP or TLS). `_slot` holds this address's
/// connection-limit slot until we return.
pub(crate) async fn handle<S>(shared: Arc<Shared>, stream: S, peer: SocketAddr, _slot: ConnSlot) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let ip = match peer.ip() {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(v6) => v6.to_ipv4_mapped().unwrap_or(Ipv4Addr::LOCALHOST),
    };
    let (r, w) = tokio::io::split(stream);
    let mut reader = FramedRead::new(r, NapCodec);
    let mut writer = FramedWrite::new(w, NapCodec);
    let (tx, mut rx) = unbounded_channel::<Message>();
    let writer_task = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            tracing::trace!(">> {} {} {:?}", m.kind, msg::name(m.kind), m.data);
            if writer.send(m).await.is_err() {
                break;
            }
        }
        let _ = writer.close().await;
    });

    let mut conn = Conn {
        shared,
        tx,
        kill: CancellationToken::new(),
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        ip,
        remote_port: peer.port(),
        nick: None,
        tokens: 0.0,
        refilled: Instant::now(),
        dropped: 0,
        warned: None,
        share_warned: false,
    };
    conn.tokens = conn.shared.config.msg_burst;
    tracing::debug!("{peer}: connected");
    let result = conn.run(&mut reader).await;
    conn.cleanup();
    drop(conn);
    let _ = writer_task.await;
    // Lingering close: the writer has flushed and sent FIN. If we closed now
    // with unread input (a flooder, say), the OS would reset the connection
    // and Windows clients would lose our final messages, so read and
    // discard for a moment first.
    let _ = tokio::time::timeout(LINGER, async { while let Some(Ok(_)) = reader.next().await {} }).await;
    tracing::debug!("{peer}: disconnected");
    result
}

struct Conn {
    shared: Arc<Shared>,
    tx: UnboundedSender<Message>,
    kill: CancellationToken,
    id: u64,
    ip: Ipv4Addr,
    remote_port: u16,
    /// Set once logged in.
    nick: Option<String>,
    /// flood control: token bucket of messages
    tokens: f64,
    refilled: Instant,
    dropped: u32,
    warned: Option<Instant>,
    share_warned: bool,
}

/// Messages dropped for flooding before the connection is cut.
const FLOOD_DISCONNECT: u32 = 300;

fn err(text: impl Into<String>) -> Message {
    Message::new(msg::SERVER_ERROR, text)
}

impl Conn {
    fn reply(&self, kind: u16, data: impl Into<String>) {
        let _ = self.tx.send(Message::new(kind, data));
    }

    fn error(&self, text: impl Into<String>) {
        let _ = self.tx.send(err(text));
    }

    fn nick(&self) -> &str {
        self.nick.as_deref().unwrap_or("")
    }

    fn key(&self) -> String {
        self.nick().to_lowercase()
    }

    async fn run<R: AsyncRead + Unpin>(&mut self, reader: &mut FramedRead<R, NapCodec>) -> anyhow::Result<()> {
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        loop {
            let next = tokio::select! {
                _ = self.kill.cancelled() => return Ok(()),
                m = reader.next() => m,
                _ = tokio::time::sleep_until(deadline.into()), if self.nick.is_none() => {
                    bail!("login timed out");
                }
            };
            let m = match next {
                None => return Ok(()),
                Some(Err(CodecError::Io(e))) => return Err(e.into()),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(m)) => m,
            };
            tracing::trace!("<< {} {} {:?}", m.kind, msg::name(m.kind), m.data);
            if self.nick.is_none() {
                if !self.pre_login(m) {
                    // give the writer a chance to deliver the error
                    return Ok(());
                }
            } else if self.allow(m.kind) {
                self.dispatch(m);
            } else if self.dropped >= FLOOD_DISCONNECT {
                self.error("Disconnected for flooding.");
                bail!("flooding");
            }
        }
    }

    /// Flood control. Shares and transfer bookkeeping are exempt (a client
    /// sends thousands of 100s at login); everything else draws from a
    /// token bucket.
    fn allow(&mut self, kind: u16) -> bool {
        if matches!(
            kind,
            msg::SHARE_FILE
                | msg::UNSHARE_FILE
                | msg::UNSHARE_ALL
                | msg::SHARE_DIRECTORY
                | msg::HOTLIST_INIT
                | msg::DOWNLOAD_START
                | msg::DOWNLOAD_COMPLETE
                | msg::UPLOAD_START
                | msg::UPLOAD_COMPLETE
        ) {
            return true;
        }
        let cfg = &self.shared.config;
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.refilled).as_secs_f64() * cfg.msg_rate).min(cfg.msg_burst);
        self.refilled = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return true;
        }
        self.dropped += 1;
        if self.warned.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            self.warned = Some(now);
            self.error("You are sending messages too fast; some were ignored.");
        }
        false
    }

    // ---------------------------------------------------------------- login

    /// Returns false to drop the connection.
    fn pre_login(&mut self, m: Message) -> bool {
        match m.kind {
            msg::NICK_CHECK => {
                let nick = m.args().str().unwrap_or("").to_owned();
                let st = self.shared.lock();
                let kind = if !valid_nick(&nick) {
                    msg::NICK_INVALID
                } else if st.db.get(&nick).is_some() || st.session(&nick).is_some() {
                    msg::NICK_REGISTERED
                } else {
                    msg::NICK_NOT_REGISTERED
                };
                self.reply(kind, "");
                true
            }
            msg::PASSWORD_CHECK => {
                let mut a = m.args();
                let (nick, pass) = (a.str().unwrap_or(""), a.str().unwrap_or(""));
                let ip = IpAddr::V4(self.ip);
                if self.shared.guard().login_locked(ip) {
                    self.reply(msg::ERROR, "Too many failed logins from your address. Try again later.");
                    return false;
                }
                let account = self.shared.lock().db.get(nick).cloned();
                if account.is_none_or(|acc| acc.check_password(pass)) {
                    self.reply(msg::PASSWORD_OK, "");
                } else {
                    self.shared.guard().login_failed(ip);
                    self.reply(msg::ERROR, "invalid password");
                }
                true
            }
            msg::LOGIN | msg::NEW_USER_LOGIN => self.login(m),
            // version check, demographics, the b10 pre-login 920: nothing to do
            msg::VERSION_CHECK | msg::LOGIN_OPTIONS | msg::LOGIN_OPTIONS2 | msg::PRE_LOGIN => true,
            other => {
                tracing::debug!("ignoring {other} before login");
                true
            }
        }
    }

    fn login(&mut self, m: Message) -> bool {
        let new_user = m.kind == msg::NEW_USER_LOGIN;
        let mut a = m.args();
        let nick = a.str().unwrap_or("").to_owned();
        let pass = a.str().unwrap_or("").to_owned();
        let port: u16 = a.num().unwrap_or(0);
        let client = a.str().unwrap_or("unknown").to_owned();
        let speed: u32 = a.num().unwrap_or(0).min(10);
        let email = if new_user { a.str().unwrap_or("anon@napster.com").to_owned() } else { String::new() };

        if !valid_nick(&nick) {
            self.reply(msg::ERROR, "Invalid nickname");
            return false;
        }
        if pass.is_empty() {
            self.reply(msg::ERROR, "invalid password");
            return false;
        }
        if pass.len() > 64 {
            self.reply(msg::ERROR, "Password is too long");
            return false;
        }
        let key = nick.to_lowercase();
        let ip = IpAddr::V4(self.ip);
        if self.shared.guard().login_locked(ip) {
            self.reply(msg::ERROR, "Too many failed logins from your address. Try again later.");
            return false;
        }
        let existing = {
            let st = self.shared.lock();
            if let Some(r) = st.remote_user(&nick) {
                let server = r.server.clone();
                drop(st);
                self.reply(msg::ERROR, format!("{nick} is already logged in on server {server}."));
                return false;
            }
            if let Some(ban) = st.db.banned(&nick, &self.ip.to_string()) {
                let reason = ban.reason.clone();
                drop(st);
                self.reply(msg::ERROR, format!("You are banned from this server: {reason}"));
                return false;
            }
            st.db.get(&nick).cloned()
        };
        let elite = self.shared.config.elite.iter().any(|e| e.eq_ignore_ascii_case(&nick));
        // Password hashing is slow by design, so it happens without the lock.
        let fresh = match &existing {
            Some(acc) => {
                if new_user {
                    self.reply(msg::ERROR, format!("Nickname {nick} is already registered"));
                    return false;
                }
                if acc.nuked {
                    self.reply(msg::ERROR, "This account has been nuked");
                    return false;
                }
                if !acc.check_password(&pass) {
                    self.shared.guard().login_failed(ip);
                    self.reply(msg::ERROR, "invalid password");
                    return false;
                }
                self.shared.guard().login_ok(ip);
                // upgrade pre-v0.4 SHA-256 hashes now that we know the password
                acc.needs_rehash().then(|| {
                    let mut a = acc.clone();
                    a.set_password(&pass);
                    a
                })
            }
            None => {
                if !self.shared.guard().try_signup(ip) {
                    self.reply(msg::ERROR, "Too many new accounts from your address. Try again later.");
                    return false;
                }
                let email = if email.is_empty() { "anon@napster.com" } else { &email };
                let level = if elite { Level::Elite } else { Level::User };
                Some(Account::new(&nick, &pass, email, level))
            }
        };
        let mut st = self.shared.lock();
        match (st.db.get_mut(&nick), fresh) {
            // someone registered this nick while we were hashing
            (Some(_), Some(_)) if existing.is_none() => {
                drop(st);
                self.reply(msg::ERROR, format!("Nickname {nick} is already registered"));
                return false;
            }
            (Some(acc), Some(upgraded)) => acc.adopt_password(&upgraded),
            (None, Some(new)) => st.db.insert(new),
            (Some(_), None) => {}
            (None, None) => {
                // deleted while we were checking
                drop(st);
                self.reply(msg::ERROR, "invalid password");
                return false;
            }
        }
        if elite {
            if let Some(acc) = st.db.get_mut(&nick) {
                acc.level = Level::Elite;
            }
        }
        let acc = st.db.get(&nick).cloned().expect("just ensured");

        // Same nick already online: the password matched, so it's the
        // owner reconnecting. Tell the old session (748) and drop it.
        if let Some(old) = st.sessions.remove(&key) {
            old.send(Message::empty(msg::LOGIN_ATTEMPT));
            old.send(err("You have been disconnected: logged in from another location"));
            old.kill.cancel();
            for ch in old.channels.clone() {
                if let Some(c) = st.channels.get_mut(&ch) {
                    c.members.retain(|m| *m != key);
                }
                st.broadcast_channel(&ch, None, &Message::new(msg::CHANNEL_PART, old.channel_entry(&ch)));
            }
        }

        let session = Session {
            id: self.id,
            nick: acc.nick.clone(),
            tx: self.tx.clone(),
            kill: self.kill.clone(),
            ip: self.ip,
            remote_port: self.remote_port,
            data_port: port,
            link_speed: speed,
            client,
            level: acc.level,
            email: acc.email.clone(),
            files: Vec::new(),
            hotlist: HashSet::new(),
            ignored: BTreeSet::new(),
            channels: BTreeSet::new(),
            downloads: 0,
            uploads: 0,
            total_downloads: 0,
            total_uploads: 0,
            connected: Instant::now(),
            muzzled: false,
            cloaked: false,
            login_ms: crate::link::now_ms(),
            announced_shares: 0,
        };
        st.sessions.insert(key.clone(), session);
        self.nick = Some(acc.nick.clone());

        self.reply(msg::LOGIN_ACK, acc.email.clone());
        for line in &self.shared.config.motd {
            self.reply(msg::MOTD, line.clone());
        }
        self.reply(msg::SERVER_STATS, st.stats());
        st.announce_user(&acc.nick);
        let signon = Message::new(msg::USER_SIGNON, format!("{} {}", acc.nick, speed));
        for s in st.sessions.values() {
            if s.hotlist.contains(&key) {
                s.send(signon.clone());
            }
        }
        let snap = st.db.snapshot();
        drop(st);
        write_snapshot(snap);
        tracing::info!("{} logged in from {} (data port {port})", acc.nick, self.ip);
        true
    }

    fn cleanup(&mut self) {
        let Some(nick) = self.nick.take() else { return };
        let key = nick.to_lowercase();
        let mut st = self.shared.lock();
        // a ghost-killed connection must not remove its replacement
        if st.sessions.get(&key).is_none_or(|s| s.id != self.id) {
            return;
        }
        let channels: Vec<String> = st.sessions[&key].channels.iter().cloned().collect();
        for ch in channels {
            st.part_channel(&nick, &ch);
        }
        st.sessions.remove(&key);
        st.link_broadcast(&Message::new(crate::link::USER_OFF, nick.clone()));
        let signoff = Message::new(msg::USER_SIGNOFF, nick.clone());
        for s in st.sessions.values() {
            if s.hotlist.contains(&key) {
                s.send(signoff.clone());
            }
        }
        if let Some(acc) = st.db.get_mut(&nick) {
            acc.last_seen = now_secs();
        }
        let snap = st.db.snapshot();
        drop(st);
        write_snapshot(snap);
        tracing::info!("{nick} logged out");
    }

    // ------------------------------------------------------------- dispatch

    fn dispatch(&mut self, m: Message) {
        match m.kind {
            msg::SHARE_FILE => self.share(&m),
            msg::SHARE_DIRECTORY => self.share_directory(&m),
            msg::UNSHARE_FILE => self.unshare(&m),
            msg::UNSHARE_ALL => self.with_me(|s| s.files.clear()),
            msg::SEARCH => self.search(&m),
            msg::DOWNLOAD_REQUEST => self.download_request(&m),
            msg::UPLOAD_ACCEPT => self.upload_accept(&m),
            msg::PUSH_REQUEST => self.push_request(&m),
            msg::QUEUE_LIMIT => self.queue_limit(&m),
            msg::DATA_PORT_ERROR => self.data_port_error(&m),
            msg::DOWNLOAD_START => self.with_me(|s| {
                s.downloads += 1;
                s.total_downloads += 1;
            }),
            msg::DOWNLOAD_COMPLETE => self.with_me(|s| s.downloads = s.downloads.saturating_sub(1)),
            msg::UPLOAD_START => self.with_me(|s| {
                s.uploads += 1;
                s.total_uploads += 1;
            }),
            msg::UPLOAD_COMPLETE => self.with_me(|s| s.uploads = s.uploads.saturating_sub(1)),
            msg::RESUME_REQUEST => self.resume_request(&m),
            msg::PRIVATE_MESSAGE => self.private_message(&m),
            msg::HOTLIST_ADD | msg::HOTLIST_INIT => self.hotlist_add(&m),
            msg::HOTLIST_REMOVE => {
                let target = m.data.trim().to_lowercase();
                self.with_me(|s| {
                    s.hotlist.remove(&target);
                });
            }
            msg::BROWSE => self.browse(&m),
            msg::DIRECT_BROWSE => self.direct_browse(&m),
            msg::DIRECT_BROWSE_ACCEPT => self.direct_browse_accept(&m),
            msg::SERVER_STATS => {
                let stats = self.shared.lock().stats();
                self.reply(msg::SERVER_STATS, stats);
            }
            msg::IGNORE_LIST => self.ignore_list(),
            msg::IGNORE_ADD => self.ignore_add(&m),
            msg::IGNORE_REMOVE => self.ignore_remove(&m),
            msg::IGNORE_CLEAR => {
                let mut n = 0;
                self.with_me(|s| {
                    n = s.ignored.len();
                    s.ignored.clear();
                });
                self.reply(msg::IGNORE_CLEAR, n.to_string());
            }
            msg::JOIN_CHANNEL => self.join(&m),
            msg::PART_CHANNEL => {
                let key = m.data.trim().to_lowercase();
                let nick = self.nick().to_owned();
                self.shared.lock().part_channel(&nick, &key);
            }
            msg::CHANNEL_SAY => self.channel_say(&m),
            msg::EMOTE => self.emote(&m),
            msg::CHANNEL_TOPIC => self.topic(&m),
            msg::LIST_CHANNELS => self.list_channels(),
            msg::LIST_ALL_CHANNELS => self.list_all_channels(),
            msg::LIST_CHANNEL_USERS => self.list_channel_users(&m),
            msg::CHANNEL_INVITE => self.invite(&m),
            msg::CHANNEL_INVITE_ACCEPT | msg::CHANNEL_INVITE_DECLINE => {}
            msg::KICK => self.kick(&m),
            msg::CHANNEL_BAN_LIST => self.channel_ban_list(&m),
            msg::CHANNEL_BAN => self.channel_ban(&m, true),
            msg::CHANNEL_UNBAN => self.channel_ban(&m, false),
            msg::CHANNEL_BAN_CLEAR => self.channel_ban_clear(&m),
            msg::CLEAR_CHANNEL => self.clear_channel(&m),
            msg::LINK_SPEED_REQUEST => {
                let target = m.data.trim();
                let st = self.shared.lock();
                let reply = st
                    .session(target)
                    .map(|s| format!("{} {}", s.nick, s.link_speed))
                    .or_else(|| st.remote_user(target).map(|r| format!("{} {}", r.nick, r.speed)));
                drop(st);
                match reply {
                    Some(r) => self.reply(msg::LINK_SPEED, r),
                    None => self.error(format!("User {target} is not currently online.")),
                }
            }
            msg::WHOIS => self.whois(&m),
            msg::PING => self.relay_ping(&m, msg::PING),
            msg::PONG => self.relay_ping(&m, msg::PONG),
            // the b10 client sends a timestamp and computes the round trip
            // from the echo
            msg::SERVER_PING => self.reply(msg::SERVER_PING, m.data.clone()),
            msg::MOTD => {
                for line in &self.shared.config.motd {
                    self.reply(msg::MOTD, line.clone());
                }
            }
            msg::SERVER_VERSION => self.error(VERSION),
            msg::CHANGE_LINK_SPEED => {
                if let Some(speed) = m.args().num::<u32>() {
                    self.with_me(|s| s.link_speed = speed.min(10));
                }
            }
            msg::CHANGE_DATA_PORT => {
                if let Some(port) = m.args().num::<u16>() {
                    self.with_me(|s| s.data_port = port);
                }
            }
            msg::CHANGE_PASSWORD => self.change_password(&m),
            msg::CHANGE_EMAIL => self.change_email(&m),
            msg::SET_USER_LEVEL => self.set_user_level(&m),
            msg::KILL => self.kill_user(&m),
            msg::NUKE => self.nuke(&m, true),
            msg::UNNUKE => self.nuke(&m, false),
            msg::BAN => self.ban(&m),
            msg::UNBAN => self.unban(&m),
            msg::BAN_LIST => self.ban_list(),
            msg::MUZZLE => self.muzzle(&m, true),
            msg::UNMUZZLE => self.muzzle(&m, false),
            msg::SET_LINK_SPEED => self.set_link_speed(&m),
            msg::SET_DATA_PORT => self.set_data_port(&m),
            msg::SET_PASSWORD => self.set_password(&m),
            msg::OP_SAY => self.op_say(&m),
            msg::ANNOUNCE => self.announce(&m),
            msg::CLOAK => {
                if self.require(Level::Moderator) {
                    let mut cloaked = false;
                    self.with_me(|s| {
                        s.cloaked = !s.cloaked;
                        cloaked = s.cloaked;
                    });
                    self.error(if cloaked { "You are now cloaked." } else { "You are no longer cloaked." });
                }
            }
            msg::RELOAD_CONFIG | msg::SET_CONFIG | msg::REDIRECT | msg::CYCLE => {
                if self.require(Level::Elite) {
                    self.error("not supported by this server");
                }
            }
            msg::OPTIONAL_PORTS | msg::UNKNOWN_704 | msg::PRE_LOGIN | msg::LOGIN_OPTIONS | msg::LOGIN_OPTIONS2 => {}
            other => tracing::debug!("{}: unhandled message {other}: {:?}", self.nick(), m.data),
        }
    }

    fn with_me(&self, f: impl FnOnce(&mut Session)) {
        let key = self.key();
        if let Some(s) = self.shared.lock().sessions.get_mut(&key) {
            f(s);
        }
    }

    fn my_level(&self) -> Level {
        self.shared.lock().session(self.nick()).map(|s| s.level).unwrap_or(Level::Leech)
    }

    fn require(&self, level: Level) -> bool {
        if self.my_level() >= level {
            true
        } else {
            self.error("permission denied");
            false
        }
    }

    // -------------------------------------------------------------- library

    fn share(&mut self, m: &Message) {
        match napster_proto::FileInfo::parse_share(&m.data) {
            Some(f) => self.add_shares(vec![f]),
            None => self.error("invalid share request"),
        }
    }

    fn add_shares(&mut self, files: Vec<napster_proto::FileInfo>) {
        let max = self.shared.config.max_shares;
        let mut over = false;
        self.with_me(|s| {
            for f in files {
                if s.files.len() >= max {
                    over = true;
                    break;
                }
                if !s.files.iter().any(|x| x.filename == f.filename) {
                    s.files.push(f);
                }
            }
        });
        if over && !self.share_warned {
            self.share_warned = true;
            self.error(format!("This server lets each user share at most {max} files; the rest were ignored."));
        }
    }

    fn share_directory(&mut self, m: &Message) {
        let mut a = m.args();
        let Some(dir) = a.str() else { return };
        let sep = if dir.contains('\\') { '\\' } else { '/' };
        let mut files = Vec::new();
        while let Some(mut f) = napster_proto::FileInfo::from_args(&mut a) {
            f.filename = format!("{}{}{}", dir.trim_end_matches(sep), sep, f.filename);
            files.push(f);
        }
        self.add_shares(files);
    }

    fn unshare(&self, m: &Message) {
        let name = m.args().str().unwrap_or("").to_owned();
        self.with_me(|s| s.files.retain(|f| f.filename != name));
    }

    fn search(&self, m: &Message) {
        let q = match SearchQuery::parse(&m.data) {
            Ok(q) => q,
            Err(e) => {
                self.error(e.to_string());
                self.reply(msg::SEARCH_END, "");
                return;
            }
        };
        let words = q.words();
        let max = q.max_results.min(self.shared.config.max_results) as usize;
        let me = self.key();
        let mut st = self.shared.lock();
        let mut n = 0;
        'outer: for (k, s) in &st.sessions {
            if *k == me {
                continue;
            }
            for f in &s.files {
                if n >= max {
                    break 'outer;
                }
                if q.matches(&words, f, s.link_speed) {
                    let r = SearchResult { file: f.clone(), nick: s.nick.clone(), ip: s.ip, link_speed: s.link_speed };
                    self.reply(msg::SEARCH_RESULT, r.to_payload());
                    n += 1;
                }
            }
        }
        // other servers finish the search (and send 202) if linked
        let remaining = max.saturating_sub(n);
        let fanned = !q.local_only && crate::link::start_search(&self.shared, &mut st, self.nick(), remaining, &m.data);
        drop(st);
        if !fanned {
            self.reply(msg::SEARCH_END, "");
        }
        tracing::debug!("{}: search {:?} -> {n} local results", self.nick(), q.terms);
    }

    fn resume_request(&self, m: &Message) {
        let mut a = m.args();
        let md5 = a.str().unwrap_or("").to_owned();
        let size: u64 = a.num().unwrap_or(0);
        let me = self.key();
        let st = self.shared.lock();
        for (k, s) in &st.sessions {
            if *k == me {
                continue;
            }
            for f in s.files.iter().filter(|f| f.md5 == md5 && f.size == size) {
                self.reply(
                    msg::RESUME_RESULT,
                    format!(
                        "{} {} {} {} {} {} {}",
                        s.nick,
                        ip::encode(s.ip),
                        s.data_port,
                        quote(&f.filename),
                        f.md5,
                        f.size,
                        s.link_speed
                    ),
                );
            }
        }
        drop(st);
        self.reply(msg::RESUME_END, "");
    }

    // ------------------------------------------------------------ transfers

    /// 203 `<nick> "<file>"`: ask the uploader (607); they answer with 608
    /// which produces the 204 for us.
    fn download_request(&self, m: &Message) {
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let file = a.str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        if st.forward(&target, self.nick(), m) {
            return;
        }
        let me = st.session(self.nick());
        let up = st.session(&target);
        match (me, up) {
            (Some(me), Some(up)) if up.files.iter().any(|f| f.filename == file) => {
                up.send(Message::new(msg::UPLOAD_REQUEST, format!("{} {} {}", me.nick, quote(&file), me.link_speed)));
            }
            _ => self.reply(msg::DOWNLOAD_ERROR, format!("{} {}", target, quote(&file))),
        }
    }

    /// 608 `<downloader> "<file>"` from the uploader.
    fn upload_accept(&self, m: &Message) {
        let mut a = m.args();
        let downloader = a.str().unwrap_or("").to_owned();
        let file = a.str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        let Some(me) = st.session(self.nick()) else { return };
        let Some(f) = me.files.iter().find(|f| f.filename == file) else {
            st.deliver(&downloader, Message::new(msg::DOWNLOAD_ERROR, format!("{} {}", me.nick, quote(&file))));
            return;
        };
        st.deliver(
            &downloader,
            Message::new(
                msg::DOWNLOAD_ACK,
                format!(
                    "{} {} {} {} {} {}",
                    me.nick,
                    ip::encode(me.ip),
                    me.data_port,
                    quote(&f.filename),
                    f.md5,
                    me.link_speed
                ),
            ),
        );
    }

    /// 500 `<uploader> "<file>"`: the uploader is firewalled, ask them to
    /// connect to us (501).
    fn push_request(&self, m: &Message) {
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let file = a.str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        if st.forward(&target, self.nick(), m) {
            return;
        }
        let (Some(me), Some(up)) = (st.session(self.nick()), st.session(&target)) else {
            self.reply(msg::DOWNLOAD_ERROR, format!("{} {}", target, quote(&file)));
            return;
        };
        let Some(f) = up.files.iter().find(|f| f.filename == file) else {
            self.reply(msg::DOWNLOAD_ERROR, format!("{} {}", target, quote(&file)));
            return;
        };
        if me.data_port == 0 {
            self.error(format!("Unable to download from {}: you are both behind firewalls (data port 0).", up.nick));
            return;
        }
        up.send(Message::new(
            msg::PUSH_ACK,
            format!(
                "{} {} {} {} {} {}",
                me.nick,
                ip::encode(me.ip),
                me.data_port,
                quote(&f.filename),
                f.md5,
                me.link_speed
            ),
        ));
    }

    /// 619 `<downloader> "<file>" <limit>` from the uploader -> 620 to the downloader.
    fn queue_limit(&self, m: &Message) {
        let mut a = m.args();
        let downloader = a.str().unwrap_or("").to_owned();
        let file = a.str().unwrap_or("").to_owned();
        let limit: u32 = a.num().unwrap_or(0);
        let st = self.shared.lock();
        let Some(me) = st.session(self.nick()) else { return };
        let size = me.files.iter().find(|f| f.filename == file).map(|f| f.size).unwrap_or(0);
        st.deliver(
            &downloader,
            Message::new(msg::QUEUE_LIMIT_NOTIFY, format!("{} {} {} {}", me.nick, quote(&file), size, limit)),
        );
    }

    /// 626 `<uploader>`: we couldn't reach their data port; tell them.
    fn data_port_error(&self, m: &Message) {
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        if let Some(up) = st.session(&target) {
            up.send(Message::new(msg::DATA_PORT_ERROR, format!("{} {}", self.nick(), up.data_port)));
        } else if let Some(r) = st.remote_user(&target) {
            let port = r.data_port;
            st.deliver(&target, Message::new(msg::DATA_PORT_ERROR, format!("{} {port}", self.nick())));
        }
    }

    // ------------------------------------------------------------- browsing

    fn browse(&self, m: &Message) {
        let target = m.args().str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        if st.forward(&target, self.nick(), m) {
            return;
        }
        match st.session(&target) {
            Some(s) => {
                for f in &s.files {
                    self.reply(msg::BROWSE_RESULT, format!("{} {}", s.nick, f.to_share()));
                }
                self.reply(msg::BROWSE_END, format!("{} {}", s.nick, ip::encode(s.ip)));
            }
            None => self.reply(msg::USER_SIGNOFF, target),
        }
    }

    /// 640 `<nick>`: forward to the browsee, who connects or accepts.
    fn direct_browse(&self, m: &Message) {
        let target = m.args().str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        if st.forward(&target, self.nick(), m) {
            return;
        }
        let Some(me) = st.session(self.nick()) else { return };
        let fail = |why: &str| {
            self.reply(msg::DIRECT_BROWSE_ERROR, format!("{} {}", target, quote(why)));
        };
        let Some(t) = st.session(&target) else { return fail("User is not currently online.") };
        if t.files.is_empty() {
            return fail("User is not sharing any files.");
        }
        if t.data_port == 0 && me.data_port == 0 {
            return fail("Both you and the user are firewalled.");
        }
        let data = if t.data_port == 0 {
            format!("{} {} {}", me.nick, ip::encode(me.ip), me.data_port)
        } else {
            me.nick.clone()
        };
        t.send(Message::new(msg::DIRECT_BROWSE, data));
    }

    /// 641 `<requester>` from the browsee.
    fn direct_browse_accept(&self, m: &Message) {
        let requester = m.args().str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        let Some(me) = st.session(self.nick()) else { return };
        if me.data_port != 0 {
            st.deliver(
                &requester,
                Message::new(msg::DIRECT_BROWSE_ACCEPT, format!("{} {} {}", me.nick, ip::encode(me.ip), me.data_port)),
            );
        }
    }

    // ------------------------------------------------------- users & lists

    fn private_message(&self, m: &Message) {
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let text = a.rest().to_owned();
        let st = self.shared.lock();
        match st.session(&target) {
            Some(t) if !t.cloaked || self.my_level_locked(&st) >= Level::Moderator => {
                if !t.ignored.contains(&self.key()) {
                    t.send(Message::new(msg::PRIVATE_MESSAGE, format!("{} {}", self.nick(), text)));
                }
            }
            None if st.remote_user(&target).is_some() => {
                st.deliver(&target, Message::new(msg::PRIVATE_MESSAGE, format!("{} {}", self.nick(), text)));
            }
            _ => self.error(format!("User {target} is not currently online.")),
        }
    }

    fn my_level_locked(&self, st: &State) -> Level {
        st.session(self.nick()).map(|s| s.level).unwrap_or(Level::Leech)
    }

    fn hotlist_add(&self, m: &Message) {
        let target = m.args().str().unwrap_or("").to_owned();
        if !valid_nick(&target) {
            self.reply(msg::HOTLIST_ERROR, target);
            return;
        }
        let key = target.to_lowercase();
        let mut st = self.shared.lock();
        if let Some(me) = st.session_mut(self.nick()) {
            me.hotlist.insert(key);
        }
        self.reply(msg::HOTLIST_ACK, target.clone());
        if let Some(t) = st.session(&target).filter(|t| !t.cloaked) {
            self.reply(msg::USER_SIGNON, format!("{} {}", t.nick, t.link_speed));
        } else if let Some(r) = st.remote_user(&target) {
            self.reply(msg::USER_SIGNON, format!("{} {}", r.nick, r.speed));
        }
    }

    fn ignore_list(&self) {
        let st = self.shared.lock();
        let Some(me) = st.session(self.nick()) else { return };
        for n in &me.ignored {
            self.reply(msg::IGNORE_ENTRY, n.clone());
        }
        self.reply(msg::IGNORE_LIST, me.ignored.len().to_string());
    }

    fn ignore_add(&self, m: &Message) {
        let target = m.args().str().unwrap_or("").to_owned();
        let mut added = false;
        self.with_me(|s| added = s.ignored.insert(target.to_lowercase()));
        self.reply(if added { msg::IGNORE_ADD } else { msg::IGNORE_ALREADY }, target);
    }

    fn ignore_remove(&self, m: &Message) {
        let target = m.args().str().unwrap_or("").to_owned();
        let mut removed = false;
        self.with_me(|s| removed = s.ignored.remove(&target.to_lowercase()));
        self.reply(if removed { msg::IGNORE_REMOVE } else { msg::IGNORE_NOT_IGNORED }, target);
    }

    fn whois(&self, m: &Message) {
        let target = m.args().str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        let my_level = self.my_level_locked(&st);
        match st.session(&target) {
            Some(s) if !s.cloaked || my_level >= Level::Moderator => {
                let channels: Vec<&str> =
                    s.channels.iter().filter_map(|k| st.channels.get(k)).map(|c| c.name.as_str()).collect();
                let mut out = format!(
                    "{} {} {} {} {} {} {} {} {} {}",
                    s.nick,
                    quote(s.level.as_str()),
                    s.connected.elapsed().as_secs(),
                    quote(&channels.join(" ")),
                    quote("Active"),
                    s.files.len(),
                    s.downloads,
                    s.uploads,
                    s.link_speed,
                    quote(&s.client)
                );
                if my_level >= Level::Moderator {
                    out.push_str(&format!(
                        " {} {} {} {} {} {}",
                        s.total_downloads, s.total_uploads, s.ip, s.remote_port, s.data_port, s.email
                    ));
                }
                self.reply(msg::WHOIS_RESPONSE, out);
            }
            None if st.remote_user(&target).is_some() => {
                let r = st.remote_user(&target).unwrap();
                let channels: Vec<&str> = st
                    .channels
                    .values()
                    .filter(|c| c.members.contains(&target.to_lowercase()))
                    .map(|c| c.name.as_str())
                    .collect();
                let mut out = format!(
                    "{} {} {} {} {} {} 0 0 {} {}",
                    r.nick,
                    quote(r.level.as_str()),
                    crate::link::now_ms().saturating_sub(r.login_ms) / 1000,
                    quote(&channels.join(" ")),
                    quote("Remote"),
                    r.shared,
                    r.speed,
                    quote(&r.client)
                );
                if my_level >= Level::Moderator {
                    out.push_str(&format!(" 0 0 {} 0 {} {}", r.ip, r.data_port, r.server));
                }
                self.reply(msg::WHOIS_RESPONSE, out);
            }
            _ => match st.db.get(&target) {
                Some(acc) => {
                    self.reply(msg::WHOWAS, format!("{} {} {}", acc.nick, quote(acc.level.as_str()), acc.last_seen))
                }
                None => self.error(format!("User {target} is not a known user.")),
            },
        }
    }

    fn relay_ping(&self, m: &Message, kind: u16) {
        let target = m.args().str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        if !st.deliver(&target, Message::new(kind, self.nick().to_owned())) && kind == msg::PING {
            self.error(format!("ping failed, {target} is not online"));
        }
    }

    fn change_password(&self, m: &Message) {
        let pass = m.args().str().unwrap_or("").to_owned();
        if pass.is_empty() {
            return self.error("invalid password");
        }
        let mut st = self.shared.lock();
        if let Some(acc) = st.db.get_mut(self.nick()) {
            acc.set_password(&pass);
        }
        let snap = st.db.snapshot();
        drop(st);
        write_snapshot(snap);
        self.error("Password changed.");
    }

    fn change_email(&self, m: &Message) {
        let email = m.args().str().unwrap_or("").to_owned();
        let mut st = self.shared.lock();
        if let Some(acc) = st.db.get_mut(self.nick()) {
            acc.email = email.clone();
        }
        if let Some(s) = st.session_mut(self.nick()) {
            s.email = email;
        }
        let snap = st.db.snapshot();
        drop(st);
        write_snapshot(snap);
    }

    // ------------------------------------------------------------- channels

    fn join(&self, m: &Message) {
        let name = m.args().str().unwrap_or("").to_owned();
        if name.is_empty() || name.len() > 64 || name.contains(char::is_whitespace) {
            return self.error(format!("Invalid channel name: {name}"));
        }
        let key = name.to_lowercase();
        let me_key = self.key();
        let mut st = self.shared.lock();
        let Some(me) = st.session(self.nick()) else { return };
        let (level, ip) = (me.level, me.ip.to_string());
        if me.channels.len() >= self.shared.config.max_channels_per_user && !me.channels.contains(&key) {
            return self.error("You are in too many channels already.");
        }
        if !st.channels.contains_key(&key) {
            let user_made = st.channels.values().filter(|c| !c.permanent).count();
            if user_made >= self.shared.config.max_user_channels && level < Level::Moderator {
                return self.error("This server has too many channels; join an existing one.");
            }
        }
        let ch = st.channels.entry(key.clone()).or_insert_with(|| Channel::new(&name, false));
        if ch.members.contains(&me_key) {
            return;
        }
        if level < Level::Moderator {
            if level < ch.level {
                return self.error(format!("Channel {} requires {} level.", ch.name, ch.level.as_str()));
            }
            if ch.members.len() as u32 >= ch.limit {
                return self.error(format!("Channel {} is full.", ch.name));
            }
            if ch.bans.iter().any(|b| b.matches(self.nick(), &ip)) {
                return self.error(format!("You are banned from {}.", ch.name));
            }
        }
        ch.members.push(me_key.clone());
        let ch_name = ch.name.clone();
        let topic = ch.topic.clone();
        let members = ch.members.clone();
        if let Some(me) = st.session_mut(self.nick()) {
            me.channels.insert(key);
        }
        self.reply(msg::JOIN_ACK, ch_name.clone());
        for mk in &members {
            if let Some(entry) = st.channel_entry_for(mk, &ch_name) {
                self.reply(msg::CHANNEL_USER, entry);
            }
        }
        st.link_broadcast(&Message::new(crate::link::CHAN_JOIN, format!("{} {}", ch_name, self.nick())));
        self.reply(msg::CHANNEL_USER_END, ch_name.clone());
        self.reply(msg::CHANNEL_TOPIC, format!("{ch_name} {topic}"));
        let me = &st.sessions[&me_key];
        let join = Message::new(msg::CHANNEL_JOIN, me.channel_entry(&ch_name));
        for mk in members.iter().filter(|k| **k != me_key) {
            if let Some(s) = st.sessions.get(mk) {
                s.send(join.clone());
            }
        }
    }

    /// The member's lower-case channel key if they're in it.
    fn member_of(&self, st: &State, channel: &str) -> Option<String> {
        let key = channel.to_lowercase();
        st.channels.get(&key).filter(|c| c.members.contains(&self.key())).map(|_| key)
    }

    fn channel_say(&self, m: &Message) {
        let mut a = m.args();
        let channel = a.str().unwrap_or("").to_owned();
        let text = a.rest().to_owned();
        let st = self.shared.lock();
        let Some(key) = self.member_of(&st, &channel) else {
            return self.error(format!("You are not in channel {channel}."));
        };
        if st.session(self.nick()).is_some_and(|s| s.muzzled) {
            return self.error("You are muzzled.");
        }
        let name = st.channels[&key].name.clone();
        st.broadcast_channel(
            &name,
            Some(self.nick()),
            &Message::new(msg::CHANNEL_MESSAGE, format!("{} {} {}", name, self.nick(), text)),
        );
        st.link_broadcast(&Message::new(crate::link::CHAN_SAY, format!("{} {} {}", name, self.nick(), text)));
    }

    fn emote(&self, m: &Message) {
        let mut a = m.args();
        let channel = a.str().unwrap_or("").to_owned();
        let text = a.str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        let Some(key) = self.member_of(&st, &channel) else {
            return self.error(format!("You are not in channel {channel}."));
        };
        if st.session(self.nick()).is_some_and(|s| s.muzzled) {
            return self.error("You are muzzled.");
        }
        let name = st.channels[&key].name.clone();
        st.broadcast_channel(
            &name,
            Some(self.nick()),
            &Message::new(msg::EMOTE, format!("{} {} {}", name, self.nick(), quote(&text))),
        );
        st.link_broadcast(&Message::new(crate::link::CHAN_EMOTE, format!("{} {} {}", name, self.nick(), quote(&text))));
    }

    fn topic(&self, m: &Message) {
        let mut a = m.args();
        let channel = a.str().unwrap_or("").to_owned();
        let topic = a.rest().to_owned();
        let mut st = self.shared.lock();
        let level = self.my_level_locked(&st);
        let Some(ch) = st.channels.get_mut(&channel.to_lowercase()) else {
            return self.error(format!("Channel {channel} does not exist!"));
        };
        if topic.is_empty() {
            let t = format!("{} {}", ch.name, ch.topic);
            return self.reply(msg::CHANNEL_TOPIC, t);
        }
        let member = ch.members.contains(&self.key());
        if level < Level::Moderator && (!member || ch.permanent) {
            return self.error("permission denied");
        }
        ch.topic = topic;
        let out = Message::new(msg::CHANNEL_TOPIC, format!("{} {}", ch.name, ch.topic));
        let name = ch.name.clone();
        let synced = Message::new(crate::link::CHAN_TOPIC, format!("{} {}", ch.name, ch.topic));
        st.broadcast_channel(&name, None, &out);
        st.link_broadcast(&synced);
    }

    fn list_channels(&self) {
        let st = self.shared.lock();
        for ch in st.channels.values() {
            self.reply(msg::CHANNEL_LIST_ENTRY, format!("{} {} {}", ch.name, ch.members.len(), ch.topic));
        }
        self.reply(msg::LIST_CHANNELS, "");
    }

    fn list_all_channels(&self) {
        let st = self.shared.lock();
        for ch in st.channels.values() {
            self.reply(
                msg::ALL_CHANNELS_ENTRY,
                format!(
                    "{} {} {} {} {} {}",
                    ch.name,
                    ch.members.len(),
                    u8::from(!ch.permanent),
                    ch.level as u8,
                    ch.limit,
                    quote(&ch.topic)
                ),
            );
        }
        self.reply(msg::LIST_ALL_CHANNELS, "");
    }

    fn list_channel_users(&self, m: &Message) {
        let channel = m.args().str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        let Some(ch) = st.channels.get(&channel.to_lowercase()) else {
            return self.error(format!("Channel {channel} does not exist!"));
        };
        for mk in &ch.members {
            if let Some(entry) = st.channel_entry_for(mk, &ch.name) {
                self.reply(msg::CHANNEL_USER_ENTRY, entry);
            }
        }
        self.reply(msg::LIST_CHANNEL_USERS, ch.name.clone());
    }

    fn invite(&self, m: &Message) {
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let channel = a.str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        let Some(key) = self.member_of(&st, &channel) else {
            return self.error(format!("You are not in channel {channel}."));
        };
        let ch = &st.channels[&key];
        let text = format!("{} {} {} 0 Hello!", self.nick(), ch.name, quote(&ch.topic));
        if !st.deliver(&target, Message::new(msg::CHANNEL_INVITE, text)) {
            self.error(format!("User {target} is not currently online."));
        }
    }

    fn kick(&self, m: &Message) {
        if !self.require(Level::Moderator) {
            return;
        }
        let mut a = m.args();
        let channel = a.str().unwrap_or("").to_owned();
        let target = a.str().unwrap_or("").to_owned();
        let reason = a.str().unwrap_or("").to_owned();
        let mut st = self.shared.lock();
        let key = channel.to_lowercase();
        let name = st.channels.get(&key).map(|c| c.name.clone()).unwrap_or(channel);
        if st.part_channel(&target, &key) {
            st.send(&target, err(format!("You were kicked from {name} by {}: {reason}", self.nick())));
        } else {
            self.error(format!("{target} is not in {name}."));
        }
    }

    fn channel_ban_list(&self, m: &Message) {
        let channel = m.args().str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        if let Some(ch) = st.channels.get(&channel.to_lowercase()) {
            for b in &ch.bans {
                self.reply(msg::CHANNEL_BAN_ENTRY, format!("{} {} {} {}", b.target, b.by, quote(&b.reason), b.when));
            }
        }
        self.reply(msg::CHANNEL_BAN_LIST, channel);
    }

    fn channel_ban(&self, m: &Message, ban: bool) {
        if !self.require(Level::Moderator) {
            return;
        }
        let mut a = m.args();
        let channel = a.str().unwrap_or("").to_owned();
        let target = a.str().unwrap_or("").to_lowercase();
        let reason = a.str().unwrap_or("").to_owned();
        let mut st = self.shared.lock();
        let key = channel.to_lowercase();
        let Some(ch) = st.channels.get_mut(&key) else {
            return self.error(format!("Channel {channel} does not exist!"));
        };
        ch.bans.retain(|b| b.target != target);
        if ban {
            ch.bans.push(Ban { target: target.clone(), by: self.nick().to_owned(), reason, when: now_secs() });
            st.part_channel(&target, &key);
        }
    }

    fn channel_ban_clear(&self, m: &Message) {
        if !self.require(Level::Moderator) {
            return;
        }
        let channel = m.args().str().unwrap_or("").to_lowercase();
        if let Some(ch) = self.shared.lock().channels.get_mut(&channel) {
            ch.bans.clear();
        }
    }

    fn clear_channel(&self, m: &Message) {
        if !self.require(Level::Moderator) {
            return;
        }
        let key = m.args().str().unwrap_or("").to_lowercase();
        let mut st = self.shared.lock();
        let members: Vec<String> = st.channels.get(&key).map(|c| c.members.clone()).unwrap_or_default();
        for mk in members {
            if mk != self.key() {
                st.part_channel(&mk, &key);
            }
        }
    }

    // ---------------------------------------------------------- moderation

    /// Lookup target for an admin action: must be online and below us
    /// (Elite can act on anyone).
    fn outranks(&self, st: &State, target: &str) -> bool {
        let mine = self.my_level_locked(st);
        let theirs =
            st.session(target).map(|s| s.level).or_else(|| st.db.get(target).map(|a| a.level)).unwrap_or(Level::Leech);
        mine == Level::Elite || mine > theirs
    }

    fn kill_user(&self, m: &Message) {
        if !self.require(Level::Moderator) {
            return;
        }
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let reason = a.str().unwrap_or("").to_owned();
        let st = self.shared.lock();
        if !self.outranks(&st, &target) {
            return self.error("permission denied");
        }
        match st.session(&target) {
            Some(t) => {
                t.send(err(format!("You were killed by {}: {reason}", self.nick())));
                t.kill.cancel();
            }
            None => self.error(format!("User {target} is not currently online.")),
        }
    }

    fn nuke(&self, m: &Message, nuke: bool) {
        if !self.require(Level::Admin) {
            return;
        }
        let target = m.args().str().unwrap_or("").to_owned();
        let mut st = self.shared.lock();
        if !self.outranks(&st, &target) {
            return self.error("permission denied");
        }
        match st.db.get_mut(&target) {
            Some(acc) => acc.nuked = nuke,
            None => return self.error(format!("User {target} is not a known user.")),
        }
        if nuke {
            if let Some(t) = st.session(&target) {
                t.send(err(format!("Your account was nuked by {}", self.nick())));
                t.kill.cancel();
            }
        }
        let snap = st.db.snapshot();
        drop(st);
        write_snapshot(snap);
    }

    fn ban(&self, m: &Message) {
        if !self.require(Level::Moderator) {
            return;
        }
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_lowercase();
        let reason = a.str().unwrap_or("").to_owned();
        if target.is_empty() {
            return;
        }
        let mut st = self.shared.lock();
        if !self.outranks(&st, &target) {
            return self.error("permission denied");
        }
        let ban = Ban { target: target.clone(), by: self.nick().to_owned(), reason, when: now_secs() };
        for s in st.sessions.values() {
            if ban.matches(&s.nick, &s.ip.to_string()) && s.level < Level::Elite {
                s.send(err(format!("You were banned by {}: {}", ban.by, ban.reason)));
                s.kill.cancel();
            }
        }
        st.db.bans.retain(|b| b.target != target);
        st.db.bans.push(ban);
        let snap = st.db.snapshot();
        drop(st);
        write_snapshot(snap);
    }

    fn unban(&self, m: &Message) {
        if !self.require(Level::Moderator) {
            return;
        }
        let target = m.args().str().unwrap_or("").to_lowercase();
        let mut st = self.shared.lock();
        st.db.bans.retain(|b| b.target != target);
        let snap = st.db.snapshot();
        drop(st);
        write_snapshot(snap);
    }

    fn ban_list(&self) {
        let st = self.shared.lock();
        for b in &st.db.bans {
            self.reply(msg::BAN_ENTRY, format!("{} {} {} {} 0", b.target, b.by, quote(&b.reason), b.when));
        }
        self.reply(msg::BAN_LIST, "");
    }

    fn muzzle(&self, m: &Message, on: bool) {
        if !self.require(Level::Moderator) {
            return;
        }
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let reason = a.str().unwrap_or("").to_owned();
        let mut st = self.shared.lock();
        if !self.outranks(&st, &target) {
            return self.error("permission denied");
        }
        match st.session_mut(&target) {
            Some(t) => {
                t.muzzled = on;
                let what = if on { "muzzled" } else { "unmuzzled" };
                t.send(err(format!("You have been {what} by {}: {reason}", self.nick())));
            }
            None => self.error(format!("User {target} is not currently online.")),
        }
    }

    fn set_user_level(&self, m: &Message) {
        if !self.require(Level::Admin) {
            return;
        }
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let Some(level) = a.str().and_then(Level::parse) else {
            return self.error("invalid level");
        };
        let mut st = self.shared.lock();
        let mine = self.my_level_locked(&st);
        if !self.outranks(&st, &target) || (level >= mine && mine != Level::Elite) {
            return self.error("permission denied");
        }
        let Some(acc) = st.db.get_mut(&target) else {
            return self.error(format!("User {target} is not a known user."));
        };
        acc.level = level;
        if let Some(s) = st.session_mut(&target) {
            s.level = level;
            s.send(err(format!("{} changed your user level to {}.", self.nick(), level.as_str())));
        }
        let snap = st.db.snapshot();
        drop(st);
        write_snapshot(snap);
    }

    fn set_link_speed(&self, m: &Message) {
        if !self.require(Level::Moderator) {
            return;
        }
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let speed: u32 = a.num().unwrap_or(0);
        if let Some(s) = self.shared.lock().session_mut(&target) {
            s.link_speed = speed.min(10);
        }
    }

    fn set_data_port(&self, m: &Message) {
        if !self.require(Level::Moderator) {
            return;
        }
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let port: u16 = a.num().unwrap_or(0);
        if let Some(s) = self.shared.lock().session_mut(&target) {
            s.data_port = port;
            s.send(Message::new(msg::SET_DATA_PORT, port.to_string()));
        }
    }

    fn set_password(&self, m: &Message) {
        if !self.require(Level::Admin) {
            return;
        }
        let mut a = m.args();
        let target = a.str().unwrap_or("").to_owned();
        let pass = a.str().unwrap_or("").to_owned();
        let mut st = self.shared.lock();
        if !self.outranks(&st, &target) || pass.is_empty() {
            return self.error("permission denied");
        }
        if let Some(acc) = st.db.get_mut(&target) {
            acc.set_password(&pass);
        }
        let snap = st.db.snapshot();
        drop(st);
        write_snapshot(snap);
    }

    fn op_say(&self, m: &Message) {
        if !self.require(Level::Moderator) {
            return;
        }
        let out = Message::new(msg::OP_SAY, format!("{} {}", self.nick(), m.data));
        for s in self.shared.lock().sessions.values().filter(|s| s.level >= Level::Moderator) {
            s.send(out.clone());
        }
    }

    fn announce(&self, m: &Message) {
        if !self.require(Level::Admin) {
            return;
        }
        let out = Message::new(msg::ANNOUNCE, format!("{} {}", self.nick(), m.data));
        for s in self.shared.lock().sessions.values() {
            s.send(out.clone());
        }
    }
}
