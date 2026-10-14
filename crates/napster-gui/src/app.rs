//! The main window, laid out like Napster v2.0 BETA 7-10: a row of tab
//! buttons (Home, Chat, Library, Search, Hot List, Transfer, Discover,
//! Help) over a gray Win2000-style body, with a two-panel status bar.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, vec2, Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use napster_core::{
    ChannelUser, Client, ClientConfig, Command, ConnState, Direction, Event, TransferInfo, TransferState,
};
use napster_proto::{link_speed_name, Compare, SearchQuery, SearchResult, LINK_SPEEDS};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::classic::{self, bold, commas};
use crate::snapshot::{self, Phase, Shot, Snapshots};

// console / chat colors, as in the original's default (white) scheme
const C_SYSTEM: Color32 = Color32::from_rgb(0x00, 0x00, 0xa0);
const C_ERROR: Color32 = Color32::from_rgb(0xc0, 0x00, 0x00);
const C_EMOTE: Color32 = Color32::from_rgb(0x80, 0x00, 0x80);
const C_JOIN: Color32 = Color32::from_rgb(0x00, 0x80, 0x00);
const C_NICK: Color32 = Color32::from_rgb(0x00, 0x00, 0x80);
const C_ME: Color32 = Color32::from_rgb(0x80, 0x00, 0x00);
const MAX_LINES: usize = 2000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Home,
    Chat,
    Library,
    Search,
    HotList,
    Transfer,
    Discover,
    Help,
}

#[derive(Clone)]
struct Line {
    color: Color32,
    who: Option<(String, Color32)>,
    text: String,
}

