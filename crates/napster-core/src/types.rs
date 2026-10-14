//! Commands into and events out of the client engine.

use std::time::Instant;

use napster_proto::{FileInfo, SearchQuery, SearchResult};

use crate::config::ClientConfig;
use crate::update::UpdateInfo;

#[derive(Debug, Clone)]
pub enum Command {
    Connect,
    Disconnect,
    /// Replace settings (takes effect for the data port on next connect).
    SetConfig(Box<ClientConfig>),
    /// Rescan shared folders and re-share.
    Rescan,
    Search(SearchQuery),
    /// Queue a download of `file` from `nick`.
    Download {
        nick: String,
        file: FileInfo,
    },
    CancelTransfer(u64),
    /// Forget finished/failed transfers.
    ClearFinished,
    /// Browse a user: direct (640) first, server list (211) as fallback.
    Browse(String),
    Whois(String),
    Ping(String),
    PingServer,
    PrivateMessage {
        to: String,
        text: String,
    },
    Join(String),
    Part(String),
    Say {
        channel: String,
        text: String,
    },
    Emote {
        channel: String,
        text: String,
    },
    SetTopic {
        channel: String,
        topic: String,
    },
    ListChannels,
    HotlistAdd(String),
    HotlistRemove(String),
    Ignore(String),
    Unignore(String),
    /// Send an arbitrary message (for /raw and admin commands).
    Raw(u16, String),
    /// Discover: genres on the server.
    DiscoverGenres,
    /// Discover: files in one genre.
    DiscoverGenre(String),
    /// Discover: live charts.
    DiscoverCharts,
    /// Discover: members whose collections resemble ours.
    DiscoverMembers,
    /// Look for a newer release. `manual` also reports "up to date" and errors.
    CheckForUpdate {
        manual: bool,
    },
    /// Download, verify and install a release found by `CheckForUpdate`.
    InstallUpdate(UpdateInfo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Online { nick: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    /// Waiting for a free download slot.
    Queued,
    /// 203 sent, waiting for the uploader to accept (204).
    Requesting,
    Connecting,
    /// 500 sent, waiting for the firewalled uploader to connect to us.
    WaitingForPush,
    Transferring,
    Complete,
    Failed(String),
    Cancelled,
}

impl TransferState {
    pub fn is_finished(&self) -> bool {
        matches!(self, TransferState::Complete | TransferState::Failed(_) | TransferState::Cancelled)
    }

    pub fn label(&self) -> String {
        match self {
            TransferState::Queued => "Queued".into(),
            TransferState::Requesting => "Requesting".into(),
            TransferState::Connecting => "Connecting".into(),
            TransferState::WaitingForPush => "Waiting through firewall".into(),
            TransferState::Transferring => "Transferring".into(),
            TransferState::Complete => "Complete".into(),
            TransferState::Failed(why) => format!("Failed: {why}"),
            TransferState::Cancelled => "Cancelled".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransferInfo {
    pub id: u64,
    pub direction: Direction,
    pub nick: String,
    /// Remote path for downloads, our advertised path for uploads.
    pub filename: String,
    pub size: u64,
    /// Byte offset the transfer resumed from.
    pub offset: u64,
    /// Bytes moved this session (excluding `offset`).
    pub transferred: u64,
    pub state: TransferState,
    pub started: Option<Instant>,
    /// For completed downloads: where the file ended up.
    pub saved_to: Option<std::path::PathBuf>,
    pub md5: String,
}

impl TransferInfo {
    pub fn progress(&self) -> f32 {
        if self.size == 0 {
            return 0.0;
        }
        ((self.offset + self.transferred) as f64 / self.size as f64).min(1.0) as f32
    }

    /// Bytes per second since the data started flowing.
    pub fn rate(&self) -> f64 {
        match self.started {
            Some(t) if !self.state.is_finished() || self.transferred > 0 => {
                let secs = t.elapsed().as_secs_f64().max(0.001);
                self.transferred as f64 / secs
            }
            _ => 0.0,
        }
    }

    pub fn basename(&self) -> &str {
        napster_proto::share::basename(&self.filename)
    }
}

#[derive(Debug, Clone)]
pub struct ChannelUser {
    pub nick: String,
    pub shared: u32,
    pub link_speed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreInfo {
    pub name: String,
    pub files: u32,
    pub users: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartEntry {
    pub text: String,
    pub count: u32,
}

/// Live charts over the last 24 hours.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Charts {
    /// Most requested songs ("Artist - Title").
    pub songs: Vec<ChartEntry>,
    /// Most frequent searches.
    pub searches: Vec<ChartEntry>,
    /// Artists shared by the most users right now.
    pub artists: Vec<ChartEntry>,
}

/// A user whose collection resembles yours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub nick: String,
    /// 0-100: how much your artists overlap.
    pub matching: u32,
    pub files: u32,
    pub link_speed: u32,
    pub common: Vec<String>,
    pub favorites: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Event {
    State(ConnState),
    /// Status-bar style messages ("Connecting to Server...").
    Status(String),
    /// Something to show in the console (server errors, notices).
    Info(String),
    Error(String),
    Motd(String),
    Stats {
        users: u64,
        files: u64,
        gigabytes: u64,
    },
    LibraryScanning {
        done: usize,
        total: usize,
    },
    /// Our shared files; `info.filename` is what others see, `path` is local.
    Library(Vec<crate::library::SharedFile>),
    SearchResult(SearchResult),
    SearchEnd,
    BrowseResult(SearchResult),
    BrowseEnd {
        nick: String,
    },
    Transfer(TransferInfo),
    TransferRemoved(u64),
    PrivateMessage {
        from: String,
        text: String,
    },
    Joined {
        channel: String,
    },
    Parted {
        channel: String,
    },
    ChannelUsers {
        channel: String,
        users: Vec<ChannelUser>,
    },
    UserJoined {
        channel: String,
        user: ChannelUser,
    },
    UserParted {
        channel: String,
        nick: String,
    },
    ChannelMessage {
        channel: String,
        nick: String,
        text: String,
    },
    ChannelEmote {
        channel: String,
        nick: String,
        text: String,
    },
    Topic {
        channel: String,
        topic: String,
    },
    ChannelList(Vec<(String, u32, String)>),
    HotlistOnline {
        nick: String,
        link_speed: u32,
    },
    HotlistOffline {
        nick: String,
    },
    Whois(Vec<(String, String)>),
    Pong {
        nick: String,
    },
    ServerPong {
        millis: u64,
    },
    Announcement {
        from: String,
        text: String,
    },
    ChannelInvite {
        from: String,
        channel: String,
    },
    /// Discover answers.
    Genres(Vec<GenreInfo>),
    GenreFiles {
        genre: String,
        files: Vec<SearchResult>,
    },
    Charts(Charts),
    Members(Vec<Member>),
    /// The server doesn't support Discover (not napd).
    DiscoverUnavailable,
    UpdateAvailable(UpdateInfo),
    /// Reply to a manual check when there's nothing newer.
    UpToDate,
    UpdateProgress(String),
    /// Installed; launching `relaunch` starts the new version.
    UpdateInstalled {
        version: String,
        relaunch: std::path::PathBuf,
    },
    UpdateFailed(String),
}
