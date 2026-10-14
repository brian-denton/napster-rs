//! Interactive command-line Napster client.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use clap::Parser;
use napster_core::{Client, ClientConfig, Command, ConnState, Direction, Event, TransferInfo, TransferState};
use napster_proto::{link_speed_name, SearchQuery, SearchResult};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(name = "napster-cli", version, about = "Command line Napster client")]
struct Args {
    /// Settings file (shared with the GUI by default).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Server or metaserver `host:port`.
    #[arg(long)]
    server: Option<String>,
    /// Treat --server as a metaserver (redirector).
    #[arg(long)]
    meta: bool,
    /// Encrypt the server connection (napd's TLS port, 8889).
    #[arg(long)]
    tls: bool,
    /// Connect through a SOCKS5 proxy, `host:port`.
    #[arg(long, value_name = "HOST:PORT")]
    proxy: Option<String>,
    #[arg(long, requires = "proxy")]
    proxy_user: Option<String>,
    #[arg(long, requires = "proxy")]
    proxy_password: Option<String>,
    /// Don't send file transfers through the proxy (server connection only).
    #[arg(long, requires = "proxy")]
    proxy_server_only: bool,
    #[arg(long)]
    nick: Option<String>,
    #[arg(long)]
    password: Option<String>,
    /// Folder to share (repeatable).
    #[arg(long)]
    share: Vec<PathBuf>,
    #[arg(long)]
    download_dir: Option<PathBuf>,
    /// Data port for transfers; 0 = firewalled.
    #[arg(long)]
    data_port: Option<u16>,
    /// Total upload cap in KB/s (0 = unlimited). Implies --modern-speeds.
    #[arg(long)]
    upload_limit: Option<u32>,
    /// Total download cap in KB/s (0 = unlimited). Implies --modern-speeds.
    #[arg(long)]
    download_limit: Option<u32>,
    /// Your connection type, 0-10 (4 = 56K, 8 = DSL); see `napster-cli --help`.
    #[arg(long)]
    line_speed: Option<u32>,
    /// Don't throttle transfers to the line speed's period-accurate rate.
    #[arg(long)]
    modern_speeds: bool,
    /// Interface for the data port listener (default 0.0.0.0).
    #[arg(long)]
    bind_host: Option<String>,
    /// Install a signed update package from disk (the .sig must be next
    /// to it) and exit.
    #[arg(long, value_name = "PACKAGE")]
    apply_update: Option<PathBuf>,
    /// Save the resulting settings back to the config file.
    #[arg(long)]
    save: bool,
}

const HELP: &str = "\
commands:
  /connect  /disconnect  /quit
  /search <words>          search; results are numbered
  /get <n> [n...]          download result(s) from the last search/browse
  /browse <user>           list a user's files (numbered like search)
  /transfers  /cancel <id>  /clear
  /files                   list your shared files   /rescan
  /join <room>  /part [room]  /rooms  /me <action>  /topic <text>
  /msg <user> <text>  /whois <user>  /ping <user>
  /hotlist [add|rm <user>]
  /genres  /genre <name>   browse music by genre (files numbered like search)
  /charts                  live charts: most requested, hot searches, top artists
  /members                 members whose collections are like yours
  /raw <numeric> <data>
  /update                  check for a new version and install it
anything else is said in the current room.";