impl Line {
    fn sys(text: impl Into<String>) -> Self {
        Line { color: C_SYSTEM, who: None, text: text.into() }
    }
    fn err(text: impl Into<String>) -> Self {
        Line { color: C_ERROR, who: None, text: text.into() }
    }
    fn join(text: impl Into<String>) -> Self {
        Line { color: C_JOIN, who: None, text: text.into() }
    }
    fn said(who: &str, me: bool, text: impl Into<String>) -> Self {
        Line { color: classic::TEXT, who: Some((who.to_owned(), if me { C_ME } else { C_NICK })), text: text.into() }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ChatKey {
    Channel(String),
    Private(String),
}

struct ChatView {
    title: String,
    topic: String,
    users: Vec<ChannelUser>,
    /// The user last clicked in the member list (highlighted).
    selected_user: Option<String>,
    lines: Vec<Line>,
    input: String,
    unread: bool,
}

impl ChatView {
    fn new(title: &str) -> Self {
        Self {
            title: title.into(),
            topic: String::new(),
            users: vec![],
            selected_user: None,
            lines: vec![],
            input: String::new(),
            unread: false,
        }
    }
    fn push(&mut self, l: Line) {
        self.lines.push(l);
        if self.lines.len() > MAX_LINES {
            self.lines.drain(..self.lines.len() - MAX_LINES);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortCol {
    File,
    Size,
    Bitrate,
    Freq,
    Length,
    User,
    Speed,
}

const SORT_COLS: [SortCol; 7] =
    [SortCol::File, SortCol::Size, SortCol::Bitrate, SortCol::Freq, SortCol::Length, SortCol::User, SortCol::Speed];

struct SearchView {
    artist: String,
    title: String,
    max_results: u32,
    bitrate: (Option<Compare>, u32),
    freq: (Option<Compare>, u32),
    speed: (Option<Compare>, u32),
    results: Vec<SearchResult>,
    searching: bool,
    selected: HashSet<usize>,
    sort: (SortCol, bool),
}

struct BrowseView {
    nick: String,
    results: Vec<SearchResult>,
    done: bool,
    selected: HashSet<usize>,
}

/// The update dialog's current step.
enum UpdateStep {
    Available(napster_core::UpdateInfo),
    Working(String),
    Installed { version: String, relaunch: PathBuf },
    Failed(String),
    UpToDate,
}

/// What a right-click menu or button asked for, applied after drawing.
#[derive(Clone)]
enum Act {
    Get(SearchResult),
    User(String, &'static str),
}

pub struct App {
    client: Client,
    events: UnboundedReceiver<Event>,
    config: ClientConfig,
    config_path: PathBuf,
    conn: ConnState,
    status: String,
    stats: (u64, u64, u64),
    tab: Tab,
    console: Vec<Line>,
    console_input: String,
    /// Our shared files (`info.filename` is what others see).
    library: Vec<napster_core::library::SharedFile>,
    library_selected: Option<usize>,
    scanning: Option<(usize, usize)>,
    search: SearchView,
    browse: Option<BrowseView>,
    transfers: BTreeMap<u64, TransferInfo>,
    selected_transfer: Option<u64>,
    /// link speeds seen in search results, for the Transfer "Speed" column
    speeds: HashMap<String, u32>,
    chats: BTreeMap<ChatKey, ChatView>,
    active_chat: Option<ChatKey>,
    channel_list: Vec<(String, u32, String)>,
    channel_selected: Option<usize>,
    new_channel: String,
    /// lower nick -> (display nick, link speed if online)
    hotlist: BTreeMap<String, (String, Option<u32>)>,
    hotlist_input: String,
    hotlist_selected: Option<String>,
    whois: Option<Vec<(String, String)>>,
    prefs: Option<ClientConfig>,
    force_splash: bool,
    player: crate::player::Player,
    update: Option<UpdateStep>,
    snap: Option<Snapshots>,
    snap_whois: Option<Vec<(String, String)>>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, config_path: PathBuf) -> Self {
        classic::install(&cc.egui_ctx);
        let config = ClientConfig::load(&config_path).unwrap_or_default();
        let ctx = cc.egui_ctx.clone();
        let (client, events) =
            napster_core::Client::start(config.clone(), Some(Arc::new(move || ctx.request_repaint())));
        let hotlist = config.hotlist.iter().map(|n| (n.to_lowercase(), (n.clone(), None))).collect();
        let first_run = config.nick.is_empty();
        let mut app = Self {
            client,
            events,
            config: config.clone(),
            config_path,
            conn: ConnState::Disconnected,
            status: "Offline".into(),
            stats: (0, 0, 0),
            tab: Tab::Home,
            console: vec![Line::sys(format!("{} — Napster v2.0 BETA 10, rebuilt in Rust.", napster_core::CLIENT_INFO))],
            console_input: String::new(),
            library: vec![],
            library_selected: None,
            scanning: None,
            search: SearchView {
                artist: String::new(),
                title: String::new(),
                max_results: 100,
                bitrate: (None, 128),
                freq: (None, 44100),
                speed: (None, 7),
                results: vec![],
                searching: false,
                selected: HashSet::new(),
                sort: (SortCol::File, true),
            },
            browse: None,
            transfers: BTreeMap::new(),
            selected_transfer: None,
            speeds: HashMap::new(),
            chats: BTreeMap::new(),
            active_chat: None,
            channel_list: vec![],
            channel_selected: None,
            new_channel: String::new(),
            hotlist,
            hotlist_input: String::new(),
            hotlist_selected: None,
            whois: None,
            prefs: None,
            force_splash: false,
            player: crate::player::Player::default(),
            update: None,
            snap: Snapshots::from_env(),
            snap_whois: None,
        };
        if first_run {
            app.console.push(Line::sys("Welcome! Choose a user name and password in File > Preferences."));
            app.prefs = Some(config);
        } else if config.auto_connect && !config.password.is_empty() {
            app.client.send(Command::Connect);
        } else {
            app.console.push(Line::sys("Use File->Connect to connect to a Napster Server!"));
        }
        app
    }

    fn online(&self) -> bool {
        matches!(self.conn, ConnState::Online { .. })
    }

    fn my_nick(&self) -> &str {
        match &self.conn {
            ConnState::Online { nick } => nick,
            _ => &self.config.nick,
        }
    }

    fn log(&mut self, l: Line) {
        self.console.push(l);
        if self.console.len() > MAX_LINES {
            self.console.drain(..self.console.len() - MAX_LINES);
        }
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save(&self.config_path) {
            self.log(Line::err(format!("WARNING: Registry write attempt failed! ({e})")));
        }
    }

    fn chat(&mut self, key: ChatKey, title: &str) -> &mut ChatView {
        self.chats.entry(key).or_insert_with(|| ChatView::new(title))
    }

    // --------------------------------------------------------------- events

    fn drain_events(&mut self) {
        while let Ok(e) = self.events.try_recv() {
            self.handle(e);
        }
    }

    fn handle(&mut self, e: Event) {
        match e {
            Event::State(s) => {
                match &s {
                    ConnState::Online { .. } => {
                        self.log(Line::sys("Connected to Server!"));
                        self.client.send(Command::ListChannels);
                    }
                    ConnState::Disconnected => {
                        for (_, v) in self.hotlist.values_mut() {
                            *v = None;
                        }
                        self.chats.retain(|k, _| matches!(k, ChatKey::Private(_)));
                        if matches!(self.active_chat, Some(ChatKey::Channel(_))) {
                            self.active_chat = None;
                        }
                        self.status = "Offline".into();
                    }
                    ConnState::Connecting => {}
                }
                self.conn = s;
            }
            Event::Status(s) => {
                self.log(Line::sys(s.clone()));
                self.status = s;
            }
            Event::Info(s) => self.log(Line::sys(s)),
            Event::Error(s) => {
                if self.tab == Tab::Chat {
                    if let Some(c) = self.active_chat.clone().and_then(|k| self.chats.get_mut(&k)) {
                        c.push(Line::err(s.clone()));
                    }
                }
                if s.contains("not online") {
                    if let Some(b) = &mut self.browse {
                        b.done = true;
                    }
                }
                self.log(Line::err(s));
            }
            Event::Motd(s) => self.log(Line { color: classic::TEXT, who: None, text: s }),
            Event::Stats { users, files, gigabytes } => self.stats = (users, files, gigabytes),
            Event::LibraryScanning { done, total } => self.scanning = (done < total).then_some((done, total)),
            Event::Library(files) => {
                self.scanning = None;
                self.library = files;
                self.library_selected = None;
            }
            Event::SearchResult(r) => {
                if self.search.searching {
                    self.speeds.insert(r.nick.to_lowercase(), r.link_speed);
                    self.search.results.push(r);
                }
            }
            Event::SearchEnd => {
                if self.search.searching {
                    self.search.searching = false;
                    self.sort_results();
                }
            }
            Event::BrowseResult(r) => {
                if let Some(b) = &mut self.browse {
                    if b.nick.eq_ignore_ascii_case(&r.nick) && !b.done {
                        b.results.push(r);
                    }
                }
            }
            Event::BrowseEnd { nick } => {
                if let Some(b) = &mut self.browse {
                    if b.nick.eq_ignore_ascii_case(&nick) {
                        b.done = true;
                        b.results.sort_by_key(|x| x.file.filename.to_lowercase());
                    }
                }
            }
            Event::Transfer(t) => {
                if t.state == TransferState::Complete && t.direction == Direction::Download {
                    let was = self.transfers.get(&t.id).map(|o| o.state.clone());
                    if was != Some(TransferState::Complete) {
                        self.log(Line::sys(format!("Download complete: {}", t.basename())));
                    }
                }
                self.transfers.insert(t.id, t);
            }
            Event::TransferRemoved(id) => {
                self.transfers.remove(&id);
            }
            Event::PrivateMessage { from, text } => {
                let key = ChatKey::Private(from.to_lowercase());
                let active = self.active_chat.as_ref() == Some(&key) && self.tab == Tab::Chat;
                let view = self.chat(key.clone(), &from);
                view.push(Line::said(&from, false, text));
                view.unread |= !active;
                if self.active_chat.is_none() {
                    self.active_chat = Some(key);
                }
            }
            Event::Joined { channel } => {
                let key = ChatKey::Channel(channel.to_lowercase());
                self.chat(key.clone(), &channel);
                self.active_chat = Some(key);
                self.tab = Tab::Chat;
            }
            Event::Parted { channel } => {
                self.chats.remove(&ChatKey::Channel(channel.to_lowercase()));
                if self.active_chat == Some(ChatKey::Channel(channel.to_lowercase())) {
                    self.active_chat = self.chats.keys().next().cloned();
                }
            }
            Event::ChannelUsers { channel, users } => {
                let n = users.len();
                let v = self.chat(ChatKey::Channel(channel.to_lowercase()), &channel);
                v.users = users;
                v.push(Line::sys(format!("{n} user{} in channel.", if n == 1 { "" } else { "s" })));
            }
            Event::UserJoined { channel, user } => {
                let v = self.chat(ChatKey::Channel(channel.to_lowercase()), &channel);
                v.push(Line::join(format!(
                    "* {} ({}) [sharing {} file{}] has joined.",
                    user.nick,
                    link_speed_name(user.link_speed),
                    user.shared,
                    if user.shared == 1 { "" } else { "s" }
                )));
                v.users.retain(|u| !u.nick.eq_ignore_ascii_case(&user.nick));
                v.users.push(user);
            }
            Event::UserParted { channel, nick } => {
                let v = self.chat(ChatKey::Channel(channel.to_lowercase()), &channel);
                if let Some(u) = v.users.iter().find(|u| u.nick.eq_ignore_ascii_case(&nick)) {
                    let line = format!(
                        "* {} ({}) [sharing {} file{}] has left.",
                        u.nick,
                        link_speed_name(u.link_speed),
                        u.shared,
                        if u.shared == 1 { "" } else { "s" }
                    );
                    v.push(Line::join(line));
                }
                v.users.retain(|u| !u.nick.eq_ignore_ascii_case(&nick));
            }
            Event::ChannelMessage { channel, nick, text } => {
                let key = ChatKey::Channel(channel.to_lowercase());
                let active = self.active_chat.as_ref() == Some(&key) && self.tab == Tab::Chat;
                let me = nick.eq_ignore_ascii_case(self.my_nick());
                let v = self.chat(key, &channel);
                v.push(Line::said(&nick, me, text));
                v.unread |= !active;
            }
            Event::ChannelEmote { channel, nick, text } => {
                let v = self.chat(ChatKey::Channel(channel.to_lowercase()), &channel);
                v.push(Line { color: C_EMOTE, who: None, text: format!("* {nick} {text}") });
            }
            Event::Topic { channel, topic } => {
                let v = self.chat(ChatKey::Channel(channel.to_lowercase()), &channel);
                v.topic = topic;
            }
            Event::ChannelList(list) => {
                self.channel_list = list;
                self.channel_selected = None;
            }
            Event::HotlistOnline { nick, link_speed } => {
                self.hotlist.entry(nick.to_lowercase()).or_insert_with(|| (nick.clone(), None)).1 = Some(link_speed);
            }
            Event::HotlistOffline { nick } => {
                if let Some(e) = self.hotlist.get_mut(&nick.to_lowercase()) {
                    e.1 = None;
                }
            }
            Event::Whois(fields) => self.whois = Some(fields),
            Event::Pong { nick } => self.log(Line::sys(format!("[{}] Pong from {nick}!", clock()))),
            Event::ServerPong { millis } => {
                self.log(Line::sys(format!("[{}] Received pong from Server: {millis} ms", clock())))
            }
            Event::Announcement { from, text } => self.log(Line::err(format!("* [Announcement] {from}: {text}"))),
            Event::ChannelInvite { from, channel } => {
                self.log(Line::sys(format!("User {from} has invited you to a chat room! (/join {channel})")))
            }
            Event::UpdateAvailable(info) => {
                if self.snap.is_none() {
                    self.update = Some(UpdateStep::Available(info));
                }
            }
            Event::UpToDate => self.update = Some(UpdateStep::UpToDate),
            Event::UpdateProgress(msg) => self.update = Some(UpdateStep::Working(msg)),
            Event::UpdateInstalled { version, relaunch } => {
                self.log(Line::sys(format!("Updated to napster-rs {version}.")));
                self.update = Some(UpdateStep::Installed { version, relaunch });
            }
            Event::UpdateFailed(e) => {
                self.log(Line::err(format!("Update failed: {e}")));
                self.update = Some(UpdateStep::Failed(e));
            }
        }
    }

    // ------------------------------------------------------------- actions

    fn connect(&mut self) {
        if self.online() {
            self.log(Line::err("You are already connected to the Napster Server!"));
            return;
        }
        if self.config.nick.is_empty() || self.config.password.is_empty() {
            self.prefs = Some(self.config.clone());
            self.log(Line::err("Set a user name and password first."));
            return;
        }
        self.client.send(Command::Connect);
    }

    fn start_search(&mut self) {
        let terms: Vec<String> = [&self.search.artist, &self.search.title]
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        if terms.is_empty() {
            self.log(Line::err("Must specify a song title or artist to search!"));
            return;
        }
        if !self.online() {
            self.log(Line::err("Not connected to Server!"));
            return;
        }
        let pick = |(c, v): (Option<Compare>, u32)| c.map(|c| (c, v));
        let q = SearchQuery {
            terms,
            max_results: self.search.max_results,
            link_speed: pick(self.search.speed),
            bitrate: pick(self.search.bitrate),
            frequency: pick(self.search.freq),
            ..Default::default()
        };
        self.search.results.clear();
        self.search.selected.clear();
        self.search.searching = true;
        self.client.send(Command::Search(q));
    }

    fn sort_results(&mut self) {
        let (col, asc) = self.search.sort;
        self.search.results.sort_by(|a, b| {
            let o = match col {
                SortCol::File => a.file.basename().to_lowercase().cmp(&b.file.basename().to_lowercase()),
                SortCol::Size => a.file.size.cmp(&b.file.size),
                SortCol::Bitrate => a.file.bitrate.cmp(&b.file.bitrate),
                SortCol::Freq => a.file.frequency.cmp(&b.file.frequency),
                SortCol::Length => a.file.seconds.cmp(&b.file.seconds),
                SortCol::User => a.nick.to_lowercase().cmp(&b.nick.to_lowercase()),
                SortCol::Speed => a.link_speed.cmp(&b.link_speed),
            };
            if asc {
                o
            } else {
                o.reverse()
            }
        });
        self.search.selected.clear();
    }

    fn download(&mut self, r: &SearchResult) {
        self.client.send(Command::Download { nick: r.nick.clone(), file: r.file.clone() });
    }

    fn browse(&mut self, nick: &str) {
        self.browse =
            Some(BrowseView { nick: nick.to_owned(), results: vec![], done: false, selected: HashSet::new() });
        self.client.send(Command::Browse(nick.to_owned()));
        self.tab = Tab::HotList;
        self.hotlist_selected = Some(nick.to_lowercase());
    }

    fn hotlist_add(&mut self, nick: &str) {
        let nick = nick.trim();
        if nick.is_empty() {
            return;
        }
        self.hotlist.entry(nick.to_lowercase()).or_insert_with(|| (nick.to_owned(), None));
        if !self.config.hotlist.iter().any(|n| n.eq_ignore_ascii_case(nick)) {
            self.config.hotlist.push(nick.to_owned());
            self.save_config();
        }
        self.client.send(Command::HotlistAdd(nick.to_owned()));
    }

    fn hotlist_remove(&mut self, nick: &str) {
        self.hotlist.remove(&nick.to_lowercase());
        self.config.hotlist.retain(|n| !n.eq_ignore_ascii_case(nick));
        self.save_config();
        self.client.send(Command::HotlistRemove(nick.to_owned()));
    }

    fn open_private(&mut self, nick: &str) {
        let key = ChatKey::Private(nick.to_lowercase());
        self.chat(key.clone(), nick);
        self.active_chat = Some(key);
        self.tab = Tab::Chat;
    }

    fn apply(&mut self, act: Act) {
        match act {
            Act::Get(r) => self.download(&r),
            Act::User(nick, what) => self.user_action(&nick, what),
        }
    }

    fn user_action(&mut self, nick: &str, what: &str) {
        match what {
            "msg" => self.open_private(nick),
            "browse" => self.browse(nick),
            "whois" => self.client.send(Command::Whois(nick.to_owned())),
            "hotlist" => self.hotlist_add(nick),
            "remove" => self.hotlist_remove(nick),
            "select" => self.hotlist_selected = Some(nick.to_lowercase()),
            "ping" => {
                self.log(Line::sys(format!("[{}] Ping to {nick}...", clock())));
                self.client.send(Command::Ping(nick.to_owned()));
            }
            "ignore" => self.client.send(Command::Ignore(nick.to_owned())),
            "part" => self.client.send(Command::Part(nick.to_owned())),
            "close" => {
                self.chats.remove(&ChatKey::Private(nick.to_lowercase()));
                self.active_chat = None;
            }
            _ => {}
        }
    }

    /// IRC-style input from a chat or the console.
    fn submit(&mut self, target: Option<ChatKey>, text: String) {
        let text = text.trim_end().to_owned();
        if text.is_empty() {
            return;
        }
        let push_here = |app: &mut App, l: Line| match &target {
            Some(k) => {
                if let Some(v) = app.chats.get_mut(k) {
                    v.push(l)
                }
            }
            None => app.log(l),
        };
        if let Some(cmd) = text.strip_prefix('/') {
            let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
            let rest = rest.trim();
            let arg1 = rest.split_whitespace().next().unwrap_or("").to_owned();
            let channel = match &target {
                Some(k @ ChatKey::Channel(_)) => self.chats.get(k).map(|v| v.title.clone()),
                _ => None,
            };
            match verb.to_lowercase().as_str() {
                "join" | "j" if !arg1.is_empty() => self.client.send(Command::Join(arg1)),
                "part" | "leave" => match (arg1.is_empty(), channel) {
                    (false, _) => self.client.send(Command::Part(arg1)),
                    (true, Some(c)) => self.client.send(Command::Part(c)),
                    _ => push_here(self, Line::err("Proper syntax: /part <channel>")),
                },
                "msg" | "tell" | "query" => {
                    let body = rest[arg1.len()..].trim().to_owned();
                    if arg1.is_empty() {
                        push_here(self, Line::err(format!("Proper syntax: /{verb} <User> <Message>")));
                    } else {
                        self.open_private(&arg1);
                        if !body.is_empty() {
                            self.send_private(&arg1, body);
                        }
                    }
                }
                "me" => match channel {
                    Some(c) => self.client.send(Command::Emote { channel: c, text: rest.to_owned() }),
                    None => push_here(self, Line::err("This command will only work from a channel window!")),
                },
                "topic" => match channel {
                    Some(c) => self.client.send(Command::SetTopic { channel: c, topic: rest.to_owned() }),
                    None => push_here(self, Line::err("Proper syntax: /topic <Channel> <Topic>")),
                },
                "whois" | "finger" if !arg1.is_empty() => self.client.send(Command::Whois(arg1)),
                "ping" if !arg1.is_empty() => self.user_action(&arg1, "ping"),
                "serverping" => {
                    push_here(self, Line::sys(format!("[{}] Ping sent to Server...", clock())));
                    self.client.send(Command::PingServer);
                }
                "browse" if !arg1.is_empty() => self.browse(&arg1),
                "ignore" if !arg1.is_empty() => self.client.send(Command::Ignore(arg1)),
                "unignore" if !arg1.is_empty() => self.client.send(Command::Unignore(arg1)),
                "hotlist" | "add" if !arg1.is_empty() => self.hotlist_add(&arg1),
                "clear" => match &target {
                    Some(k) => {
                        if let Some(v) = self.chats.get_mut(k) {
                            v.lines.clear();
                        }
                    }
                    None => self.console.clear(),
                },
                "raw" => {
                    let mut parts = rest.splitn(2, ' ');
                    match parts.next().and_then(|n| n.parse::<u16>().ok()) {
                        Some(kind) => self.client.send(Command::Raw(kind, parts.next().unwrap_or("").to_owned())),
                        None => push_here(self, Line::err("Proper syntax: /raw <numeric> <data>")),
                    }
                }
                "help" => self.tab = Tab::Help,
                _ => push_here(self, Line::err(format!("Unknown Command '{verb}'!"))),
            }
            return;
        }
        match target {
            Some(k @ ChatKey::Channel(_)) => {
                let name = self.chats.get(&k).map(|v| v.title.clone()).unwrap_or_default();
                self.client.send(Command::Say { channel: name, text });
            }
            Some(k @ ChatKey::Private(_)) => {
                let nick = self.chats.get(&k).map(|v| v.title.clone()).unwrap_or_default();
                self.send_private(&nick, text);
            }
            None => self.log(Line::err("Use the 'Chat' button to join a channel, or type /help.")),
        }
    }

    fn send_private(&mut self, nick: &str, text: String) {
        let me = self.my_nick().to_owned();
        self.chat(ChatKey::Private(nick.to_lowercase()), nick).push(Line::said(&me, true, text.clone()));
        self.client.send(Command::PrivateMessage { to: nick.to_owned(), text });
    }

    // ------------------------------------------------------------ chrome

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu")
            .frame(egui::Frame::new().fill(classic::FACE).inner_margin(egui::Margin::symmetric(2, 1)))
            .show(ctx, |ui| {
                egui::menu::bar(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui.add_enabled(!self.online(), egui::Button::new("Connect")).clicked() {
                            self.connect();
                            ui.close_menu();
                        }
                        if ui.add_enabled(self.online(), egui::Button::new("Disconnect")).clicked() {
                            self.client.send(Command::Disconnect);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Preferences...").clicked() {
                            self.prefs = Some(self.config.clone());
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Exit").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                    ui.menu_button("Actions", |ui| {
                        if ui.button("Join Chat Rooms").clicked() {
                            self.tab = Tab::Discover;
                            self.client.send(Command::ListChannels);
                            ui.close_menu();
                        }
                        if ui.button("View Ignore List").clicked() {
                            self.tab = Tab::Home;
                            self.client.send(Command::Raw(napster_proto::msg::IGNORE_LIST, String::new()));
                            ui.close_menu();
                        }
                        if ui.button("Add User to Hot List").clicked() {
                            self.tab = Tab::HotList;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Rescan My Files").clicked() {
                            self.client.send(Command::Rescan);
                            ui.close_menu();
                        }
                        if ui.button("Ping Server").clicked() {
                            self.submit(None, "/serverping".into());
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Help", |ui| {
                        if ui.button("Help Topics").clicked() || ui.button("About Napster").clicked() {
                            self.tab = Tab::Help;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Check for Updates...").clicked() {
                            self.update = Some(UpdateStep::Working("Checking for updates...".into()));
                            self.client.send(Command::CheckForUpdate { manual: true });
                            ui.close_menu();
                        }
                    });
                });
            });
    }

    fn tab_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("tabs")
            .frame(egui::Frame::new().fill(classic::FACE).inner_margin(egui::Margin {
                left: 8,
                right: 8,
                top: 6,
                bottom: 4,
            }))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let unread = self.chats.values().any(|c| c.unread);
                    use classic::Icon;
                    let tabs = [
                        (Tab::Home, Icon::Home, "Home"),
                        (Tab::Chat, Icon::Chat, if unread { "Chat *" } else { "Chat" }),
                        (Tab::Library, Icon::Library, "Library"),
                        (Tab::Search, Icon::Search, "Search"),
                        (Tab::HotList, Icon::HotList, "Hot List"),
                        (Tab::Transfer, Icon::Transfer, "Transfer"),
                        (Tab::Discover, Icon::Discover, "Discover"),
                        (Tab::Help, Icon::Help, "Help"),
                    ];
                    for (tab, icon, label) in tabs {
                        if classic::tab_button(ui, icon, label, self.tab == tab).clicked() {
                            self.tab = tab;
                            if tab == Tab::Discover && self.online() {
                                self.client.send(Command::ListChannels);
                            }
                        }
                    }
                });
            });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::new().fill(classic::FACE).inner_margin(egui::Margin::same(2)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let left = match (&self.conn, self.scanning) {
                        (_, Some((d, t))) => format!("Building My Files... ({d} of {t})"),
                        (ConnState::Online { nick }, _) => {
                            format!("Online ({nick}): Sharing {} files.", commas(self.library.len() as u64))
                        }
                        (ConnState::Connecting, _) => self.status.clone(),
                        (ConnState::Disconnected, _) => "Offline".into(),
                    };
                    let w = ui.available_width();
                    classic::status_panel(ui, &left, (w * 0.42).max(200.0));
                    let right = if self.online() {
                        let (users, files, gigs) = self.stats;
                        format!(
                            "Currently {} files ({} gigabytes) available in {} libraries.",
                            commas(files),
                            commas(gigs),
                            commas(users)
                        )
                    } else {
                        String::new()
                    };
                    classic::status_panel(ui, &right, ui.available_width());
                });
            });
    }

    /// The "Logging on to Server..." window shown while connecting.
    fn splash(&mut self, ctx: &egui::Context) {
        if !(matches!(self.conn, ConnState::Connecting) || self.force_splash) {
            return;
        }
        let mut open = true;
        let status = self.status.clone();
        let text = if !self.force_splash
            && (status.starts_with("Connecting") || status.starts_with("Resolving") || status.starts_with("Finding"))
        {
            status
        } else {
            "Logging on to Server...".to_owned()
        };
        let mut cancel = false;
        classic::dialog(ctx, "splash", "Napster Status", &mut open, 380.0, |ui| {
            classic::sunken(ui, vec2(368.0, 92.0), Color32::WHITE, |ui| {
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_space(52.0);
                    classic::wordmark(ui, 58.0, false);
                });
            });
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(text).size(15.0));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                // marching blocks, like the original's progress bar
                let (rect, _) = ui.allocate_exact_size(vec2(270.0, 20.0), egui::Sense::hover());
                let p = ui.painter();
                p.rect_filled(rect, egui::CornerRadius::ZERO, Color32::WHITE);
                classic::bevel(p, rect, false);
                let blocks = ((ui.input(|i| i.time) * 4.0) as usize) % 22;
                for b in 0..blocks.max(1) {
                    let x = rect.left() + 3.0 + b as f32 * 12.0;
                    if x + 10.0 < rect.right() - 2.0 {
                        p.rect_filled(
                            egui::Rect::from_min_size(egui::pos2(x, rect.top() + 3.0), vec2(10.0, rect.height() - 6.0)),
                            egui::CornerRadius::ZERO,
                            classic::NAVY,
                        );
                    }
                }
                cancel = classic::button(ui, "Cancel", Some(84.0)).clicked();
            });
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        if cancel || !open {
            if !self.force_splash {
                self.client.send(Command::Disconnect);
            }
            self.force_splash = false;
        }
    }

    // ---------------------------------------------------------------- tabs

    fn home(&mut self, ui: &mut Ui) {
        let (banner, _) = ui.allocate_exact_size(vec2(ui.available_width(), 74.0), egui::Sense::hover());
        ui.painter().rect_filled(banner, egui::CornerRadius::ZERO, Color32::BLACK);
        classic::bevel(ui.painter(), banner, false);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(banner.shrink(10.0)));
        child.horizontal(|ui| {
            classic::wordmark(ui, 54.0, true);
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("Napster v2.0 BETA 10").color(Color32::from_rgb(0xc0, 0xc0, 0xc0)).size(14.0));
                ui.label(RichText::new(napster_core::CLIENT_INFO).color(Color32::from_rgb(0x80, 0x80, 0x80)));
            });
        });
        ui.add_space(6.0);
        let h = ui.available_height() - 30.0;
        let w = ui.available_width();
        let lines = &self.console;
        classic::sunken(ui, vec2(w, h), Color32::WHITE, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).stick_to_bottom(true).show(ui, |ui| {
                ui.add_space(2.0);
                for l in lines {
                    line_ui(ui, l);
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let w = ui.available_width() - 70.0;
            let r = classic::text_field_ex(ui, &mut self.console_input, w, false, "/help for commands");
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if classic::button(ui, "Send", Some(64.0)).clicked() || enter {
                let text = std::mem::take(&mut self.console_input);
                self.submit(None, text);
                r.request_focus();
            }
        });
    }

    fn chat_tab(&mut self, ui: &mut Ui) {
        if self.active_chat.as_ref().is_none_or(|k| !self.chats.contains_key(k)) {
            self.active_chat = self.chats.keys().next().cloned();
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let keys: Vec<ChatKey> = self.chats.keys().cloned().collect();
            for k in keys {
                let v = &self.chats[&k];
                let mut label = match &k {
                    ChatKey::Channel(_) => v.title.clone(),
                    ChatKey::Private(_) => format!("{} (IM)", v.title),
                };
                if v.unread {
                    label.push_str(" *");
                }
                let selected = self.active_chat.as_ref() == Some(&k);
                if classic::button_ex(ui, label, None, true, selected).clicked() {
                    self.active_chat = Some(k.clone());
                }
            }
            ui.add_space(8.0);
            if classic::button(ui, "Join Chat Rooms...", None).clicked() {
                self.tab = Tab::Discover;
                self.client.send(Command::ListChannels);
            }
        });
        ui.add_space(4.0);
        let Some(key) = self.active_chat.clone() else {
            let (w, h) = (ui.available_width(), ui.available_height());
            classic::sunken(ui, vec2(w, h), Color32::WHITE, |ui| {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label("Use the 'Join Chat Rooms...' button to join a channel!");
                });
            });
            return;
        };
        if let Some(v) = self.chats.get_mut(&key) {
            v.unread = false;
        }
        let is_channel = matches!(key, ChatKey::Channel(_));
        let action: RefCell<Option<Act>> = RefCell::new(None);
        let picked: RefCell<Option<String>> = RefCell::new(None);
        let total_w = ui.available_width();
        let users_w = if is_channel { 190.0 } else { 0.0 };
        let text_w = if is_channel { total_w - users_w - 6.0 } else { total_w };
        let text_h = ui.available_height() - 52.0;
        let v = &self.chats[&key];
        let topic = if v.topic.is_empty() { v.title.clone() } else { format!("{}:  {}", v.title, v.topic) };
        let title = v.title.clone();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(text_w);
                classic::status_panel(ui, &topic, text_w);
                ui.add_space(2.0);
                classic::sunken(ui, vec2(text_w, text_h), Color32::WHITE, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("chat", title.clone()))
                        .auto_shrink([false; 2])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for l in &v.lines {
                                line_ui(ui, l);
                            }
                        });
                });
            });
            if is_channel {
                ui.vertical(|ui| {
                    classic::status_panel(ui, &format!("{} users", v.users.len()), users_w);
                    ui.add_space(2.0);
                    classic::sunken(ui, vec2(users_w, text_h), Color32::WHITE, |ui| {
                        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            for u in &v.users {
                                let selected =
                                    v.selected_user.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&u.nick));
                                let (rect, r) =
                                    ui.allocate_exact_size(vec2(ui.available_width(), 17.0), egui::Sense::click());
                                let p = ui.painter();
                                if selected {
                                    p.rect_filled(rect, egui::CornerRadius::ZERO, classic::NAVY);
                                } else if r.hovered() {
                                    p.rect_filled(rect, egui::CornerRadius::ZERO, Color32::from_rgb(0xdd, 0xe6, 0xf6));
                                }
                                p.text(
                                    rect.left_center() + vec2(4.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    &u.nick,
                                    egui::FontId::proportional(12.0),
                                    if selected { Color32::WHITE } else { classic::TEXT },
                                );
                                let r = r.on_hover_text(format!(
                                    "{} — sharing {} files",
                                    link_speed_name(u.link_speed),
                                    u.shared
                                ));
                                if r.clicked() || r.secondary_clicked() {
                                    *picked.borrow_mut() = Some(u.nick.clone());
                                }
                                if r.double_clicked() {
                                    *action.borrow_mut() = Some(Act::User(u.nick.clone(), "msg"));
                                }
                                r.context_menu(|ui| user_menu(ui, &u.nick, &action));
                            }
                        });
                    });
                });
            }
        });
        ui.add_space(4.0);
        let mut submitted = None;
        ui.horizontal(|ui| {
            let v = self.chats.get_mut(&key).unwrap();
            let w = ui.available_width() - 150.0;
            let r = classic::text_field(ui, &mut v.input, w);
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if classic::button(ui, "Send", Some(64.0)).clicked() || enter {
                submitted = Some(std::mem::take(&mut v.input));
                r.request_focus();
            }
            let label = if is_channel { "Part" } else { "Close" };
            if classic::button(ui, label, Some(74.0)).clicked() {
                *action.borrow_mut() = Some(Act::User(title.clone(), if is_channel { "part" } else { "close" }));
            }
        });
        if let Some(nick) = picked.into_inner() {
            if let Some(v) = self.chats.get_mut(&key) {
                v.selected_user = Some(nick);
            }
        }
        if let Some(text) = submitted {
            self.submit(Some(key), text);
        }
        if let Some(a) = action.into_inner() {
            self.apply(a);
        }
    }

    fn library_tab(&mut self, ui: &mut Ui) {
        let action: RefCell<Option<(usize, &'static str)>> = RefCell::new(None);
        let list_h = ui.available_height() - 104.0;
        let total_w = ui.available_width();
        let playlist_w = (total_w * 0.32).clamp(200.0, 340.0);
        ui.horizontal_top(|ui| {
            // ---- My Files
            ui.vertical(|ui| {
                ui.set_width(total_w - playlist_w - 6.0);
                let files = &self.library;
                let selected = self.library_selected;
                let mut menu = |ui: &mut Ui, i: usize| {
                    for (label, what) in
                        [("Play", "play"), ("Add to Playlist", "enqueue"), ("Show in Folder", "reveal")]
                    {
                        if ui.button(label).clicked() {
                            *action.borrow_mut() = Some((i, what));
                            ui.close_menu();
                        }
                    }
                };
                let out = list_view(
                    ui,
                    "library",
                    list_h,
                    &[("Filename", 0.0), ("Filesize", 80.0), ("Bitrate", 50.0), ("Length", 50.0)],
                    files.len(),
                    |i| selected == Some(i),
                    |ui, i, c, sel| {
                        let f = &files[i].info;
                        let t = match c {
                            0 => f.filename.clone(),
                            1 => commas(f.size),
                            2 => dash(f.bitrate),
                            _ => duration(f.seconds),
                        };
                        classic::cell(ui, t, sel);
                    },
                    Some(&mut menu),
                );
                if let Some(i) = out.clicked.or(out.secondary) {
                    self.library_selected = Some(i);
                }
                if let Some(i) = out.double {
                    *action.borrow_mut() = Some((i, "play"));
                }
            });
            // ---- Playlist
            ui.vertical(|ui| {
                let tracks = &self.player.playlist;
                let (cur, sel) = (self.player.current, self.player.selected);
                let mut pl_action: Option<(usize, &'static str)> = None;
                let mut menu = |ui: &mut Ui, i: usize| {
                    if ui.button("Play").clicked() {
                        pl_action = Some((i, "pl-play"));
                        ui.close_menu();
                    }
                    if ui.button("Remove from Playlist").clicked() {
                        pl_action = Some((i, "pl-remove"));
                        ui.close_menu();
                    }
                    if ui.button("Clear Playlist").clicked() {
                        pl_action = Some((i, "pl-clear"));
                        ui.close_menu();
                    }
                };
                let out = list_view(
                    ui,
                    "playlist",
                    list_h,
                    &[("Playlist", 0.0), ("Time", 44.0)],
                    tracks.len(),
                    |i| sel == Some(i),
                    |ui, i, c, s| {
                        let t = &tracks[i];
                        let text = match c {
                            0 => format!("{}{}", if cur == Some(i) { "► " } else { "" }, t.title),
                            _ => duration(t.seconds),
                        };
                        classic::cell(ui, text, s);
                    },
                    Some(&mut menu),
                );
                if let Some(i) = out.clicked.or(out.secondary) {
                    self.player.selected = Some(i);
                }
                if let Some(i) = out.double {
                    pl_action = Some((i, "pl-play"));
                }
                match pl_action {
                    Some((i, "pl-play")) => self.player.play_index(i),
                    Some((i, "pl-remove")) => self.player.remove(i),
                    Some((_, "pl-clear")) => self.player.clear(),
                    _ => {}
                }
            });
        });
        if let Some((i, what)) = action.into_inner() {
            if let Some(f) = self.library.get(i) {
                let path = f.path.clone();
                match what {
                    "play" => self.player.play_file(path),
                    "enqueue" => {
                        self.player.enqueue(path);
                    }
                    _ => reveal(&path),
                }
            }
        }
        let text = match self.scanning {
            Some((d, t)) => format!("Building My Files... {d} of {t}"),
            None if self.library.is_empty() => "No Files Shared.".into(),
            None => format!("Sharing {} files.", commas(self.library.len() as u64)),
        };
        classic::info_bar(ui, &text);
        ui.add_space(3.0);
        self.player_bar(ui);
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 4.0 * 6.0) / 5.0;
            if classic::button(ui, "Rescan My Files", Some(w)).clicked() {
                self.client.send(Command::Rescan);
            }
            if classic::button(ui, "Library Paths...", Some(w)).clicked() {
                self.prefs = Some(self.config.clone());
            }
            if classic::button(ui, "Open Save Directory", Some(w)).clicked() {
                open_path(&self.config.download_dir);
            }
            if classic::button(ui, "Load Playlist...", Some(w)).clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("M3U and PLS Playlists", &["m3u", "pls"]).pick_file()
                {
                    if let Err(e) = self.player.load_playlist(&p) {
                        self.log(Line::err(format!("Unable to open specified playlist {}! ({e})", p.display())));
                    }
                }
            }
            if classic::button(ui, "Save Playlist...", Some(w)).clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("M3U Playlist", &["m3u"])
                    .add_filter("PLS Playlist", &["pls"])
                    .set_file_name("playlist.m3u")
                    .save_file()
                {
                    if let Err(e) = self.player.save_playlist(&p) {
                        self.log(Line::err(format!("Unable to save specified playlist {}! ({e})", p.display())));
                    }
                }
            }
        });
    }

    /// Transport buttons, LCD display, seek and volume.
    fn player_bar(&mut self, ui: &mut Ui) {
        use crate::player::State;
        use classic::Media;
        let state = self.player.state();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if classic::media_button(ui, Media::Prev, false).on_hover_text("Previous").clicked() {
                self.player.prev();
            }
            if classic::media_button(ui, Media::Play, state == State::Playing).on_hover_text("Play").clicked() {
                self.player.play();
            }
            if classic::media_button(ui, Media::Pause, state == State::Paused).on_hover_text("Pause").clicked() {
                self.player.pause();
            }
            if classic::media_button(ui, Media::Stop, false).on_hover_text("Stop").clicked() {
                self.player.stop();
            }
            if classic::media_button(ui, Media::Next, false).on_hover_text("Next").clicked() {
                self.player.next();
            }
            ui.add_space(8.0);
            let pos = self.player.position().as_secs();
            let len = self.player.length().as_secs();
            let title = match (self.player.current_track(), &self.player.error) {
                (_, Some(e)) => e.clone(),
                (Some(t), None) => t.title.clone(),
                (None, None) => "No song loaded".into(),
            };
            let status = match state {
                State::Playing => "PLAY",
                State::Paused => "PAUSE",
                State::Stopped => "STOP",
            };
            let line2 = format!("{status}  {}  /  {}", hms(pos), hms(len));
            let lcd_w = (ui.available_width() - 280.0).max(180.0);
            classic::lcd(ui, (&title, &line2), lcd_w);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.spacing_mut().slider_width = 150.0;
                let mut at = pos as f32;
                let r = ui.add_enabled(
                    len > 0 && state != State::Stopped,
                    egui::Slider::new(&mut at, 0.0..=len.max(1) as f32).show_value(false).text("Seek"),
                );
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    self.player.seek(std::time::Duration::from_secs_f32(at));
                }
                let mut vol = self.player.volume();
                if ui.add(egui::Slider::new(&mut vol, 0.0..=1.0).show_value(false).text("Volume")).changed() {
                    self.player.set_volume(vol);
                }
            });
        });
    }

    fn search_tab(&mut self, ui: &mut Ui) {
        let mut go = false;
        let mut clear = false;
        ui.horizontal_top(|ui| {
            let half = (ui.available_width() - 10.0) / 2.0;
            ui.vertical(|ui| {
                ui.set_width(half);
                classic::group(ui, "Search Fields", |ui| {
                    egui::Grid::new("search_fields").num_columns(2).spacing([8.0, 5.0]).show(ui, |ui| {
                        let fw = half - 110.0;
                        ui.label("Artist:");
                        let r = classic::text_field(ui, &mut self.search.artist, fw);
                        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.end_row();
                        ui.label("Song Title:");
                        let r = classic::text_field(ui, &mut self.search.title, fw);
                        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.end_row();
                        ui.label("Max Results:");
                        let mut max = self.search.max_results.to_string();
                        if classic::text_field(ui, &mut max, fw).changed() {
                            self.search.max_results = max.trim().parse::<u32>().unwrap_or(100).clamp(1, 100);
                        }
                        ui.end_row();
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let bw = (half - 34.0) / 2.0;
                        clear |= classic::button(ui, "Clear Fields", Some(bw)).clicked();
                        if self.search.searching {
                            if classic::button(ui, "Cancel", Some(bw)).clicked() {
                                self.search.searching = false;
                            }
                        } else {
                            go |= classic::button(ui, "Find It!", Some(bw)).clicked();
                        }
                    });
                });
            });
            ui.vertical(|ui| {
                ui.set_width(half);
                classic::group(ui, "Advanced Fields (OPTIONAL)", |ui| {
                    egui::Grid::new("adv_fields").num_columns(3).spacing([6.0, 7.0]).show(ui, |ui| {
                        let cw = (half - 150.0) / 2.0;
                        let bitrates = [20, 24, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 256, 320];
                        compare_row(ui, "Bitrate must be:", "br", &mut self.search.bitrate, cw, &bitrates, |v| {
                            format!("{v} KB/S")
                        });
                        let freqs = [8000, 11025, 16000, 22050, 32000, 44100, 48000];
                        compare_row(ui, "Frequency must be:", "fq", &mut self.search.freq, cw, &freqs, |v| {
                            format!("{v} HZ")
                        });
                        let speeds: Vec<u32> = (0..=10).collect();
                        compare_row(ui, "Line Speed must be:", "ls", &mut self.search.speed, cw, &speeds, |v| {
                            link_speed_name(v).to_owned()
                        });
                    });
                });
            });
        });
        if clear {
            self.search.artist.clear();
            self.search.title.clear();
            self.search.bitrate.0 = None;
            self.search.freq.0 = None;
            self.search.speed.0 = None;
        }
        if go && !self.search.searching {
            self.start_search();
        }
        ui.add_space(4.0);

        let h = ui.available_height() - 58.0;
        let results = &self.search.results;
        let selected = &self.search.selected;
        let sort = self.search.sort;
        let action: RefCell<Option<Act>> = RefCell::new(None);
        let cols: Vec<(String, f32)> = [
            ("Filename", 0.0),
            ("Filesize", 80.0),
            ("Bitrate", 50.0),
            ("Freq", 52.0),
            ("Length", 50.0),
            ("User", 100.0),
            ("Line Speed", 80.0),
            ("Ping", 44.0),
        ]
        .iter()
        .enumerate()
        .map(|(i, (t, w))| {
            let arrow = if i < SORT_COLS.len() && SORT_COLS[i] == sort.0 {
                if sort.1 {
                    " ▲"
                } else {
                    " ▼"
                }
            } else {
                ""
            };
            (format!("{t}{arrow}"), *w)
        })
        .collect();
        let col_refs: Vec<(&str, f32)> = cols.iter().map(|(t, w)| (t.as_str(), *w)).collect();
        let mut menu = |ui: &mut Ui, i: usize| {
            let r = &results[i];
            if ui.button("Get Song").clicked() {
                *action.borrow_mut() = Some(Act::Get(r.clone()));
                ui.close_menu();
            }
            user_menu(ui, &r.nick, &action);
        };
        let out = list_view(
            ui,
            "results",
            h,
            &col_refs,
            results.len(),
            |i| selected.contains(&i),
            |ui, i, c, sel| {
                let r = &results[i];
                match c {
                    0 => {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            speed_dot(ui, r.link_speed);
                            classic::cell(ui, r.file.basename(), sel).on_hover_text(&r.file.filename);
                        });
                    }
                    1 => {
                        classic::cell(ui, commas(r.file.size), sel);
                    }
                    2 => {
                        classic::cell(ui, dash(r.file.bitrate), sel);
                    }
                    3 => {
                        classic::cell(ui, dash(r.file.frequency), sel);
                    }
                    4 => {
                        classic::cell(ui, duration(r.file.seconds), sel);
                    }
                    5 => {
                        classic::cell(ui, &r.nick, sel);
                    }
                    6 => {
                        classic::cell(ui, link_speed_name(r.link_speed), sel);
                    }
                    _ => {
                        classic::cell(ui, "N/A", sel);
                    }
                }
            },
            Some(&mut menu),
        );
        if let Some(c) = out.header.filter(|&c| c < SORT_COLS.len()) {
            let col = SORT_COLS[c];
            self.search.sort = if self.search.sort.0 == col { (col, !self.search.sort.1) } else { (col, true) };
            self.sort_results();
        }
        if let Some(i) = out.double {
            let r = self.search.results[i].clone();
            self.download(&r);
        } else if let Some(i) = out.clicked {
            toggle_select(ui, &mut self.search.selected, i);
        } else if let Some(i) = out.secondary {
            if !self.search.selected.contains(&i) {
                self.search.selected = HashSet::from([i]);
            }
        }
        let n = self.search.results.len();
        let text = if self.search.searching {
            "Searching...".to_owned()
        } else if n == 0 && self.search.artist.is_empty() && self.search.title.is_empty() {
            String::new()
        } else {
            format!("Returned {n} results.")
        };
        classic::info_bar(ui, &text);
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 6.0) / 2.0;
            let any = !self.search.selected.is_empty();
            if classic::button_ex(ui, "Get Selected Song(s)", Some(w), any, false).clicked() {
                let picks: Vec<SearchResult> =
                    self.search.selected.iter().map(|&i| self.search.results[i].clone()).collect();
                for r in picks {
                    self.download(&r);
                }
                self.search.selected.clear();
            }
            if classic::button_ex(ui, "Add Selected User to Hot List", Some(w), any, false).clicked() {
                let nicks: HashSet<String> =
                    self.search.selected.iter().map(|&i| self.search.results[i].nick.clone()).collect();
                for n in nicks {
                    self.hotlist_add(&n);
                }
            }
        });
        if let Some(a) = action.into_inner() {
            self.apply(a);
        }
    }

    fn hotlist_tab(&mut self, ui: &mut Ui) {
        let action: RefCell<Option<Act>> = RefCell::new(None);
        let h = ui.available_height();
        let mut get: Vec<SearchResult> = vec![];
        ui.horizontal_top(|ui| {
            // left: the hot list
            ui.vertical(|ui| {
                ui.set_width(200.0);
                classic::status_panel(ui, "Hot List", 200.0);
                ui.add_space(2.0);
                let selected = self.hotlist_selected.clone();
                classic::sunken(ui, vec2(200.0, h - 82.0), Color32::WHITE, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        for (key, (nick, online)) in &self.hotlist {
                            let sel = selected.as_ref() == Some(key);
                            let (rect, r) =
                                ui.allocate_exact_size(vec2(ui.available_width(), 17.0), egui::Sense::click());
                            if sel {
                                ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, classic::NAVY);
                            }
                            let color = if sel { Color32::WHITE } else { classic::TEXT };
                            let dc = if online.is_some() {
                                Color32::from_rgb(0x00, 0xb0, 0x00)
                            } else {
                                Color32::from_gray(0xb0)
                            };
                            let p = ui.painter();
                            p.circle_filled(rect.left_center() + vec2(8.0, 0.0), 4.0, dc);
                            p.circle_stroke(
                                rect.left_center() + vec2(8.0, 0.0),
                                4.0,
                                egui::Stroke::new(1.0_f32, Color32::from_gray(60)),
                            );
                            let label = match online {
                                Some(s) => format!("{nick} ({})", link_speed_name(*s)),
                                None => nick.clone(),
                            };
                            p.text(
                                rect.left_center() + vec2(18.0, 0.0),
                                egui::Align2::LEFT_CENTER,
                                label,
                                egui::FontId::proportional(12.0),
                                color,
                            );
                            if r.double_clicked() {
                                *action.borrow_mut() = Some(Act::User(nick.clone(), "browse"));
                            } else if r.clicked() {
                                *action.borrow_mut() = Some(Act::User(nick.clone(), "select"));
                            }
                            r.context_menu(|ui| {
                                user_menu(ui, nick, &action);
                                if ui.button("Remove User from Hot List").clicked() {
                                    *action.borrow_mut() = Some(Act::User(nick.clone(), "remove"));
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let r = classic::text_field_ex(ui, &mut self.hotlist_input, 130.0, false, "user name");
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if classic::button(ui, "Add", Some(64.0)).clicked() || enter {
                        let n = std::mem::take(&mut self.hotlist_input);
                        *action.borrow_mut() = Some(Act::User(n, "hotlist"));
                    }
                });
                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    let nick = self.hotlist_selected.as_ref().and_then(|k| self.hotlist.get(k).map(|e| e.0.clone()));
                    if classic::button_ex(ui, "Browse", Some(98.0), nick.is_some(), false).clicked() {
                        *action.borrow_mut() = nick.clone().map(|n| Act::User(n, "browse"));
                    }
                    if classic::button_ex(ui, "Remove", Some(98.0), nick.is_some(), false).clicked() {
                        *action.borrow_mut() = nick.map(|n| Act::User(n, "remove"));
                    }
                });
            });
            // right: the selected user's files
            ui.vertical(|ui| {
                let w = ui.available_width();
                let title = match &self.browse {
                    Some(b) => format!("{}'s Files", b.nick),
                    None => "Double-click a user to see their files".into(),
                };
                classic::status_panel(ui, &title, w);
                ui.add_space(2.0);
                let empty = BrowseView { nick: String::new(), results: vec![], done: true, selected: HashSet::new() };
                let b = self.browse.as_ref().unwrap_or(&empty);
                let mut menu = |ui: &mut Ui, i: usize| {
                    if ui.button("Get Song").clicked() {
                        *action.borrow_mut() = Some(Act::Get(b.results[i].clone()));
                        ui.close_menu();
                    }
                };
                let out = list_view(
                    ui,
                    "browse",
                    h - 82.0,
                    &[("Filename", 0.0), ("Filesize", 85.0), ("Bitrate", 50.0), ("Freq", 52.0), ("Length", 50.0)],
                    b.results.len(),
                    |i| b.selected.contains(&i),
                    |ui, i, c, sel| {
                        let f = &b.results[i].file;
                        let t = match c {
                            0 => display_name(&f.filename).to_owned(),
                            1 => commas(f.size),
                            2 => dash(f.bitrate),
                            3 => dash(f.frequency),
                            _ => duration(f.seconds),
                        };
                        classic::cell(ui, t, sel);
                    },
                    Some(&mut menu),
                );
                let text = match &self.browse {
                    Some(b) if !b.done => format!("Populating list for User:  {}  ...", b.nick),
                    Some(b) => format!("{} files.", commas(b.results.len() as u64)),
                    None => String::new(),
                };
                classic::info_bar(ui, &text);
                ui.add_space(3.0);
                let any = self.browse.as_ref().is_some_and(|b| !b.selected.is_empty());
                let w = ui.available_width();
                let get_clicked = classic::button_ex(ui, "Get Selected Song(s)", Some(w), any, false).clicked();
                if let Some(b) = &mut self.browse {
                    if let Some(i) = out.double {
                        get.push(b.results[i].clone());
                    } else if let Some(i) = out.clicked {
                        toggle_select(ui, &mut b.selected, i);
                    } else if let Some(i) = out.secondary {
                        if !b.selected.contains(&i) {
                            b.selected = HashSet::from([i]);
                        }
                    }
                    if get_clicked {
                        get.extend(b.selected.iter().map(|&i| b.results[i].clone()));
                        b.selected.clear();
                    }
                }
            });
        });
        for r in get {
            self.download(&r);
        }
        if let Some(a) = action.into_inner() {
            self.apply(a);
        }
    }

    fn transfer_tab(&mut self, ui: &mut Ui) {
        let list_h = (ui.available_height() - 64.0) / 2.0;
        let mut clicked: Option<u64> = None;
        let mut opened: Option<PathBuf> = None;
        for dir in [Direction::Download, Direction::Upload] {
            let rows: Vec<&TransferInfo> = self.transfers.values().filter(|t| t.direction == dir).collect();
            let sel = self.selected_transfer;
            let speeds = &self.speeds;
            let out = list_view(
                ui,
                if dir == Direction::Download { "downloads" } else { "uploads" },
                list_h,
                &[
                    ("Filename", 0.0),
                    ("File Size", 140.0),
                    ("User", 90.0),
                    ("Status", 110.0),
                    ("Speed", 60.0),
                    ("Progress", 104.0),
                    ("Rate", 64.0),
                    ("Time Left", 66.0),
                ],
                rows.len(),
                |i| sel == Some(rows[i].id),
                |ui, i, c, s| {
                    let t = rows[i];
                    match c {
                        0 => {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 3.0;
                                let arrow = if t.direction == Direction::Download { "⬇" } else { "⬆" };
                                let color = if s { Color32::WHITE } else { classic::TEXT };
                                ui.label(RichText::new(arrow).color(color).size(11.0));
                                classic::cell(ui, t.basename(), s).on_hover_text(&t.filename);
                            });
                        }
                        1 => {
                            let done = t.offset + t.transferred;
                            let text = if t.state.is_finished() || done == 0 {
                                commas(t.size)
                            } else {
                                format!("{} of {}", commas(done), commas(t.size))
                            };
                            classic::cell(ui, text, s);
                        }
                        2 => {
                            classic::cell(ui, &t.nick, s);
                        }
                        3 => {
                            let r = classic::cell(ui, status_label(t), s);
                            if let TransferState::Failed(why) = &t.state {
                                r.on_hover_text(why);
                            }
                        }
                        4 => {
                            let sp = speeds.get(&t.nick.to_lowercase()).map(|s| short_speed(*s)).unwrap_or("Unknown");
                            classic::cell(ui, sp, s);
                        }
                        5 => {
                            if matches!(t.state, TransferState::Transferring | TransferState::Complete)
                                || t.transferred > 0
                            {
                                let color = if t.direction == Direction::Download {
                                    classic::BAR_BLUE
                                } else {
                                    classic::BAR_YELLOW
                                };
                                classic::progress(ui, t.progress(), color, 96.0);
                            }
                        }
                        6 => {
                            if t.state == TransferState::Transferring {
                                classic::cell(ui, format!("{:.2} k/s", t.rate() / 1024.0), s);
                            }
                        }
                        _ => {
                            if t.state == TransferState::Transferring && t.rate() > 1.0 {
                                let left = t.size.saturating_sub(t.offset + t.transferred) as f64 / t.rate();
                                classic::cell(ui, hms(left as u64), s);
                            }
                        }
                    }
                },
                None,
            );
            if let Some(i) = out.clicked.or(out.secondary) {
                clicked = Some(rows[i].id);
            }
            if let Some(i) = out.double {
                opened = rows[i].saved_to.clone();
            }
            ui.add_space(4.0);
        }
        if clicked.is_some() {
            self.selected_transfer = clicked;
        }
        if let Some(p) = opened {
            if napster_core::is_audio_file(&p.to_string_lossy()) {
                self.player.play_file(p);
            } else {
                reveal(&p);
            }
        }
        let active =
            |d: Direction| self.transfers.values().filter(|t| t.direction == d && !t.state.is_finished()).count();
        classic::info_bar(
            ui,
            &format!(
                "Concurrent Downloads:   {}            Concurrent Uploads:   {}",
                active(Direction::Download),
                active(Direction::Upload)
            ),
        );
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 6.0) / 2.0;
            if classic::button(ui, "Clear Finished", Some(w)).clicked() {
                self.client.send(Command::ClearFinished);
            }
            let can_cancel =
                self.selected_transfer.and_then(|id| self.transfers.get(&id)).is_some_and(|t| !t.state.is_finished());
            if classic::button_ex(ui, "Cancel", Some(w), can_cancel, false).clicked() {
                if let Some(id) = self.selected_transfer {
                    self.client.send(Command::CancelTransfer(id));
                }
            }
        });
    }

    fn discover_tab(&mut self, ui: &mut Ui) {
        classic::status_panel(ui, "Chat Rooms", ui.available_width());
        ui.add_space(2.0);
        let rows = &self.channel_list;
        let joined: HashSet<String> = self
            .chats
            .keys()
            .filter_map(|k| match k {
                ChatKey::Channel(c) => Some(c.clone()),
                _ => None,
            })
            .collect();
        let sel = self.channel_selected;
        let out = list_view(
            ui,
            "channels",
            ui.available_height() - 58.0,
            &[("Channel Name", 170.0), ("User Count", 80.0), ("Channel Topic", 0.0)],
            rows.len(),
            |i| sel == Some(i),
            |ui, i, c, s| {
                let (name, users, topic) = &rows[i];
                match c {
                    0 => {
                        let t = if joined.contains(&name.to_lowercase()) {
                            format!("{name}  (joined)")
                        } else {
                            name.clone()
                        };
                        classic::cell(ui, t, s);
                    }
                    1 => {
                        classic::cell(ui, users.to_string(), s);
                    }
                    _ => {
                        classic::cell(ui, topic, s);
                    }
                }
            },
            None,
        );
        let mut join: Option<String> = None;
        if let Some(i) = out.clicked.or(out.secondary) {
            self.channel_selected = Some(i);
        }
        if let Some(i) = out.double {
            join = Some(self.channel_list[i].0.clone());
        }
        let total: u32 = self.channel_list.iter().map(|c| c.1).sum();
        classic::info_bar(ui, &format!("{} channels, {} users chatting.", self.channel_list.len(), total));
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let can_join = self.channel_selected.is_some();
            if classic::button_ex(ui, "Join", Some(100.0), can_join, false).clicked() {
                join = self.channel_selected.map(|i| self.channel_list[i].0.clone());
            }
            if classic::button(ui, "Refresh", Some(100.0)).clicked() {
                self.client.send(Command::ListChannels);
            }
            ui.add_space(20.0);
            ui.label("Create:");
            let r = classic::text_field_ex(ui, &mut self.new_channel, 160.0, false, "new room name");
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (classic::button(ui, "Create", Some(90.0)).clicked() || enter) && !self.new_channel.trim().is_empty() {
                join = Some(std::mem::take(&mut self.new_channel).trim().replace(' ', "_"));
            }
        });
        if let Some(name) = join {
            self.client.send(Command::Join(name));
        }
    }

    fn help_tab(&mut self, ui: &mut Ui) {
        let (w, h) = (ui.available_width(), ui.available_height());
        classic::sunken(ui, vec2(w, h), Color32::WHITE, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(w - 40.0);
                        classic::wordmark(ui, 46.0, false);
                        ui.add_space(6.0);
                        ui.label(bold(format!(
                            "{} — a Rust rebuild of Napster v2.0 BETA 10",
                            napster_core::CLIENT_INFO
                        )));
                        ui.add_space(10.0);
                        for (title, body) in HELP {
                            ui.label(bold(*title));
                            ui.label(*body);
                            ui.add_space(8.0);
                        }
                    });
                });
            });
        });
    }

    fn whois_window(&mut self, ctx: &egui::Context) {
        let Some(fields) = self.whois.clone() else {
            return;
        };
        let nick = fields.first().map(|(_, v)| v.clone()).unwrap_or_default();
        let mut open = true;
        let mut close = false;
        classic::dialog(ctx, "whois", &format!("{nick}'s Finger Information"), &mut open, 330.0, |ui| {
            classic::group(ui, "User Information", |ui| {
                egui::Grid::new("whois_grid").num_columns(2).spacing([12.0, 3.0]).show(ui, |ui| {
                    for (k, v) in &fields {
                        ui.label(format!("{k}:"));
                        ui.label(v);
                        ui.end_row();
                    }
                });
            });
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                close = classic::button(ui, "OK", Some(84.0)).clicked();
            });
        });
        if close || !open {
            self.whois = None;
        }
    }

    fn prefs_window(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.prefs.take() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        classic::dialog(ctx, "prefs", "Napster Preferences", &mut open, 560.0, |ui| {
            let (banner, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), egui::Sense::hover());
            ui.painter().rect_filled(banner, egui::CornerRadius::ZERO, Color32::BLACK);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(banner.shrink2(vec2(160.0, 5.0))));
            classic::wordmark(&mut child, 42.0, true);
            ui.add_space(4.0);
            let fw = 170.0;
            // the settings scroll so OK/Cancel stay visible in small windows
            let max_h = (ctx.screen_rect().height() - 150.0).max(160.0);
            egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                classic::group(ui, "Miscellaneous Settings", |ui| {
                    egui::Grid::new("misc").num_columns(4).spacing([8.0, 5.0]).show(ui, |ui| {
                        ui.label("User Name:");
                        classic::text_field(ui, &mut draft.nick, fw);
                        ui.label("Data Port:");
                        let mut port = draft.data_port.to_string();
                        if classic::text_field(ui, &mut port, 70.0).changed() {
                            draft.data_port = port.trim().parse().unwrap_or(0);
                        }
                        ui.end_row();

                        ui.label("Password:");
                        classic::text_field_ex(ui, &mut draft.password, fw, true, "");
                        ui.label("Line Speed:");
                        egui::ComboBox::from_id_salt("speed")
                            .width(110.0)
                            .selected_text(link_speed_name(draft.link_speed))
                            .show_ui(ui, |ui| {
                                for (i, name) in LINK_SPEEDS.iter().enumerate() {
                                    ui.selectable_value(&mut draft.link_speed, i as u32, *name);
                                }
                            });
                        ui.end_row();

                        ui.label("Email:");
                        classic::text_field(ui, &mut draft.email, fw);
                        ui.end_row();
                    });
                    if !draft.nick.is_empty() && !napster_proto::valid_nick(&draft.nick) {
                        ui.colored_label(C_ERROR, "Name contains invalid characters! Please choose another name!");
                    }
                    ui.add_space(4.0);
                    egui::Grid::new("paths").num_columns(3).spacing([8.0, 5.0]).show(ui, |ui| {
                        ui.label("Library Paths:");
                        let mut paths =
                            draft.shared_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(";");
                        if classic::text_field(ui, &mut paths, 300.0).changed() {
                            draft.shared_dirs =
                                paths.split(';').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
                        }
                        if classic::button(ui, "Add Path", Some(90.0)).clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                draft.shared_dirs.push(p);
                            }
                        }
                        ui.end_row();
                        ui.label("Save Directory:");
                        let mut dl = draft.download_dir.display().to_string();
                        if classic::text_field(ui, &mut dl, 300.0).changed() {
                            draft.download_dir = PathBuf::from(dl);
                        }
                        if classic::button(ui, "Choose Dir.", Some(90.0)).clicked() {
                            if let Some(p) = rfd::FileDialog::new().set_directory(&draft.download_dir).pick_folder() {
                                draft.download_dir = p;
                            }
                        }
                        ui.end_row();
                    });
                    ui.add_space(4.0);
                    ui.checkbox(&mut draft.auto_connect, "Connect to the Server when Napster starts");
                    ui.checkbox(&mut draft.check_for_updates, "Check for new versions of Napster when it starts");
                });
                ui.add_space(6.0);
                classic::group(ui, "Server Settings", |ui| {
                    egui::Grid::new("srv").num_columns(2).spacing([8.0, 5.0]).show(ui, |ui| {
                        ui.label("Server (host:port):");
                        classic::text_field(ui, &mut draft.server, 240.0);
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut draft.use_metaserver, "This is a metaserver (redirector, port 8875)");
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut draft.tls, "Encrypt the connection (TLS; napd's port 8889)");
                        ui.end_row();
                    });
                });
                ui.add_space(6.0);
                classic::group(ui, "Firewall Settings", |ui| {
                    ui.checkbox(&mut draft.proxy.enabled, "Connect through a SOCKS5 proxy");
                    ui.add_enabled_ui(draft.proxy.enabled, |ui| {
                        egui::Grid::new("proxy").num_columns(4).spacing([8.0, 5.0]).show(ui, |ui| {
                            ui.label("Proxy Server:");
                            classic::text_field(ui, &mut draft.proxy.host, 200.0);
                            ui.label("Port:");
                            let mut port = draft.proxy.port.to_string();
                            if classic::text_field(ui, &mut port, 60.0).changed() {
                                draft.proxy.port = port.trim().parse().unwrap_or(1080);
                            }
                            ui.end_row();
                            ui.label("User Name:");
                            classic::text_field(ui, &mut draft.proxy.username, 200.0);
                            ui.label("Password:");
                            classic::text_field_ex(ui, &mut draft.proxy.password, 120.0, true, "");
                            ui.end_row();
                        });
                        ui.checkbox(&mut draft.proxy.transfers, "Also send file transfers through the proxy");
                    });
                });
                ui.add_space(6.0);
                classic::group(ui, "Transfer Settings", |ui| {
                    egui::Grid::new("xfer").num_columns(2).spacing([8.0, 5.0]).show(ui, |ui| {
                        ui.label("Max Simultaneous Uploads:");
                        let mut up = draft.max_uploads.to_string();
                        if classic::text_field(ui, &mut up, 50.0).changed() {
                            draft.max_uploads = up.trim().parse().unwrap_or(0).min(50);
                        }
                        ui.end_row();
                        ui.label("Max Simultaneous Downloads (rest will be queued):");
                        let mut down = draft.max_downloads.to_string();
                        if classic::text_field(ui, &mut down, 50.0).changed() {
                            draft.max_downloads = down.trim().parse().unwrap_or(1).clamp(1, 50);
                        }
                        ui.end_row();
                    });
                    if draft.max_uploads == 0 {
                        ui.colored_label(
                            C_ERROR,
                            "Your file server is currently disabled (max outbound transfers = 0).",
                        );
                    }
                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut draft.authentic_speeds,
                        "Authentic speeds: transfer at the real speed of my Line Speed",
                    );
                    if draft.authentic_speeds {
                        let (d, u) = napster_core::PERIOD_RATES[draft.link_speed.min(10) as usize];
                        let text = if d == 0.0 {
                            "Line Speed is Unknown, so transfers are not limited.".to_owned()
                        } else {
                            format!(
                                "{}: downloads up to {} KB/s, uploads up to {} KB/s (shared by all transfers).",
                                link_speed_name(draft.link_speed),
                                d,
                                u
                            )
                        };
                        ui.label(RichText::new(text).color(C_SYSTEM));
                    } else {
                        egui::Grid::new("limits").num_columns(2).spacing([8.0, 5.0]).show(ui, |ui| {
                            ui.label("Max Download Speed, all transfers (KB/s, 0 = unlimited):");
                            let mut lim = draft.download_limit_kbps.to_string();
                            if classic::text_field(ui, &mut lim, 60.0).changed() {
                                draft.download_limit_kbps = lim.trim().parse().unwrap_or(0);
                            }
                            ui.end_row();
                            ui.label("Max Upload Speed, all transfers (KB/s, 0 = unlimited):");
                            let mut lim = draft.upload_limit_kbps.to_string();
                            if classic::text_field(ui, &mut lim, 60.0).changed() {
                                draft.upload_limit_kbps = lim.trim().parse().unwrap_or(0);
                            }
                            ui.end_row();
                        });
                    }
                    ui.label(
                        RichText::new("Data Port 0 means you are behind a firewall; your uploads will be pushed.")
                            .color(classic::DISABLED),
                    );
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(ui.available_width() - 2.0 * 84.0 - 12.0);
                save = classic::button(ui, "OK", Some(84.0)).clicked();
                cancel = classic::button(ui, "Cancel", Some(84.0)).clicked();
            });
        });
        if save {
            self.config = draft;
            self.save_config();
            self.client.send(Command::SetConfig(Box::new(self.config.clone())));
            self.log(Line::sys("Preferences saved."));
        } else if !cancel && open {
            self.prefs = Some(draft);
        }
    }

    fn update_window(&mut self, ctx: &egui::Context) {
        let Some(step) = &self.update else { return };
        let current = napster_core::update::current_version();
        let mut open = true;
        let mut next: Option<Option<UpdateStep>> = None;
        let mut install: Option<napster_core::UpdateInfo> = None;
        let mut restart: Option<PathBuf> = None;
        classic::dialog(ctx, "update", "Napster Update", &mut open, 420.0, |ui| match step {
            UpdateStep::Available(info) => {
                ui.label(bold(format!("Napster (napster-rs) {} is available.", info.version)));
                ui.label(format!("You have version {current}."));
                ui.add_space(4.0);
                if !info.notes.trim().is_empty() {
                    classic::sunken(ui, vec2(406.0, 140.0), Color32::WHITE, |ui| {
                        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                            ui.label(info.notes.trim());
                        });
                    });
                    ui.add_space(4.0);
                }
                ui.label(
                    RichText::new(
                        "The update is downloaded from GitHub and its signature is checked before it is installed.",
                    )
                    .color(classic::DISABLED),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(ui.available_width() - 2.0 * 96.0 - 8.0);
                    if classic::button(ui, "Update Now", Some(96.0)).clicked() {
                        install = Some(info.clone());
                    }
                    if classic::button(ui, "Later", Some(96.0)).clicked() {
                        next = Some(None);
                    }
                });
            }
            UpdateStep::Working(msg) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(msg);
                });
                ui.add_space(4.0);
            }
            UpdateStep::Installed { version, relaunch } => {
                ui.label(bold(format!("Napster has been updated to {version}.")));
                ui.label("Restart Napster to use the new version.");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(ui.available_width() - 2.0 * 96.0 - 8.0);
                    if classic::button(ui, "Restart Now", Some(96.0)).clicked() {
                        restart = Some(relaunch.clone());
                    }
                    if classic::button(ui, "Later", Some(96.0)).clicked() {
                        next = Some(None);
                    }
                });
            }
            UpdateStep::Failed(e) => {
                ui.colored_label(C_ERROR, format!("Error while auto upgrading: {e}"));
                ui.label("You can download the latest version from the GitHub releases page.");
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    if classic::button(ui, "OK", Some(84.0)).clicked() {
                        next = Some(None);
                    }
                });
            }
            UpdateStep::UpToDate => {
                ui.label(format!("You have the latest version of Napster (napster-rs {current})."));
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    if classic::button(ui, "OK", Some(84.0)).clicked() {
                        next = Some(None);
                    }
                });
            }
        });
        if let Some(info) = install {
            self.client.send(Command::InstallUpdate(info));
            next = Some(Some(UpdateStep::Working("Downloading update...".into())));
        }
        if let Some(path) = restart {
            match napster_core::update::relaunch(&path) {
                Ok(()) => {
                    self.client.send(Command::Disconnect);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Err(e) => next = Some(Some(UpdateStep::Failed(format!("couldn't restart: {e}")))),
            }
        }
        if !open {
            next = Some(None);
        }
        if let Some(n) = next {
            self.update = n;
        }
        if matches!(self.update, Some(UpdateStep::Working(_))) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    // ------------------------------------------------------------ snapshots

    fn drive_snapshots(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.snap.take() else {
            return;
        };
        ctx.request_repaint();
        match s.phase {
            Phase::Starting => {
                if (self.online() && !self.library.is_empty()) || s.elapsed().as_secs() > 8 {
                    self.client.send(Command::Join("Lobby".into()));
                    self.search.artist = s.search.clone();
                    self.start_search();
                    s.go(Phase::Searching);
                }
            }
            Phase::Searching => {
                if !self.search.searching || s.elapsed().as_secs() > 5 {
                    let picks: Vec<SearchResult> = self.search.results.iter().take(3).cloned().collect();
                    for r in &picks {
                        self.download(r);
                    }
                    if let Some(r) = picks.first() {
                        self.hotlist_add(&r.nick);
                        self.browse(&r.nick);
                        self.client.send(Command::Whois(r.nick.clone()));
                    }
                    self.search.selected.insert(1);
                    s.go(Phase::Settling);
                }
            }
            Phase::Settling => {
                if self.player.playlist.is_empty() {
                    for f in self.library.iter().take(3) {
                        self.player.enqueue(f.path.clone());
                    }
                    if s.play && !self.player.playlist.is_empty() {
                        self.player.play_index(0);
                    } else {
                        self.player.current = Some(0);
                        self.player.selected = Some(0);
                    }
                }
                if s.elapsed().as_millis() > 2500 {
                    s.go(Phase::Show(0));
                }
            }
            Phase::Show(i) => {
                let shot = snapshot::ORDER[i];
                // the whois window should only appear in its own shot
                let stashed = self.whois.take().or(self.snap_whois.take());
                self.prefs = None;
                self.force_splash = false;
                match shot {
                    Shot::Home => self.tab = Tab::Home,
                    Shot::Chat | Shot::Whois => {
                        self.tab = Tab::Chat;
                        // show the member-list highlight on someone else
                        let me = self.my_nick().to_lowercase();
                        if let Some(v) = self.active_chat.clone().and_then(|k| self.chats.get_mut(&k)) {
                            v.selected_user =
                                v.users.iter().find(|u| u.nick.to_lowercase() != me).map(|u| u.nick.clone());
                        }
                    }
                    Shot::Library => self.tab = Tab::Library,
                    Shot::Search => self.tab = Tab::Search,
                    Shot::HotList => self.tab = Tab::HotList,
                    Shot::Transfer => {
                        self.tab = Tab::Transfer;
                        self.selected_transfer = self.transfers.keys().next().copied();
                    }
                    Shot::Discover => self.tab = Tab::Discover,
                    Shot::SearchWide => {
                        // the table must follow a window resize after it was first laid out
                        self.tab = Tab::Search;
                        if s.elapsed().as_millis() < 50 {
                            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(1280.0, 720.0)));
                        }
                    }
                    Shot::Help => self.tab = Tab::Help,
                    Shot::Prefs => {
                        self.tab = Tab::Search;
                        self.prefs = Some(self.config.clone());
                    }
                    Shot::Splash => {
                        self.tab = Tab::Home;
                        self.force_splash = true;
                    }
                }
                if shot == Shot::Whois {
                    self.whois = stashed.or(Some(vec![("User".into(), "(none)".into())]));
                } else {
                    self.snap_whois = stashed;
                }
                if s.elapsed().as_millis() > 700 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                        shot.name().to_owned(),
                    )));
                    s.go(Phase::Capture(i));
                }
            }
            Phase::Capture(i) => {
                let got = ctx.input(|inp| {
                    inp.raw.events.iter().find_map(|e| match e {
                        egui::Event::Screenshot { user_data, image, .. } => {
                            let name = user_data.data.as_ref().and_then(|d| d.downcast_ref::<String>()).cloned();
                            Some((name, image.clone()))
                        }
                        _ => None,
                    })
                });
                if let Some((name, image)) = got {
                    let name = name.unwrap_or_else(|| format!("shot-{i}"));
                    let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                    let path = s.dir.join(format!("{name}.png"));
                    if let Err(e) = snapshot::write_png(&path, image.size[0], image.size[1], &rgba) {
                        tracing::error!("saving {}: {e}", path.display());
                    }
                    s.go(if i + 1 < snapshot::ORDER.len() { Phase::Show(i + 1) } else { Phase::Done });
                } else if s.elapsed().as_secs() > 5 {
                    tracing::error!("screenshot {i} never arrived");
                    s.go(Phase::Done);
                }
            }
            Phase::Done => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
        self.snap = Some(s);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.player.tick();
        if self.player.state() == crate::player::State::Playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.drive_snapshots(ctx);
        self.menu_bar(ctx);
        self.tab_bar(ctx);
        self.status_bar(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(classic::FACE).inner_margin(egui::Margin {
                left: 8,
                right: 8,
                top: 4,
                bottom: 6,
            }))
            .show(ctx, |ui| match self.tab {
                Tab::Home => self.home(ui),
                Tab::Chat => self.chat_tab(ui),
                Tab::Library => self.library_tab(ui),
                Tab::Search => self.search_tab(ui),
                Tab::HotList => self.hotlist_tab(ui),
                Tab::Transfer => self.transfer_tab(ui),
                Tab::Discover => self.discover_tab(ui),
                Tab::Help => self.help_tab(ui),
            });
        self.whois_window(ctx);
        self.prefs_window(ctx);
        self.update_window(ctx);
        self.splash(ctx);
        if self.transfers.values().any(|t| t.state == TransferState::Transferring) {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.client.send(Command::Disconnect);
    }
}

