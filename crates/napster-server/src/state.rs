//! In-memory server state.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::Ipv4Addr;
use std::time::Instant;

use napster_proto::{FileInfo, Message};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::db::{Ban, Db};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Level {
    Leech,
    User,
    Moderator,
    Admin,
    Elite,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Leech => "Leech",
            Level::User => "User",
            Level::Moderator => "Moderator",
            Level::Admin => "Admin",
            Level::Elite => "Elite",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "leech" | "0" => Some(Level::Leech),
            "user" | "1" => Some(Level::User),
            "moderator" | "mod" | "2" => Some(Level::Moderator),
            "admin" | "3" => Some(Level::Admin),
            "elite" | "4" => Some(Level::Elite),
            _ => None,
        }
    }
}

pub struct Session {
    /// distinguishes a ghosted connection from its replacement
    pub id: u64,
    pub nick: String,
    pub tx: UnboundedSender<Message>,
    pub kill: CancellationToken,
    pub ip: Ipv4Addr,
    pub remote_port: u16,
    pub data_port: u16,
    pub link_speed: u32,
    pub client: String,
    pub level: Level,
    pub email: String,
    pub files: Vec<FileInfo>,
    /// lower-case nicks this user wants signon/signoff notices for
    pub hotlist: HashSet<String>,
    /// lower-case nicks this user ignores
    pub ignored: BTreeSet<String>,
    /// lower-case channel keys
    pub channels: BTreeSet<String>,
    pub downloads: u32,
    pub uploads: u32,
    pub total_downloads: u32,
    pub total_uploads: u32,
    pub connected: Instant,
    pub muzzled: bool,
    pub cloaked: bool,
    /// Wall-clock login time; the earlier login wins a network nick collision.
    pub login_ms: u64,
    /// Share count last announced to linked servers.
    pub announced_shares: u32,
    /// Tags for shared files, by file name (napster-rs clients only).
    pub meta: HashMap<String, crate::discover::Meta>,
}

impl Session {
    pub fn send(&self, msg: Message) {
        let _ = self.tx.send(msg);
    }

    pub fn shared_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// `<nick> <shared> <speed>` as used in channel join/part/list messages.
    pub fn channel_entry(&self, channel: &str) -> String {
        format!("{} {} {} {}", channel, self.nick, self.files.len(), self.link_speed)
    }
}

pub struct Channel {
    pub name: String,
    pub topic: String,
    /// lower-case nicks in join order
    pub members: Vec<String>,
    pub limit: u32,
    pub level: Level,
    /// predefined channels survive being empty
    pub permanent: bool,
    pub bans: Vec<Ban>,
}

impl Channel {
    pub fn new(name: &str, permanent: bool) -> Self {
        Self {
            name: name.to_owned(),
            topic: format!("Welcome to the {name} channel."),
            members: Vec::new(),
            limit: 200,
            level: Level::Leech,
            permanent,
            bans: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct State {
    /// keyed by lower-case nick
    pub sessions: HashMap<String, Session>,
    /// keyed by lower-case channel name
    pub channels: BTreeMap<String, Channel>,
    pub db: Db,
    /// Linked servers, by link id.
    pub links: HashMap<u64, crate::link::LinkHandle>,
    /// Users on linked servers, keyed by lower-case nick.
    pub remote: HashMap<String, crate::link::RemoteUser>,
    /// Searches waiting on linked servers.
    pub searches: HashMap<u64, crate::link::PendingSearch>,
    /// Recent downloads and searches, for the Discover charts.
    pub trends: crate::discover::Trends,
}

impl State {
    pub fn send(&self, nick: &str, msg: Message) -> bool {
        match self.sessions.get(&nick.to_lowercase()) {
            Some(s) => {
                s.send(msg);
                true
            }
            None => false,
        }
    }

    pub fn session(&self, nick: &str) -> Option<&Session> {
        self.sessions.get(&nick.to_lowercase())
    }

    pub fn session_mut(&mut self, nick: &str) -> Option<&mut Session> {
        self.sessions.get_mut(&nick.to_lowercase())
    }

    /// `<users> <files> <gigabytes>`
    pub fn stats(&self) -> String {
        let files: usize = self.sessions.values().map(|s| s.files.len()).sum::<usize>()
            + self.remote.values().map(|r| r.shared as usize).sum::<usize>();
        let bytes: u64 = self.sessions.values().map(|s| s.shared_bytes()).sum();
        // remote users report counts, not sizes: estimate 4 MB a song
        let remote_bytes: u64 = self.remote.values().map(|r| (r.shared as u64 * 4) << 20).sum();
        format!("{} {} {}", self.sessions.len() + self.remote.len(), files, (bytes + remote_bytes) / (1 << 30))
    }

    /// Send to everyone in `channel` except those ignoring `from`.
    pub fn broadcast_channel(&self, channel: &str, from: Option<&str>, msg: &Message) {
        if let Some(ch) = self.channels.get(&channel.to_lowercase()) {
            let from = from.map(str::to_lowercase);
            for m in &ch.members {
                if let Some(s) = self.sessions.get(m) {
                    if from.as_ref().is_some_and(|f| s.ignored.contains(f)) {
                        continue;
                    }
                    s.send(msg.clone());
                }
            }
        }
    }

    /// Remove `nick` from `channel`, notifying the rest; deletes empty
    /// non-permanent channels. Returns false if they weren't a member.
    pub fn part_channel(&mut self, nick: &str, channel_key: &str) -> bool {
        let key = nick.to_lowercase();
        let Some(ch) = self.channels.get_mut(channel_key) else { return false };
        let Some(pos) = ch.members.iter().position(|m| *m == key) else { return false };
        ch.members.remove(pos);
        let name = ch.name.clone();
        let empty = ch.members.is_empty() && !ch.permanent;
        if let Some(s) = self.sessions.get_mut(&key) {
            s.channels.remove(channel_key);
        }
        if let Some(s) = self.sessions.get(&key) {
            let entry = s.channel_entry(&name);
            s.send(Message::new(napster_proto::msg::PART_CHANNEL, name.clone()));
            self.broadcast_channel(&name, None, &Message::new(napster_proto::msg::CHANNEL_PART, entry));
            self.link_broadcast(&Message::new(crate::link::CHAN_PART, format!("{} {}", name, s.nick)));
        }
        if empty {
            self.channels.remove(channel_key);
        }
        true
    }
}