struct Ui {
    client: Client,
    results: Vec<SearchResult>,
    room: Option<String>,
    transfers: BTreeMap<u64, TransferInfo>,
    files: Vec<napster_core::library::SharedFile>,
    /// /update was typed: install whatever the check finds
    install_when_found: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    if let Some(pkg) = &args.apply_update {
        let installed = napster_core::update::install_local(pkg)?;
        println!("installed {} -> {}", pkg.display(), installed.display());
        return Ok(());
    }
    let path = args.config.clone().unwrap_or_else(ClientConfig::default_path);
    let mut config = ClientConfig::load(&path)?;
    if let Some(s) = args.server {
        config.server = s;
        config.use_metaserver = args.meta;
    }
    if args.tls {
        config.tls = true;
    }
    if let Some(p) = &args.proxy {
        let (host, port) = p.rsplit_once(':').ok_or_else(|| anyhow::anyhow!("--proxy needs host:port"))?;
        config.proxy = napster_core::proxy::ProxyConfig {
            enabled: true,
            host: host.to_owned(),
            port: port.parse()?,
            username: args.proxy_user.clone().unwrap_or_default(),
            password: args.proxy_password.clone().unwrap_or_default(),
            transfers: !args.proxy_server_only,
        };
    }
    if let Some(n) = args.nick {
        config.nick = n;
    }
    if let Some(p) = args.password {
        config.password = p;
    }
    if !args.share.is_empty() {
        config.shared_dirs = args.share;
    }
    if let Some(d) = args.download_dir {
        config.download_dir = d;
    }
    if let Some(p) = args.data_port {
        config.data_port = p;
    }
    if let Some(l) = args.line_speed {
        config.link_speed = l.min(10);
    }
    if args.modern_speeds || args.upload_limit.is_some() || args.download_limit.is_some() {
        config.authentic_speeds = false;
    }
    if let Some(l) = args.upload_limit {
        config.upload_limit_kbps = l;
    }
    if let Some(l) = args.download_limit {
        config.download_limit_kbps = l;
    }
    if let Some(h) = args.bind_host {
        config.bind_host = h;
    }
    if args.save {
        config.save(&path)?;
    }
    println!("{} — type /help", napster_core::CLIENT_INFO);
    let (down, up) = config.rate_limits();
    let fmt = |b: u64| if b == 0 { "unlimited".to_owned() } else { format!("{:.1} KB/s", b as f64 / 1024.0) };
    println!(
        "line speed {}: downloads {}, uploads {}{}",
        link_speed_name(config.link_speed),
        fmt(down),
        fmt(up),
        if config.authentic_speeds { " (authentic speeds)" } else { "" }
    );
    println!(
        "server {}{}, nick {:?}, data port {}, sharing {:?}",
        config.server,
        if config.use_metaserver { " (metaserver)" } else { "" },
        config.nick,
        config.data_port,
        config.shared_dirs
    );

    let (client, mut events) = Client::start(config, None);
    client.send(Command::Connect);
    let mut ui = Ui {
        client,
        results: vec![],
        room: None,
        transfers: BTreeMap::new(),
        files: vec![],
        install_when_found: false,
    };
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(l) => if !ui.input(l.trim()) { break },
                None => break,
            },
            e = events.recv() => match e {
                Some(e) => ui.event(e),
                None => break,
            },
        }
        let _ = std::io::stdout().flush();
    }
    ui.client.send(Command::Disconnect);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    Ok(())
}

impl Ui {
    /// Returns false to quit.
    fn input(&mut self, line: &str) -> bool {
        if line.is_empty() {
            return true;
        }
        let Some(cmd) = line.strip_prefix('/') else {
            match &self.room {
                Some(r) => self.client.send(Command::Say { channel: r.clone(), text: line.to_owned() }),
                None => println!("not in a room — /join <room> first, or /help"),
            }
            return true;
        };
        let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
        let rest = rest.trim();
        let arg = rest.split_whitespace().next().unwrap_or("").to_owned();
        let c = &self.client;
        match verb {
            "quit" | "exit" | "q" => return false,
            "help" | "?" => println!("{HELP}"),
            "connect" => c.send(Command::Connect),
            "disconnect" => c.send(Command::Disconnect),
            "search" | "s" => {
                self.results.clear();
                c.send(Command::Search(SearchQuery::new(rest)));
            }
            "browse" => {
                self.results.clear();
                c.send(Command::Browse(arg));
            }
            "genres" => c.send(Command::DiscoverGenres),
            "genre" => {
                self.results.clear();
                c.send(Command::DiscoverGenre(rest.to_owned()));
            }
            "charts" => c.send(Command::DiscoverCharts),
            "members" => c.send(Command::DiscoverMembers),
            "get" | "g" => {
                for n in rest.split_whitespace().filter_map(|n| n.parse::<usize>().ok()) {
                    match self.results.get(n.wrapping_sub(1)) {
                        Some(r) => {
                            c.send(Command::Download { nick: r.nick.clone(), file: r.file.clone() });
                            println!("queued {} from {}", r.file.basename(), r.nick);
                        }
                        None => println!("no result #{n}"),
                    }
                }
            }
            "transfers" | "t" => {
                if self.transfers.is_empty() {
                    println!("no transfers");
                }
                for t in self.transfers.values() {
                    println!(
                        "  #{:<3} {:<4} {:<40} {:<12} {:>3}%  {}",
                        t.id,
                        if t.direction == Direction::Download { "down" } else { "up" },
                        t.basename(),
                        t.nick,
                        (t.progress() * 100.0) as u32,
                        t.state.label()
                    );
                }
            }
            "cancel" => match arg.parse() {
                Ok(id) => c.send(Command::CancelTransfer(id)),
                Err(_) => println!("usage: /cancel <id>"),
            },
            "clear" => c.send(Command::ClearFinished),
            "rescan" => c.send(Command::Rescan),
            "files" => {
                for f in &self.files {
                    println!("  {}  [{} kbps, {}]", f.info.filename, f.info.bitrate, size(f.info.size));
                }
                println!("--- {} files shared", self.files.len());
            }
            "join" | "j" => c.send(Command::Join(arg)),
            "part" => {
                let room = if arg.is_empty() { self.room.clone().unwrap_or_default() } else { arg };
                c.send(Command::Part(room));
            }
            "rooms" | "list" => c.send(Command::ListChannels),
            "me" => {
                if let Some(r) = &self.room {
                    c.send(Command::Emote { channel: r.clone(), text: rest.to_owned() });
                }
            }
            "topic" => {
                if let Some(r) = &self.room {
                    c.send(Command::SetTopic { channel: r.clone(), topic: rest.to_owned() });
                }
            }
            "msg" | "m" => {
                let text = rest[arg.len()..].trim().to_owned();
                c.send(Command::PrivateMessage { to: arg.clone(), text: text.clone() });
                println!("-> *{arg}* {text}");
            }
            "whois" | "w" => c.send(Command::Whois(arg)),
            "ping" => c.send(Command::Ping(arg)),
            "hotlist" => {
                let mut it = rest.split_whitespace();
                match (it.next(), it.next()) {
                    (Some("add"), Some(n)) => c.send(Command::HotlistAdd(n.into())),
                    (Some("rm"), Some(n)) => c.send(Command::HotlistRemove(n.into())),
                    _ => println!("usage: /hotlist add|rm <user>"),
                }
            }
            "update" => {
                self.install_when_found = true;
                c.send(Command::CheckForUpdate { manual: true });
            }
            "raw" => {
                let mut parts = rest.splitn(2, ' ');
                match parts.next().and_then(|n| n.parse::<u16>().ok()) {
                    Some(k) => c.send(Command::Raw(k, parts.next().unwrap_or("").to_owned())),
                    None => println!("usage: /raw <numeric> <data>"),
                }
            }
            _ => println!("unknown command /{verb} — /help"),
        }
        true
    }