// ------------------------------------------------------------------ helpers

/// Right-click menu for a list row.
type RowMenu<'a> = &'a mut dyn FnMut(&mut Ui, usize);

struct ListOut {
    clicked: Option<usize>,
    /// Right-clicked (its context menu is opening); callers select it so
    /// it's clear which row the menu acts on.
    secondary: Option<usize>,
    double: Option<usize>,
    header: Option<usize>,
}

/// A classic list view: sunken white box, raised column headers, navy
/// selection. Column width 0 means "take the remaining space".
#[allow(clippy::too_many_arguments)]
fn list_view(
    ui: &mut Ui,
    id: &str,
    height: f32,
    cols: &[(&str, f32)],
    rows: usize,
    is_selected: impl Fn(usize) -> bool,
    mut cell: impl FnMut(&mut Ui, usize, usize, bool),
    mut menu: Option<RowMenu<'_>>,
) -> ListOut {
    let mut out = ListOut { clicked: None, secondary: None, double: None, header: None };
    let width = ui.available_width();
    let height = height.max(60.0);
    classic::sunken(ui, vec2(width, height), Color32::WHITE, |ui| {
        ui.push_id(id, |ui| {
            let mut tb = TableBuilder::new(ui)
                .striped(false)
                .resizable(true)
                .sense(egui::Sense::click())
                .auto_shrink([false; 2])
                .max_scroll_height(height - 24.0)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for (_, w) in cols {
                tb = if *w == 0.0 {
                    // Not user-resizable on purpose: egui only re-fits a
                    // remainder column to the window when it isn't resizable,
                    // so this one grows and shrinks with the window while the
                    // other columns can still be dragged.
                    tb.column(Column::remainder().at_least(120.0).clip(true).resizable(false))
                } else {
                    tb.column(Column::initial(*w).at_least(30.0).clip(true))
                };
            }
            tb.header(19.0, |mut h| {
                for (i, (t, _)) in cols.iter().enumerate() {
                    h.col(|ui| {
                        if classic::header_cell(ui, t).clicked() {
                            out.header = Some(i);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(classic::ROW_H, rows, |mut row| {
                    let i = row.index();
                    let sel = is_selected(i);
                    row.set_selected(sel);
                    for c in 0..cols.len() {
                        row.col(|ui| cell(ui, i, c, sel));
                    }
                    let r = row.response();
                    if r.double_clicked() {
                        out.double = Some(i);
                    } else if r.clicked() {
                        out.clicked = Some(i);
                    } else if r.secondary_clicked() {
                        out.secondary = Some(i);
                    }
                    if let Some(m) = menu.as_mut() {
                        r.context_menu(|ui| m(ui, i));
                    }
                });
            });
        });
    });
    out
}

fn user_menu(ui: &mut Ui, nick: &str, action: &RefCell<Option<Act>>) {
    ui.label(bold(nick));
    ui.separator();
    for (label, what) in [
        ("Instant Message", "msg"),
        ("Browse User's Files", "browse"),
        ("View User Information", "whois"),
        ("Add User to Hot List", "hotlist"),
        ("Ping User", "ping"),
        ("Ignore User", "ignore"),
    ] {
        if ui.button(label).clicked() {
            *action.borrow_mut() = Some(Act::User(nick.to_owned(), what));
            ui.close_menu();
        }
    }
}

/// Ctrl/Shift-click adds to the selection, plain click replaces it.
fn toggle_select(ui: &Ui, set: &mut HashSet<usize>, i: usize) {
    let multi = ui.input(|inp| inp.modifiers.command || inp.modifiers.shift);
    if !multi {
        let only_this = set.len() == 1 && set.contains(&i);
        set.clear();
        if only_this {
            return;
        }
    }
    if !set.remove(&i) {
        set.insert(i);
    }
}

fn compare_row(
    ui: &mut Ui,
    label: &str,
    id: &str,
    value: &mut (Option<Compare>, u32),
    width: f32,
    choices: &[u32],
    fmt: impl Fn(u32) -> String,
) {
    ui.label(label);
    egui::ComboBox::from_id_salt((id, "cmp"))
        .width(width)
        .selected_text(value.0.map(|c| c.as_str()).unwrap_or(""))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut value.0, None, "");
            for c in Compare::ALL {
                ui.selectable_value(&mut value.0, Some(c), c.as_str());
            }
        });
    egui::ComboBox::from_id_salt((id, "val"))
        .width(width)
        .selected_text(if value.0.is_some() { fmt(value.1) } else { String::new() })
        .show_ui(ui, |ui| {
            for &c in choices {
                ui.selectable_value(&mut value.1, c, fmt(c));
            }
        });
    ui.end_row();
}

/// Colored dot in front of search results, as in BETA 5: green for fast
/// connections, yellow for medium, red for modems.
fn speed_dot(ui: &mut Ui, speed: u32) {
    let color = match speed {
        7..=10 => Color32::from_rgb(0x00, 0xa0, 0x00),
        4..=6 => Color32::from_rgb(0xe0, 0xc0, 0x00),
        1..=3 => Color32::from_rgb(0xd0, 0x00, 0x00),
        _ => Color32::from_rgb(0x90, 0x90, 0x90),
    };
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    ui.painter().circle_stroke(rect.center(), 4.0, egui::Stroke::new(1.0_f32, Color32::from_gray(60)));
}

fn line_ui(ui: &mut Ui, l: &Line) {
    ui.spacing_mut().item_spacing.y = 1.0;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if let Some((who, c)) = &l.who {
            ui.label(bold(format!("<{who}>")).color(*c));
        }
        ui.label(RichText::new(&l.text).color(l.color));
    });
}

