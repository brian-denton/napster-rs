//! The main window. Tabs mirror Napster 2.0: Chat, Library (My Files),
//! Search, Hot List, Transfer and Discover, plus a Home console.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use napster_core::{
    ChannelUser, Client, ClientConfig, Command, ConnState, Direction, Event, TransferInfo, TransferState,
};
use napster_proto::{link_speed_name, Compare, FileInfo, SearchQuery, SearchResult, LINK_SPEEDS};
use tokio::sync::mpsc::UnboundedReceiver;

const ACCENT: Color32 = Color32::from_rgb(0x5a, 0xb4, 0xff);
const SYSTEM: Color32 = Color32::from_rgb(0xe6, 0xc0, 0x4a);
const ERROR: Color32 = Color32::from_rgb(0xff, 0x6a, 0x6a);
const EMOTE: Color32 = Color32::from_rgb(0xc8, 0x8c, 0xff);
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
}

#[derive(Clone)]
struct Line {
    color: Option<Color32>,
    who: Option<String>,
    text: String,
}

impl Line {
    fn sys(text: impl Into<String>) -> Self {
        Line { color: Some(SYSTEM), who: None, text: text.into() }
    }
    fn err(text: impl Into<String>) -> Self {
        Line { color: Some(ERROR), who: None, text: text.into() }
    }
    fn said(who: &str, text: impl Into<String>) -> Self {
        Line { color: None, who: Some(who.to_owned()), text: text.into() }
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
    lines: Vec<Line>,
    input: String,
    unread: bool,
}

impl ChatView {
    fn new(title: &str) -> Self {
        Self { title: title.into(), topic: String::new(), users: vec![], lines: vec![], input: String::new(), unread: false }
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

struct SearchView {
    artist: String,
    title: String,
    max_results: u32,
    advanced: bool,
    bitrate: Option<(Compare, u32)>,
    speed: Option<(Compare, u32)>,
    freq: Option<(Compare, u32)>,
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
    library: Vec<FileInfo>,
    library_filter: String,
    scanning: Option<(usize, usize)>,
    search: SearchView,
    browse: Option<BrowseView>,
    transfers: BTreeMap<u64, TransferInfo>,
    chats: BTreeMap<ChatKey, ChatView>,
    active_chat: Option<ChatKey>,
    channel_list: Vec<(String, u32, String)>,
    channel_filter: String,
    new_channel: String,
    /// lower nick -> (display nick, link speed if online)
    hotlist: BTreeMap<String, (String, Option<u32>)>,
    hotlist_input: String,
    whois: Option<Vec<(String, String)>>,
    prefs: Option<ClientConfig>,
    new_shared_dir: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, config_path: PathBuf) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let config = ClientConfig::load(&config_path).unwrap_or_default();
        let ctx = cc.egui_ctx.clone();
        let (client, events) = napster_core::Client::start(config.clone(), Some(Arc::new(move || ctx.request_repaint())));
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
            console: vec![
                Line::sys(format!("{} - a Rust rewrite of Napster v2.0 BETA 10", napster_core::CLIENT_INFO)),
                Line::sys("Use File > Connect (or the Connect button) to connect to a Napster Server!"),
            ],
            console_input: String::new(),
            library: vec![],
            library_filter: String::new(),
            scanning: None,
            search: SearchView {
                artist: String::new(),
                title: String::new(),
                max_results: 100,
                advanced: false,
                bitrate: None,
                speed: None,
                freq: None,
                results: vec![],
                searching: false,
                selected: HashSet::new(),
                sort: (SortCol::File, true),
            },
            browse: None,
            transfers: BTreeMap::new(),
            chats: BTreeMap::new(),
            active_chat: None,
            channel_list: vec![],
            channel_filter: String::new(),
            new_channel: String::new(),
            hotlist,
            hotlist_input: String::new(),
            whois: None,
            prefs: None,
            new_shared_dir: String::new(),
        };
        if first_run {
            app.console.push(Line::sys("Welcome! Pick a user name and password in Preferences to get started."));
            app.prefs = Some(config);
        } else if config.auto_connect && !config.password.is_empty() {
            app.client.send(Command::Connect);
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
            self.log(Line::err(format!("WARNING: could not save settings: {e}")));
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
                    ConnState::Online { nick } => {
                        self.log(Line::sys(format!("Connected to Server as {nick}!")));
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
                    }
                    ConnState::Connecting => {}
                }
                if s == ConnState::Disconnected {
                    self.status = "Offline".into();
                }
                self.conn = s;
            }
            Event::Status(s) => {
                self.log(Line::sys(s.clone()));
                self.status = s;
            }
            Event::Info(s) => self.log(Line::sys(s)),
            Event::Error(s) => {
                // route channel errors to the active chat too
                if let Some(key) = &self.active_chat {
                    if self.tab == Tab::Chat {
                        if let Some(c) = self.chats.get_mut(key) {
                            c.push(Line::err(s.clone()));
                        }
                    }
                }
                if self.browse.as_ref().is_some_and(|b| !b.done) && s.contains("not online") {
                    if let Some(b) = &mut self.browse {
                        b.done = true;
                    }
                }
                self.log(Line::err(s));
            }
            Event::Motd(s) => self.log(Line { color: Some(ACCENT), who: None, text: s }),
            Event::Stats { users, files, gigabytes } => self.stats = (users, files, gigabytes),
            Event::LibraryScanning { done, total } => {
                self.scanning = (done < total).then_some((done, total));
            }
            Event::Library(files) => {
                self.scanning = None;
                self.log(Line::sys(format!("Library: {} shareable files.", files.len())));
                self.library = files;
            }
            Event::SearchResult(r) => {
                if self.search.searching {
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
                    self.log(Line::sys(format!("Download complete: {}", t.basename())));
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
                view.push(Line::said(&from, text));
                view.unread |= !active;
                if self.active_chat.is_none() {
                    self.active_chat = Some(key);
                }
            }
            Event::Joined { channel } => {
                let key = ChatKey::Channel(channel.to_lowercase());
                self.chat(key.clone(), &channel).push(Line::sys(format!("Joined {channel}.")));
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
                v.push(Line::sys(format!(
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
                v.push(Line::sys(format!("* {nick} has left.")));
                v.users.retain(|u| !u.nick.eq_ignore_ascii_case(&nick));
            }
            Event::ChannelMessage { channel, nick, text } => {
                let key = ChatKey::Channel(channel.to_lowercase());
                let active = self.active_chat.as_ref() == Some(&key) && self.tab == Tab::Chat;
                let v = self.chat(key, &channel);
                v.push(Line::said(&nick, text));
                v.unread |= !active;
            }
            Event::ChannelEmote { channel, nick, text } => {
                let v = self.chat(ChatKey::Channel(channel.to_lowercase()), &channel);
                v.push(Line { color: Some(EMOTE), who: None, text: format!("* {nick} {text}") });
            }
            Event::Topic { channel, topic } => {
                let v = self.chat(ChatKey::Channel(channel.to_lowercase()), &channel);
                v.push(Line::sys(format!("Topic: {topic}")));
                v.topic = topic;
            }
            Event::ChannelList(list) => self.channel_list = list,
            Event::HotlistOnline { nick, link_speed } => {
                self.hotlist.entry(nick.to_lowercase()).or_insert_with(|| (nick.clone(), None)).1 = Some(link_speed);
            }
            Event::HotlistOffline { nick } => {
                if let Some(e) = self.hotlist.get_mut(&nick.to_lowercase()) {
                    e.1 = None;
                }
            }
            Event::Whois(fields) => self.whois = Some(fields),
            Event::Pong { nick } => self.log(Line::sys(format!("Pong from {nick}!"))),
            Event::ServerPong { millis } => self.log(Line::sys(format!("Received pong from Server: {millis} ms"))),
            Event::Announcement { from, text } => {
                self.log(Line { color: Some(ACCENT), who: None, text: format!("* [Announcement] {from}: {text}") })
            }
            Event::ChannelInvite { from, channel } => {
                self.log(Line::sys(format!("User {from} has invited you to the chat room {channel}! (/join {channel})")))
            }
        }
    }

    // ------------------------------------------------------------- actions

    fn connect(&mut self) {
        if self.config.nick.is_empty() || self.config.password.is_empty() {
            self.prefs = Some(self.config.clone());
            self.log(Line::err("Set a user name and password first."));
            return;
        }
        self.client.send(Command::Connect);
    }

    fn start_search(&mut self) {
        let terms: Vec<String> =
            [&self.search.artist, &self.search.title].iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
        if terms.is_empty() {
            self.log(Line::err("Must specify a song title or artist to search!"));
            return;
        }
        let q = SearchQuery {
            terms,
            max_results: self.search.max_results,
            link_speed: self.search.speed.filter(|_| self.search.advanced),
            bitrate: self.search.bitrate.filter(|_| self.search.advanced),
            frequency: self.search.freq.filter(|_| self.search.advanced),
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
        self.browse = Some(BrowseView { nick: nick.to_owned(), results: vec![], done: false, selected: HashSet::new() });
        self.client.send(Command::Browse(nick.to_owned()));
        self.tab = Tab::HotList;
        self.log(Line::sys(format!("Populating list for User: {nick} ...")));
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
                Some(ChatKey::Channel(c)) => self.chats.get(&ChatKey::Channel(c.clone())).map(|v| v.title.clone()),
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
                    None => push_here(self, Line::err("Proper syntax: /topic <Topic> (from a channel window)")),
                },
                "whois" | "finger" if !arg1.is_empty() => self.client.send(Command::Whois(arg1)),
                "ping" if !arg1.is_empty() => {
                    push_here(self, Line::sys(format!("Ping to {arg1}...")));
                    self.client.send(Command::Ping(arg1));
                }
                "serverping" => self.client.send(Command::PingServer),
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
                "help" => {
                    for l in [
                        "/join <channel>  /part [channel]  /msg <user> <text>  /me <action>",
                        "/topic <text>  /whois <user>  /ping <user>  /serverping  /browse <user>",
                        "/ignore <user>  /unignore <user>  /hotlist <user>  /clear  /raw <num> <data>",
                    ] {
                        push_here(self, Line::sys(l));
                    }
                }
                _ => push_here(self, Line::err(format!("Unknown Command '{verb}'! (try /help)"))),
            }
            return;
        }
        match target {
            Some(ChatKey::Channel(c)) => {
                let name = self.chats.get(&ChatKey::Channel(c)).map(|v| v.title.clone()).unwrap_or_default();
                self.client.send(Command::Say { channel: name, text });
            }
            Some(ChatKey::Private(n)) => {
                let nick = self.chats.get(&ChatKey::Private(n)).map(|v| v.title.clone()).unwrap_or_default();
                self.send_private(&nick, text);
            }
            None => self.log(Line::err("Type /help for commands, or open the Chat tab to talk.")),
        }
    }