    fn event(&mut self, e: Event) {
        match e {
            Event::State(ConnState::Online { nick }) => println!("*** online as {nick}"),
            Event::State(ConnState::Disconnected) => println!("*** offline"),
            Event::State(ConnState::Connecting) => println!("*** connecting..."),
            Event::Status(s) | Event::Info(s) => println!("*** {s}"),
            Event::Error(s) => println!("!!! {s}"),
            Event::Motd(s) => println!("motd: {s}"),
            Event::Stats { users, files, gigabytes } => {
                println!("*** {users} users sharing {files} files ({gigabytes} GB)")
            }
            Event::LibraryScanning { .. } => {}
            Event::Library(files) => {
                println!("*** sharing {} files", files.len());
                self.files = files;
            }
            Event::SearchResult(r) | Event::BrowseResult(r) => {
                self.results.push(r.clone());
                println!(
                    "{:>3}. {}  [{} kbps, {}, {}] {} ({})",
                    self.results.len(),
                    r.file.basename(),
                    r.file.bitrate,
                    size(r.file.size),
                    secs(r.file.seconds),
                    r.nick,
                    link_speed_name(r.link_speed)
                );
            }
            Event::Genres(g) => {
                if g.is_empty() {
                    println!("*** no tagged music is being shared yet");
                }
                for g in g {
                    println!("  {:<24} {:>6} files  {:>4} users", g.name, g.files, g.users);
                }
            }
            Event::GenreFiles { genre, files } => {
                for r in files {
                    self.results.push(r.clone());
                    println!(
                        "{:>3}. {}  [{} kbps, {}] {} ({})",
                        self.results.len(),
                        r.file.basename(),
                        r.file.bitrate,
                        secs(r.file.seconds),
                        r.nick,
                        link_speed_name(r.link_speed)
                    );
                }
                println!("--- {} {genre} files (/get <n>)", self.results.len());
            }
            Event::Charts(c) => {
                for (title, rows) in
                    [("most requested", &c.songs), ("hot searches", &c.searches), ("most shared artists", &c.artists)]
                {
                    println!("--- {title} (24 hrs)");
                    for (i, e) in rows.iter().enumerate() {
                        println!("  {:>2}. {}  ({})", i + 1, e.text, e.count);
                    }
                }
            }
            Event::Members(m) => {
                if m.is_empty() {
                    println!("*** no members to suggest yet");
                }
                for m in m {
                    println!(
                        "  {:<16} {:>3}% match  {:>5} files  in common: {}  favorites: {}",
                        m.nick,
                        m.matching,
                        m.files,
                        m.common.join(", "),
                        m.favorites.join(", ")
                    );
                }
            }
            Event::DiscoverUnavailable => println!("!!! this server doesn't offer Discover"),
            Event::SearchEnd => println!("--- {} results (/get <n>)", self.results.len()),
            Event::BrowseEnd { nick } => println!("--- {} files from {nick} (/get <n>)", self.results.len()),
            Event::Transfer(t) => {
                let changed = self.transfers.get(&t.id).is_none_or(|old| old.state != t.state);
                transfer_line(&t, changed);
                self.transfers.insert(t.id, t);
            }
            Event::TransferRemoved(id) => {
                self.transfers.remove(&id);
            }
            Event::PrivateMessage { from, text } => println!("*{from}* {text}"),
            Event::Joined { channel } => {
                println!("*** joined {channel}");
                self.room = Some(channel);
            }
            Event::Parted { channel } => {
                println!("*** left {channel}");
                if self.room.as_deref().is_some_and(|r| r.eq_ignore_ascii_case(&channel)) {
                    self.room = None;
                }
            }
            Event::ChannelUsers { channel, users } => {
                let names: Vec<_> = users.iter().map(|u| u.nick.as_str()).collect();
                println!("[{channel}] users: {}", names.join(" "));
            }
            Event::UserJoined { channel, user } => println!("[{channel}] {} has joined", user.nick),
            Event::UserParted { channel, nick } => println!("[{channel}] {nick} has left"),
            Event::ChannelMessage { channel, nick, text } => println!("[{channel}] <{nick}> {text}"),
            Event::ChannelEmote { channel, nick, text } => println!("[{channel}] * {nick} {text}"),
            Event::Topic { channel, topic } => println!("[{channel}] topic: {topic}"),
            Event::ChannelList(list) => {
                for (name, users, topic) in list {
                    println!("  {name:<20} {users:>4}  {topic}");
                }
            }
            Event::HotlistOnline { nick, .. } => println!("*** {nick} is online"),
            Event::HotlistOffline { nick } => println!("*** {nick} is offline"),
            Event::Whois(fields) => {
                for (k, v) in fields {
                    println!("  {k:<18} {v}");
                }
            }
            Event::Pong { nick } => println!("*** pong from {nick}"),
            Event::ServerPong { millis } => println!("*** server pong: {millis} ms"),
            Event::Announcement { from, text } => println!("*** [announcement] {from}: {text}"),
            Event::ChannelInvite { from, channel } => println!("*** {from} invites you to {channel}"),
            Event::UpdateAvailable(info) => {
                if std::mem::take(&mut self.install_when_found) {
                    println!("*** installing napster-rs {} ...", info.version);
                    self.client.send(Command::InstallUpdate(info));
                } else {
                    println!(
                        "*** napster-rs {} is available (you have {}). Type /update to install it.",
                        info.version,
                        napster_core::update::current_version()
                    );
                }
            }
            Event::UpToDate => {
                self.install_when_found = false;
                println!("*** you have the latest version ({})", napster_core::update::current_version());
            }
            Event::UpdateProgress(msg) => println!("*** {msg}"),
            Event::UpdateInstalled { version, .. } => {
                println!("*** updated to {version}; restart napster-cli to use it")
            }
            Event::UpdateFailed(e) => {
                self.install_when_found = false;
                println!("!!! update failed: {e}");
            }
        }
    }
}

/// Print state changes, plus progress at most once per quarter.
fn transfer_line(t: &TransferInfo, changed: bool) {
    let dir = match t.direction {
        Direction::Download => "down",
        Direction::Upload => "up",
    };
    match &t.state {
        TransferState::Transferring => {
            let pct = (t.progress() * 100.0) as u32;
            if changed || (pct.is_multiple_of(25) && pct > 0) {
                println!("[#{} {dir}] {} {}% ({}/s)", t.id, t.basename(), pct, size(t.rate() as u64));
            }
        }
        TransferState::Complete => match &t.saved_to {
            Some(p) => println!("[#{} {dir}] {} complete -> {}", t.id, t.basename(), p.display()),
            None => println!("[#{} {dir}] {} complete", t.id, t.basename()),
        },
        s => println!("[#{} {dir}] {} {} ({})", t.id, t.basename(), s.label(), t.nick),
    }
}

fn size(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    } else {
        format!("{:.0} KB", n as f64 / 1024.0)
    }
}

fn secs(s: u32) -> String {
    format!("{}:{:02}", s / 60, s % 60)
}