/// How to show a file name from another user. napster-rs clients send
/// `Folder\\Song.mp3`, but older clients send full paths like
/// `C:\\Users\\...\\Song.mp3`; show just the file name for those.
fn display_name(name: &str) -> &str {
    let b = name.as_bytes();
    let absolute = name.starts_with(['/', '\\']) || (b.len() > 2 && b[1] == b':' && b[0].is_ascii_alphabetic());
    if absolute {
        napster_proto::share::basename(name)
    } else {
        name
    }
}

fn status_label(t: &TransferInfo) -> String {
    match (&t.state, t.direction) {
        (TransferState::Queued, _) => "Queued".into(),
        (TransferState::Requesting, _) => "Getting Info...".into(),
        (TransferState::Connecting, _) => "Connecting...".into(),
        (TransferState::WaitingForPush, _) => "Waiting through Firewall".into(),
        (TransferState::Transferring, Direction::Download) => "Downloading...".into(),
        (TransferState::Transferring, Direction::Upload) => "Uploading...".into(),
        (TransferState::Complete, _) => "File Complete!".into(),
        (TransferState::Cancelled, _) => "Cancelled".into(),
        (TransferState::Failed(why), _) => why.clone(),
    }
}

fn short_speed(s: u32) -> &'static str {
    ["Unknown", "14.4", "28.8", "33.6", "56K", "ISDN-64", "ISDN-128", "Cable", "DSL", "T1", "T3"]
        .get(s as usize)
        .copied()
        .unwrap_or("Unknown")
}