    fn send_private(&mut self, nick: &str, text: String) {
        let me = self.my_nick().to_owned();
        self.chat(ChatKey::Private(nick.to_lowercase()), nick).push(Line::said(&me, text.clone()));
        self.client.send(Command::PrivateMessage { to: nick.to_owned(), text });
    }

    // ------------------------------------------------------------------ ui

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
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
                    if ui.button("Rescan My Files").clicked() {
                        self.client.send(Command::Rescan);
                        ui.close_menu();
                    }
                    if ui.button("Ping Server").clicked() {
                        self.client.send(Command::PingServer);
                        ui.close_menu();
                    }
                    if ui.button("Clear Finished Transfers").clicked() {
                        self.client.send(Command::ClearFinished);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    ui.label(format!("{} — Rust rewrite of Napster v2.0 BETA 10", napster_core::CLIENT_INFO));
                    ui.label("Type /help in any chat box for commands.");
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match &self.conn {
                        ConnState::Online { .. } => {
                            if ui.button("Disconnect").clicked() {
                                self.client.send(Command::Disconnect);
                            }
                            ui.label(RichText::new("● Online").color(Color32::LIGHT_GREEN));
                        }
                        ConnState::Connecting => {
                            ui.spinner();
                            ui.label("Connecting...");
                        }
                        ConnState::Disconnected => {
                            if ui.button("Connect").clicked() {
                                self.connect();
                            }
                            ui.label(RichText::new("● Offline").color(Color32::GRAY));
                        }
                    }
                });
            });
        });
    }

    fn tab_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("napster").color(ACCENT).strong());
                ui.add_space(12.0);
                let unread = self.chats.values().any(|c| c.unread);
                let downloads = self.transfers.values().filter(|t| !t.state.is_finished()).count();
                let tabs = [
                    (Tab::Home, "Home".to_owned()),
                    (Tab::Chat, if unread { "Chat •".into() } else { "Chat".into() }),
                    (Tab::Library, "Library".into()),
                    (Tab::Search, "Search".into()),
                    (Tab::HotList, "Hot List".into()),
                    (Tab::Transfer, if downloads > 0 { format!("Transfer ({downloads})") } else { "Transfer".into() }),
                    (Tab::Discover, "Discover".into()),
                ];
                for (tab, label) in tabs {
                    if ui.selectable_label(self.tab == tab, RichText::new(label).size(15.0)).clicked() {
                        self.tab = tab;
                        if tab == Tab::Discover && self.online() {
                            self.client.send(Command::ListChannels);
                        }
                    }
                }
            });
            ui.add_space(4.0);
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                if let Some((d, t)) = self.scanning {
                    ui.separator();
                    ui.spinner();
                    ui.label(format!("Scanning {d} of {t}"));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.online() {
                        let (u, f, g) = self.stats;
                        ui.label(format!("Currently {u} users sharing {f} files ({g} gigs)"));
                    }
                });
            });
        });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        ui.heading("Console");
        ui.separator();
        let input_h = 32.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .stick_to_bottom(true)
            .max_height(ui.available_height() - input_h)
            .show(ui, |ui| {
                for l in &self.console {
                    line_ui(ui, l);
                }
            });
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.console_input)
                    .hint_text("/help for commands")
                    .desired_width(f32::INFINITY),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let text = std::mem::take(&mut self.console_input);
                self.submit(None, text);
                r.request_focus();
            }
        });
    }

    fn chat_tab(&mut self, ui: &mut egui::Ui) {
        if self.chats.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label("You are not in any chat rooms.");
                if ui.button("Join Chat Rooms").clicked() {
                    self.tab = Tab::Discover;
                    self.client.send(Command::ListChannels);
                }
            });
            return;
        }
        if self.active_chat.as_ref().is_none_or(|k| !self.chats.contains_key(k)) {
            self.active_chat = self.chats.keys().next().cloned();
        }
        // room tabs
        ui.horizontal_wrapped(|ui| {
            let keys: Vec<ChatKey> = self.chats.keys().cloned().collect();
            for k in keys {
                let v = &self.chats[&k];
                let mut label = match &k {
                    ChatKey::Channel(_) => v.title.clone(),
                    ChatKey::Private(_) => format!("✉ {}", v.title),
                };
                if v.unread {
                    label.push_str(" •");
                }
                let selected = self.active_chat.as_ref() == Some(&k);
                if ui.selectable_label(selected, label).clicked() {
                    self.active_chat = Some(k.clone());
                }
            }
        });
        ui.separator();
        let Some(key) = self.active_chat.clone() else { return };
        if let Some(v) = self.chats.get_mut(&key) {
            v.unread = false;
        }
        let is_channel = matches!(key, ChatKey::Channel(_));
        let mut action: Option<(String, &'static str)> = None;
        if is_channel {
            egui::SidePanel::right("chat_users").resizable(true).default_width(180.0).show_inside(ui, |ui| {
                let v = &self.chats[&key];
                ui.strong(format!("{} users", v.users.len()));
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    for u in &v.users {
                        let r = ui.selectable_label(false, &u.nick).on_hover_text(format!(
                            "{} — sharing {} files",
                            link_speed_name(u.link_speed),
                            u.shared
                        ));
                        r.context_menu(|ui| {
                            for (label, what) in [
                                ("Instant Message", "msg"),
                                ("Browse Files", "browse"),
                                ("View User Information", "whois"),
                                ("Add to Hot List", "hotlist"),
                                ("Ping", "ping"),
                                ("Ignore", "ignore"),
                            ] {
                                if ui.button(label).clicked() {
                                    action = Some((u.nick.clone(), what));
                                    ui.close_menu();
                                }
                            }
                        });
                        if r.double_clicked() {
                            action = Some((u.nick.clone(), "msg"));
                        }
                    }
                });
            });
        }
        let v = self.chats.get_mut(&key).unwrap();
        ui.horizontal(|ui| {
            ui.strong(&v.title);
            if !v.topic.is_empty() {
                ui.label(RichText::new(&v.topic).italics());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if is_channel { "Leave" } else { "Close" };
                if ui.button(label).clicked() {
                    action = Some((v.title.clone(), if is_channel { "part" } else { "close" }));
                }
            });
        });
        let input_h = 32.0;
        egui::ScrollArea::vertical()
            .id_salt(("chat", v.title.clone()))
            .auto_shrink([false; 2])
            .stick_to_bottom(true)
            .max_height(ui.available_height() - input_h)
            .show(ui, |ui| {
                for l in &v.lines {
                    line_ui(ui, l);
                }
            });
        let mut submitted = None;
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut v.input).desired_width(f32::INFINITY).hint_text("Say something..."));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submitted = Some(std::mem::take(&mut v.input));
                r.request_focus();
            }
        });
        if let Some(text) = submitted {
            self.submit(Some(key.clone()), text);
        }
        if let Some((nick, what)) = action {
            self.user_action(&nick, what);
        }
    }

    fn user_action(&mut self, nick: &str, what: &str) {
        match what {
            "msg" => self.open_private(nick),
            "browse" => self.browse(nick),
            "whois" => self.client.send(Command::Whois(nick.to_owned())),
            "hotlist" => self.hotlist_add(nick),
            "ping" => self.client.send(Command::Ping(nick.to_owned())),
            "ignore" => self.client.send(Command::Ignore(nick.to_owned())),
            "part" => self.client.send(Command::Part(nick.to_owned())),
            "close" => {
                self.chats.remove(&ChatKey::Private(nick.to_lowercase()));
                self.active_chat = None;
            }
            _ => {}
        }
    }

    fn library_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("My Files");
            ui.label(format!("{} files shared", self.library.len()));
            if ui.button("Rescan").clicked() {
                self.client.send(Command::Rescan);
            }
            if ui.button("Shared Folders...").clicked() {
                self.prefs = Some(self.config.clone());
            }
            if ui.button("Open Downloads").clicked() {
                open_path(&self.config.download_dir);
            }
            ui.add(egui::TextEdit::singleline(&mut self.library_filter).hint_text("filter").desired_width(200.0));
        });
        if let Some((d, t)) = self.scanning {
            ui.add(egui::ProgressBar::new(d as f32 / t.max(1) as f32).text(format!("Building My Files... {d}/{t}")));
        }
        ui.separator();
        let filter = self.library_filter.to_lowercase();
        let files: Vec<&FileInfo> =
            self.library.iter().filter(|f| filter.is_empty() || f.filename.to_lowercase().contains(&filter)).collect();
        if files.is_empty() {
            ui.label("No Files shared. Add folders under Preferences > Shared Folders.");
            return;
        }
        let mut open: Option<PathBuf> = None;
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .column(Column::remainder().at_least(240.0).clip(true))
            .column(Column::auto().at_least(70.0))
            .column(Column::auto().at_least(60.0))
            .column(Column::auto().at_least(60.0))
            .column(Column::auto().at_least(60.0))
            .header(20.0, |mut h| {
                for t in ["Filename", "Size", "Bitrate", "Freq", "Length"] {
                    h.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, files.len(), |mut row| {
                    let f = files[row.index()];
                    row.col(|ui| {
                        ui.label(&f.filename);
                    });
                    row.col(|ui| {
                        ui.label(human_size(f.size));
                    });
                    row.col(|ui| {
                        ui.label(if f.bitrate > 0 { format!("{}", f.bitrate) } else { "-".into() });
                    });
                    row.col(|ui| {
                        ui.label(if f.frequency > 0 { format!("{}", f.frequency) } else { "-".into() });
                    });
                    row.col(|ui| {
                        ui.label(duration(f.seconds));
                    });
                    if row.response().double_clicked() {
                        open = Some(PathBuf::from(&f.filename));
                    }
                });
            });
        if let Some(p) = open {
            open_path(&p);
        }
    }

    fn search_tab(&mut self, ui: &mut egui::Ui) {
        let mut go = false;
        egui::Grid::new("search_form").num_columns(4).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Artist:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.search.artist).desired_width(260.0));
            go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.label("Max Results:");
            egui::ComboBox::from_id_salt("max_results")
                .selected_text(self.search.max_results.to_string())
                .show_ui(ui, |ui| {
                    for n in [25, 50, 75, 100] {
                        ui.selectable_value(&mut self.search.max_results, n, n.to_string());
                    }
                });
            ui.end_row();
            ui.label("Title:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.search.title).desired_width(260.0));
            go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if self.search.searching {
                if ui.button("Cancel").clicked() {
                    self.search.searching = false;
                }
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Searching...");
                });
            } else {
                go |= ui.button(RichText::new("Find It!").strong()).clicked();
                ui.label("");
            }
            ui.end_row();
        });
        ui.checkbox(&mut self.search.advanced, "Advanced");
        if self.search.advanced {
            ui.horizontal(|ui| {
                compare_filter(ui, "Bitrate", "br", &mut self.search.bitrate, &[56, 64, 96, 112, 128, 160, 192, 256, 320], |v| {
                    format!("{v} Kb/s")
                });
                compare_filter(ui, "Line Speed", "ls", &mut self.search.speed, &(0..=10).collect::<Vec<_>>(), |v| {
                    link_speed_name(v).to_owned()
                });
                compare_filter(ui, "Frequency", "fq", &mut self.search.freq, &[16000, 22050, 32000, 44100, 48000], |v| {
                    format!("{v} Hz")
                });
            });
        }
        if go && !self.search.searching {
            self.start_search();
        }
        ui.separator();
        let n = self.search.results.len();
        ui.horizontal(|ui| {
            ui.label(if self.search.searching { format!("{n} results so far...") } else { format!("Returned {n} results.") });
            let sel = self.search.selected.len();
            if ui.add_enabled(sel > 0, egui::Button::new(format!("Get Selected Files ({sel})"))).clicked() {
                let picks: Vec<SearchResult> = self.search.selected.iter().map(|&i| self.search.results[i].clone()).collect();
                for r in picks {
                    self.download(&r);
                }
                self.search.selected.clear();
            }
        });
        let mut clicked_header: Option<SortCol> = None;
        let mut act: Option<(usize, &'static str)> = None;
        let sort = self.search.sort;
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .column(Column::remainder().at_least(220.0).clip(true))
            .column(Column::auto().at_least(70.0))
            .column(Column::auto().at_least(50.0))
            .column(Column::auto().at_least(55.0))
            .column(Column::auto().at_least(55.0))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(80.0))
            .header(20.0, |mut h| {
                for (t, c) in [
                    ("Filename", SortCol::File),
                    ("Filesize", SortCol::Size),
                    ("Bitrate", SortCol::Bitrate),
                    ("Freq", SortCol::Freq),
                    ("Length", SortCol::Length),
                    ("User", SortCol::User),
                    ("Connection", SortCol::Speed),
                ] {
                    h.col(|ui| {
                        let arrow = if sort.0 == c { if sort.1 { " ▲" } else { " ▼" } } else { "" };
                        if ui.add(egui::Label::new(RichText::new(format!("{t}{arrow}")).strong()).sense(egui::Sense::click())).clicked() {
                            clicked_header = Some(c);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, n, |mut row| {
                    let i = row.index();
                    let r = &self.search.results[i];
                    row.set_selected(self.search.selected.contains(&i));
                    row.col(|ui| {
                        ui.label(r.file.basename()).on_hover_text(&r.file.filename);
                    });
                    row.col(|ui| {
                        ui.label(human_size(r.file.size));
                    });
                    row.col(|ui| {
                        ui.label(r.file.bitrate.to_string());
                    });
                    row.col(|ui| {
                        ui.label(r.file.frequency.to_string());
                    });
                    row.col(|ui| {
                        ui.label(duration(r.file.seconds));
                    });
                    row.col(|ui| {
                        ui.label(&r.nick);
                    });
                    row.col(|ui| {
                        ui.label(link_speed_name(r.link_speed));
                    });
                    let resp = row.response();
                    if resp.double_clicked() {
                        act = Some((i, "get"));
                    } else if resp.clicked() {
                        act = Some((i, "select"));
                    }
                    resp.context_menu(|ui| {
                        for (label, what) in [
                            ("Get File", "get"),
                            ("Browse User's Files", "browse"),
                            ("Add User to Hot List", "hotlist"),
                            ("Instant Message", "msg"),
                            ("View User Information", "whois"),
                        ] {
                            if ui.button(label).clicked() {
                                act = Some((i, what));
                                ui.close_menu();
                            }
                        }
                    });
                });
            });
        if let Some(c) = clicked_header {
            self.search.sort = if self.search.sort.0 == c { (c, !self.search.sort.1) } else { (c, true) };
            self.sort_results();
        }
        if let Some((i, what)) = act {
            let ctrl = ui.input(|inp| inp.modifiers.command || inp.modifiers.shift);
            match what {
                "select" => {
                    if !ctrl {
                        self.search.selected.clear();
                    }
                    if !self.search.selected.remove(&i) {
                        self.search.selected.insert(i);
                    }
                }
                "get" => {
                    let r = self.search.results[i].clone();
                    self.download(&r);
                }
                other => {
                    let nick = self.search.results[i].nick.clone();
                    self.user_action(&nick, other);
                }
            }
        }
    }

    fn hotlist_tab(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<(String, &'static str)> = None;
        egui::SidePanel::left("hotlist_users").resizable(true).default_width(220.0).show_inside(ui, |ui| {
            ui.strong("Hot List");
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.hotlist_input).hint_text("user name").desired_width(120.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Add").clicked() || enter {
                    let n = std::mem::take(&mut self.hotlist_input);
                    action = Some((n, "hotlist"));
                }
            });
            ui.separator();
            for (nick, online) in self.hotlist.values() {
                let (dot, color) = match online {
                    Some(_) => ("●", Color32::LIGHT_GREEN),
                    None => ("○", Color32::GRAY),
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(dot).color(color));
                    let r = ui.selectable_label(false, nick);
                    let r = match online {
                        Some(speed) => r.on_hover_text(link_speed_name(*speed)),
                        None => r.on_hover_text("offline"),
                    };
                    if r.double_clicked() {
                        action = Some((nick.clone(), "browse"));
                    }
                    r.context_menu(|ui| {
                        for (label, what) in [
                            ("Browse Files", "browse"),
                            ("Instant Message", "msg"),
                            ("View User Information", "whois"),
                            ("Ping", "ping"),
                            ("Remove from Hot List", "remove"),
                        ] {
                            if ui.button(label).clicked() {
                                action = Some((nick.clone(), what));
                                ui.close_menu();
                            }
                        }
                    });
                });
            }
            if self.hotlist.is_empty() {
                ui.label("Add users to keep track of when they are online. Double-click to browse.");
            }
        });
        // browse results
        match &mut self.browse {
            None => {
                ui.label("Double-click a user to browse their files.");
            }
            Some(b) => {
                let mut get: Vec<SearchResult> = vec![];
                ui.horizontal(|ui| {
                    ui.heading(format!("{}'s files", b.nick));
                    if !b.done {
                        ui.spinner();
                        ui.label("Populating...");
                    } else {
                        ui.label(format!("{} files", b.results.len()));
                    }
                    let sel = b.selected.len();
                    if ui.add_enabled(sel > 0, egui::Button::new(format!("Get Selected Files ({sel})"))).clicked() {
                        get = b.selected.iter().map(|&i| b.results[i].clone()).collect();
                        b.selected.clear();
                    }
                });
                ui.separator();
                let mut clicked: Option<(usize, bool)> = None;
                TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .column(Column::remainder().at_least(240.0).clip(true))
                    .column(Column::auto().at_least(70.0))
                    .column(Column::auto().at_least(50.0))
                    .column(Column::auto().at_least(55.0))
                    .header(20.0, |mut h| {
                        for t in ["Filename", "Size", "Bitrate", "Length"] {
                            h.col(|ui| {
                                ui.strong(t);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(18.0, b.results.len(), |mut row| {
                            let i = row.index();
                            let r = &b.results[i];
                            row.set_selected(b.selected.contains(&i));
                            row.col(|ui| {
                                ui.label(&r.file.filename);
                            });
                            row.col(|ui| {
                                ui.label(human_size(r.file.size));
                            });
                            row.col(|ui| {
                                ui.label(r.file.bitrate.to_string());
                            });
                            row.col(|ui| {
                                ui.label(duration(r.file.seconds));
                            });
                            let resp = row.response();
                            if resp.double_clicked() {
                                clicked = Some((i, true));
                            } else if resp.clicked() {
                                clicked = Some((i, false));
                            }
                        });
                    });
                if let Some((i, dbl)) = clicked {
                    if dbl {
                        get.push(b.results[i].clone());
                    } else if !b.selected.remove(&i) {
                        b.selected.insert(i);
                    }
                }
                for r in get {
                    self.download(&r);
                }
            }
        }
        if let Some((nick, what)) = action {
            match what {
                "remove" => self.hotlist_remove(&nick),
                other => self.user_action(&nick, other),
            }
        }
    }

    fn transfer_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Clear Finished").clicked() {
                self.client.send(Command::ClearFinished);
            }
            if ui.button("Open Downloads Folder").clicked() {
                open_path(&self.config.download_dir);
            }
        });
        let half = (ui.available_height() - 40.0) / 2.0;
        let mut cancel: Option<u64> = None;
        let mut retry: Option<TransferInfo> = None;
        for (dir, title) in [(Direction::Download, "Downloads"), (Direction::Upload, "Uploads")] {
            let rows: Vec<&TransferInfo> = self.transfers.values().filter(|t| t.direction == dir).collect();
            let active = rows.iter().filter(|t| !t.state.is_finished()).count();
            ui.separator();
            ui.strong(format!("{title} ({active} active)"));
            ui.push_id(title, |ui| {
                TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .max_scroll_height(half)
                    .column(Column::remainder().at_least(200.0).clip(true))
                    .column(Column::auto().at_least(90.0))
                    .column(Column::auto().at_least(150.0))
                    .column(Column::auto().at_least(80.0))
                    .column(Column::auto().at_least(150.0))
                    .column(Column::auto().at_least(60.0))
                    .header(20.0, |mut h| {
                        for t in ["Filename", "User", "Progress", "Rate", "Status", ""] {
                            h.col(|ui| {
                                ui.strong(t);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(20.0, rows.len(), |mut row| {
                            let t = rows[row.index()];
                            row.col(|ui| {
                                ui.label(t.basename()).on_hover_text(&t.filename);
                            });
                            row.col(|ui| {
                                ui.label(&t.nick);
                            });
                            row.col(|ui| {
                                ui.add(egui::ProgressBar::new(t.progress()).text(format!(
                                    "{} / {}",
                                    human_size(t.offset + t.transferred),
                                    human_size(t.size)
                                )));
                            });
                            row.col(|ui| {
                                if t.state == TransferState::Transferring {
                                    ui.label(format!("{}/s", human_size(t.rate() as u64)));
                                }
                            });
                            row.col(|ui| {
                                let color = match &t.state {
                                    TransferState::Complete => Color32::LIGHT_GREEN,
                                    TransferState::Failed(_) => ERROR,
                                    _ => ui.visuals().text_color(),
                                };
                                ui.label(RichText::new(t.state.label()).color(color)).on_hover_text(t.state.label());
                            });
                            row.col(|ui| {
                                if !t.state.is_finished() {
                                    if ui.small_button("Cancel").clicked() {
                                        cancel = Some(t.id);
                                    }
                                } else if t.direction == Direction::Download {
                                    if let Some(p) = &t.saved_to {
                                        if ui.small_button("Open").clicked() {
                                            open_path(p);
                                        }
                                    } else if ui.small_button("Retry").clicked() {
                                        retry = Some(t.clone());
                                    }
                                }
                            });
                        });
                    });
            });
        }
        if let Some(id) = cancel {
            self.client.send(Command::CancelTransfer(id));
        }
        if let Some(t) = retry {
            self.client.send(Command::Download {
                nick: t.nick.clone(),
                file: FileInfo { filename: t.filename.clone(), md5: t.md5.clone(), size: t.size, bitrate: 0, frequency: 0, seconds: 0 },
            });
        }
    }

    fn discover_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Chat Rooms");
            if ui.button("Refresh").clicked() {
                self.client.send(Command::ListChannels);
            }
            ui.add(egui::TextEdit::singleline(&mut self.channel_filter).hint_text("filter").desired_width(160.0));
            ui.separator();
            let r = ui.add(egui::TextEdit::singleline(&mut self.new_channel).hint_text("new room name").desired_width(140.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("Create / Join").clicked() || enter) && !self.new_channel.trim().is_empty() {
                let name = std::mem::take(&mut self.new_channel).trim().replace(' ', "_");
                self.client.send(Command::Join(name));
            }
        });
        if !self.online() {
            ui.label("Not connected to Server!");
            return;
        }
        ui.separator();
        let filter = self.channel_filter.to_lowercase();
        let rows: Vec<(String, u32, String)> = self
            .channel_list
            .iter()
            .filter(|(n, _, t)| filter.is_empty() || n.to_lowercase().contains(&filter) || t.to_lowercase().contains(&filter))
            .cloned()
            .collect();
        let mut join: Option<String> = None;
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .column(Column::auto().at_least(160.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::remainder().clip(true))
            .header(20.0, |mut h| {
                for t in ["Channel Name", "User Count", "Channel Topic"] {
                    h.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, rows.len(), |mut row| {
                    let (name, users, topic) = &rows[row.index()];
                    let joined = self.chats.contains_key(&ChatKey::Channel(name.to_lowercase()));
                    row.col(|ui| {
                        ui.label(if joined { RichText::new(name).strong().color(ACCENT) } else { RichText::new(name) });
                    });
                    row.col(|ui| {
                        ui.label(users.to_string());
                    });
                    row.col(|ui| {
                        ui.label(topic);
                    });
                    if row.response().double_clicked() {
                        join = Some(name.clone());
                    }
                });
            });
        ui.label("Double-click a room to join it.");
        if let Some(name) = join {
            self.client.send(Command::Join(name));
        }
    }

    fn whois_window(&mut self, ctx: &egui::Context) {
        let Some(fields) = &self.whois else { return };
        let title = fields.first().map(|(_, v)| format!("{v}'s Finger Information")).unwrap_or_default();
        let mut open = true;
        egui::Window::new(title).open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            egui::Grid::new("whois").num_columns(2).striped(true).show(ui, |ui| {
                for (k, v) in fields {
                    ui.strong(k);
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
        if !open {
            self.whois = None;
        }
    }

    fn prefs_window(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.prefs else { return };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Preferences").open(&mut open).collapsible(false).default_width(520.0).show(ctx, |ui| {
            egui::CollapsingHeader::new("Account").default_open(true).show(ui, |ui| {
                egui::Grid::new("acct").num_columns(2).show(ui, |ui| {
                    ui.label("User Name:");
                    ui.text_edit_singleline(&mut draft.nick);
                    ui.end_row();
                    ui.label("Password:");
                    ui.add(egui::TextEdit::singleline(&mut draft.password).password(true));
                    ui.end_row();
                    ui.label("Email:");
                    ui.text_edit_singleline(&mut draft.email);
                    ui.end_row();
                    ui.label("Connection:");
                    egui::ComboBox::from_id_salt("speed")
                        .selected_text(link_speed_name(draft.link_speed))
                        .show_ui(ui, |ui| {
                            for (i, name) in LINK_SPEEDS.iter().enumerate() {
                                ui.selectable_value(&mut draft.link_speed, i as u32, *name);
                            }
                        });
                    ui.end_row();
                });
                if !draft.nick.is_empty() && !napster_proto::valid_nick(&draft.nick) {
                    ui.colored_label(ERROR, "User names may use letters, digits and _[]{}-@^!$");
                }
                ui.label(RichText::new("New user names are registered automatically on first login.").weak());
            });
            egui::CollapsingHeader::new("Server").default_open(true).show(ui, |ui| {
                egui::Grid::new("srv").num_columns(2).show(ui, |ui| {
                    ui.label("Server (host:port):");
                    ui.text_edit_singleline(&mut draft.server);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut draft.use_metaserver, "This is a metaserver (port 8875 redirector)");
                    ui.end_row();
                    ui.label("Data port:");
                    ui.add(egui::DragValue::new(&mut draft.data_port).range(0..=65535));
                    ui.end_row();
                });
                ui.label(RichText::new("Data port 0 = behind a firewall (others can't connect to you; uploads are pushed).").weak());
                ui.checkbox(&mut draft.auto_connect, "Connect automatically when Napster starts");
            });
            egui::CollapsingHeader::new("Shared Folders").default_open(true).show(ui, |ui| {
                let mut remove = None;
                for (i, d) in draft.shared_dirs.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("✖").clicked() {
                            remove = Some(i);
                        }
                        ui.label(d.display().to_string());
                    });
                }
                if let Some(i) = remove {
                    draft.shared_dirs.remove(i);
                }
                ui.horizontal(|ui| {
                    if ui.button("Add Folder...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            draft.shared_dirs.push(p);
                        }
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.new_shared_dir).hint_text("or type a path").desired_width(220.0));
                    if ui.button("Add").clicked() && !self.new_shared_dir.trim().is_empty() {
                        draft.shared_dirs.push(PathBuf::from(std::mem::take(&mut self.new_shared_dir).trim()));
                    }
                });
            });
            egui::CollapsingHeader::new("Transfers").default_open(true).show(ui, |ui| {
                egui::Grid::new("xfer").num_columns(2).show(ui, |ui| {
                    ui.label("Download folder:");
                    ui.horizontal(|ui| {
                        ui.label(draft.download_dir.display().to_string());
                        if ui.small_button("Change...").clicked() {
                            if let Some(p) = rfd::FileDialog::new().set_directory(&draft.download_dir).pick_folder() {
                                draft.download_dir = p;
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("Max simultaneous downloads:");
                    ui.add(egui::DragValue::new(&mut draft.max_downloads).range(1..=50));
                    ui.end_row();
                    ui.label("Max simultaneous uploads:");
                    ui.add(egui::DragValue::new(&mut draft.max_uploads).range(0..=50));
                    ui.end_row();
                });
                if draft.max_uploads == 0 {
                    ui.colored_label(SYSTEM, "Your file server is disabled (max uploads = 0).");
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if save {
            let draft = self.prefs.take().unwrap();
            self.config = draft;
            self.save_config();
            self.client.send(Command::SetConfig(self.config.clone()));
            self.log(Line::sys("Preferences saved."));
        } else if cancel || !open {
            self.prefs = None;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.menu_bar(ctx);
        self.tab_bar(ctx);
        self.status_bar(ctx);
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Home => self.home(ui),
            Tab::Chat => self.chat_tab(ui),
            Tab::Library => self.library_tab(ui),
            Tab::Search => self.search_tab(ui),
            Tab::HotList => self.hotlist_tab(ui),
            Tab::Transfer => self.transfer_tab(ui),
            Tab::Discover => self.discover_tab(ui),
        });
        self.whois_window(ctx);
        self.prefs_window(ctx);
        // keep transfer rates ticking
        if self.transfers.values().any(|t| t.state == TransferState::Transferring) {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.client.send(Command::Disconnect);
    }
}

fn line_ui(ui: &mut egui::Ui, l: &Line) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if let Some(who) = &l.who {
            ui.label(RichText::new(format!("<{who}>")).color(ACCENT).strong());
        }
        match l.color {
            Some(c) => ui.label(RichText::new(&l.text).color(c)),
            None => ui.label(&l.text),
        };
    });
}

fn compare_filter(
    ui: &mut egui::Ui,
    label: &str,
    id: &str,
    value: &mut Option<(Compare, u32)>,
    choices: &[u32],
    fmt: impl Fn(u32) -> String,
) {
    ui.group(|ui| {
        let mut on = value.is_some();
        if ui.checkbox(&mut on, label).changed() {
            *value = on.then(|| (Compare::AtLeast, choices[choices.len() / 2]));
        }
        if let Some((cmp, v)) = value {
            egui::ComboBox::from_id_salt((id, "cmp")).selected_text(cmp.as_str()).show_ui(ui, |ui| {
                for c in Compare::ALL {
                    ui.selectable_value(cmp, c, c.as_str());
                }
            });
            egui::ComboBox::from_id_salt((id, "val")).selected_text(fmt(*v)).show_ui(ui, |ui| {
                for &c in choices {
                    ui.selectable_value(v, c, fmt(c));
                }
            });
        }
    });
}

fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn duration(secs: u32) -> String {
    if secs == 0 {
        return "-".into();
    }
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn open_path(p: &std::path::Path) {
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