fn dash(n: u32) -> String {
    if n == 0 {
        "-".into()
    } else {
        n.to_string()
    }
}

fn duration(secs: u32) -> String {
    if secs == 0 {
        return "-".into();
    }
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn hms(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn clock() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    hms(now % 86400)
}

/// Open a folder, or play an audio file. Anything else (an `.exe` someone
/// passed off as a song, say) is only shown in its folder, never run.
fn open_path(p: &std::path::Path) {
    if p.is_file() && !napster_core::is_audio_file(&p.to_string_lossy()) {
        return reveal(p);
    }
    let _ = std::fs::create_dir_all(if p.is_dir() || p.extension().is_none() { p } else { p.parent().unwrap_or(p) });
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(p).spawn();
    if let Err(e) = r {
        tracing::warn!("open {}: {e}", p.display());
    }
}

/// Show a file selected in its folder without opening it.
fn reveal(p: &std::path::Path) {
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(p.parent().unwrap_or(p)).spawn();
    if let Err(e) = r {
        tracing::warn!("reveal {}: {e}", p.display());
    }
}

const HELP: &[(&str, &str)] = &[
    (
        "Getting Started",
        "Choose File > Preferences to set your user name, password, the server to connect to, and the folders \
         you want to share. Then use File > Connect. New user names are registered automatically the first \
         time you log in.",
    ),
    (
        "Search",
        "Type an artist and/or song title and click Find It!. The Advanced Fields narrow results by bitrate, \
         frequency and the sharer's line speed. Double-click a result, or select several and click Get Selected \
         Song(s), to download. Right-click a result for more options.",
    ),
    (
        "Transfer",
        "Downloads are listed on top and uploads below. Partial downloads are kept in the Incomplete folder and \
         resume automatically. Select a transfer and click Cancel to stop it; double-click a finished download to \
         open it.",
    ),
    (
        "Hot List",
        "Add users to your Hot List to see when they are online. Double-click a user to browse the files they are \
         sharing.",
    ),
    (
        "Chat",
        "Join rooms from Discover (or Join Chat Rooms...). Commands: /join <room>, /part, /msg <user> <text>, \
         /me <action>, /topic <text>, /whois <user>, /ping <user>, /serverping, /browse <user>, /ignore <user>, \
         /unignore <user>, /hotlist <user>, /clear, /raw <numeric> <data>.",
    ),
    (
        "Firewalls",
        "Other users connect to your Data Port (6699 by default) to download your files. If they can't, set the \
         Data Port to 0: you can still share, and your uploads are pushed out instead. Two firewalled users \
         cannot transfer files with each other.",
    ),
];

#[cfg(test)]
mod tests {
    #[test]
    fn hides_absolute_paths() {
        assert_eq!(super::display_name("Music\\Band\\Song.mp3"), "Music\\Band\\Song.mp3");
        assert_eq!(super::display_name("C:\\Users\\bob\\Music\\Song.mp3"), "Song.mp3");
        assert_eq!(super::display_name("/home/bob/Song.mp3"), "Song.mp3");
    }
}
