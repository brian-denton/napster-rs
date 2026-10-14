//! End to end: a real `napd` server and real client engines on loopback.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use napster_core::{mp3, Client, ClientConfig, Command, ConnState, Event, TransferState};
use napster_proto::{SearchQuery, SearchResult};
use napster_server::{Config, Server};
use tokio::sync::mpsc::UnboundedReceiver;

const WAIT: Duration = Duration::from_secs(15);

async fn start_server() -> (SocketAddr, SocketAddr) {
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        meta_listen: Some("127.0.0.1:0".parse().unwrap()),
        public_host: Some("127.0.0.1".into()),
        stats_interval: Duration::from_secs(3600),
        ..Config::default()
    };
    let server = Server::bind(config).await.unwrap();
    let addrs = (server.local_addr(), server.meta_addr().unwrap());
    server.spawn();
    addrs
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Peer {
    client: Client,
    events: UnboundedReceiver<Event>,
    _dir: tempfile::TempDir,
    shared: PathBuf,
    downloads: PathBuf,
}

impl Peer {
    async fn new(nick: &str, server: SocketAddr, firewalled: bool) -> Peer {
        Self::with(nick, server, firewalled, |_| {}).await
    }

    async fn with(nick: &str, server: SocketAddr, firewalled: bool, tweak: impl FnOnce(&mut ClientConfig)) -> Peer {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared");
        let downloads = dir.path().join("downloads");
        std::fs::create_dir_all(&shared).unwrap();
        let mut config = ClientConfig {
            nick: nick.into(),
            password: "secret".into(),
            server: server.to_string(),
            data_port: if firewalled { 0 } else { free_port() },
            bind_host: "127.0.0.1".into(),
            shared_dirs: vec![shared.clone()],
            download_dir: downloads.clone(),
            cache_library: false,
            authentic_speeds: false,
            ..ClientConfig::default()
        };
        tweak(&mut config);
        let (client, events) = Client::start(config, None);
        Peer { client, events, _dir: dir, shared, downloads }
    }

    fn share(&self, name: &str, data: &[u8]) -> PathBuf {
        let p = self.shared.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    async fn wait<T>(&mut self, mut f: impl FnMut(&Event) -> Option<T>) -> T {
        tokio::time::timeout(WAIT, async {
            loop {
                let e = self.events.recv().await.expect("engine alive");
                if let Event::Error(text) = &e {
                    eprintln!("engine error: {text}");
                }
                if let Some(t) = f(&e) {
                    return t;
                }
            }
        })
        .await
        .expect("timed out waiting for event")
    }

    /// Rescan and wait for a library of `expected` files (skipping any
    /// stale result from the engine's startup scan).
    async fn rescan(&mut self, expected: usize) {
        self.client.send(Command::Rescan);
        self.wait(|e| match e {
            Event::Library(files) if files.len() == expected => Some(()),
            _ => None,
        })
        .await
    }

    async fn connect(&mut self) {
        self.client.send(Command::Connect);
        self.wait(|e| matches!(e, Event::State(ConnState::Online { .. })).then_some(())).await;
    }

    async fn search(&mut self, text: &str) -> Vec<SearchResult> {
        self.client.send(Command::Search(SearchQuery::new(text)));
        let mut results = vec![];
        self.wait(|e| match e {
            Event::SearchResult(r) => {
                results.push(r.clone());
                None
            }
            Event::SearchEnd => Some(()),
            _ => None,
        })
        .await;
        results
    }

    /// Search until the server has indexed at least `n` matches. Shares are
    /// sent right after login, so a search issued immediately can race them.
    async fn search_until(&mut self, text: &str, n: usize) -> Vec<SearchResult> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let results = self.search(text).await;
            if results.len() >= n || tokio::time::Instant::now() > deadline {
                return results;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn download(&mut self, r: &SearchResult) -> PathBuf {
        self.client.send(Command::Download { nick: r.nick.clone(), file: r.file.clone() });
        self.wait(|e| match e {
            Event::Transfer(t) if t.state == TransferState::Complete => t.saved_to.clone(),
            Event::Transfer(t) => match &t.state {
                TransferState::Failed(why) => panic!("download failed: {why}"),
                _ => None,
            },
            _ => None,
        })
        .await
    }
}

fn song(frames: usize) -> Vec<u8> {
    let mut data = b"ID3\x03\x00\x00\x00\x00\x00\x00".to_vec();
    data.extend(mp3::synth_mp3(frames, 9));
    data
}

fn assert_same(a: &Path, b: &[u8]) {
    let got = std::fs::read(a).unwrap();
    assert_eq!(got.len(), b.len(), "size mismatch for {}", a.display());
    assert!(got == b, "content mismatch for {}", a.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn search_and_download() {
    let (server, _) = start_server().await;
    let mut alice = Peer::new("alice", server, false).await;
    let mut bob = Peer::new("bob", server, false).await;
    let data = song(3000); // ~1.2 MB
    alice.share("Generic Band - Generic Song.mp3", &data);
    alice.rescan(1).await;
    alice.connect().await;
    bob.connect().await;

    let results = bob.search_until("generic song", 1).await;
    assert_eq!(results.len(), 1, "{results:?}");
    let r = &results[0];
    assert_eq!(r.nick, "alice");
    assert_eq!(r.file.bitrate, 128);
    assert_eq!(r.file.frequency, 44100);
    assert!(bob.search("nothing matches this").await.is_empty());

    let saved = bob.download(r).await;
    assert_eq!(saved, bob.downloads.join("Generic Band - Generic Song.mp3"));
    assert_same(&saved, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn firewalled_uploader_pushes() {
    let (server, _) = start_server().await;
    let mut alice = Peer::new("alice", server, true).await; // data port 0
    let mut bob = Peer::new("bob", server, false).await;
    let data = song(500);
    alice.share("pushed.mp3", &data);
    alice.rescan(1).await;
    alice.connect().await;
    bob.connect().await;
    let results = bob.search_until("pushed", 1).await;
    assert_eq!(results.len(), 1);
    let saved = bob.download(&results[0]).await;
    assert_same(&saved, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_partial_download() {
    let (server, _) = start_server().await;
    let mut alice = Peer::new("alice", server, false).await;
    let mut bob = Peer::new("bob", server, false).await;
    let data = song(2000);
    alice.share("resume me.mp3", &data);
    alice.rescan(1).await;
    alice.connect().await;
    bob.connect().await;
    let incomplete = bob.downloads.join("Incomplete");
    std::fs::create_dir_all(&incomplete).unwrap();
    std::fs::write(incomplete.join("resume me.mp3"), &data[..300_000]).unwrap();
    let r = bob.search_until("resume", 1).await.remove(0);
    let saved = bob.download(&r).await;
    assert_same(&saved, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_browse_and_firewalled_browse() {
    let (server, _) = start_server().await;
    for firewalled in [false, true] {
        let mut alice = Peer::new(if firewalled { "carol" } else { "alice" }, server, firewalled).await;
        let mut bob = Peer::new(if firewalled { "dave" } else { "bob" }, server, false).await;
        alice.share("one.mp3", &song(100));
        alice.share("two.mp3", &song(120));
        alice.rescan(2).await;
        alice.connect().await;
        bob.connect().await;
        let target = if firewalled { "carol" } else { "alice" };
        assert_eq!(bob.search_until("mp3", 2).await.len(), 2);
        bob.client.send(Command::Browse(target.into()));
        let mut files = vec![];
        bob.wait(|e| match e {
            Event::BrowseResult(r) => {
                files.push(r.file.basename().to_owned());
                None
            }
            Event::BrowseEnd { .. } => Some(()),
            _ => None,
        })
        .await;
        files.sort();
        assert_eq!(files, vec!["one.mp3", "two.mp3"], "firewalled={firewalled}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_and_private_messages() {
    let (server, _) = start_server().await;
    let mut alice = Peer::new("alice", server, false).await;
    let mut bob = Peer::new("bob", server, false).await;
    alice.connect().await;
    bob.connect().await;

    alice.client.send(Command::Join("Lobby".into()));
    alice.wait(|e| matches!(e, Event::Joined { .. }).then_some(())).await;
    bob.client.send(Command::Join("lobby".into()));
    let users = bob
        .wait(|e| match e {
            Event::ChannelUsers { users, .. } => Some(users.iter().map(|u| u.nick.clone()).collect::<Vec<_>>()),
            _ => None,
        })
        .await;
    assert_eq!(users, vec!["alice", "bob"]);
    alice
        .wait(|e| matches!(e, Event::UserJoined { user, .. } if user.nick == "bob").then_some(()))
        .await;

    alice.client.send(Command::Say { channel: "Lobby".into(), text: "hello  world".into() });
    let (nick, text) = bob
        .wait(|e| match e {
            Event::ChannelMessage { nick, text, .. } => Some((nick.clone(), text.clone())),
            _ => None,
        })
        .await;
    assert_eq!((nick.as_str(), text.as_str()), ("alice", "hello  world"));

    bob.client.send(Command::PrivateMessage { to: "alice".into(), text: "psst".into() });
    let (from, text) = alice
        .wait(|e| match e {
            Event::PrivateMessage { from, text } => Some((from.clone(), text.clone())),
            _ => None,
        })
        .await;
    assert_eq!((from.as_str(), text.as_str()), ("bob", "psst"));

    // hot list: bob watches alice, alice leaves
    bob.client.send(Command::HotlistAdd("alice".into()));
    bob.wait(|e| matches!(e, Event::HotlistOnline { nick, .. } if nick == "alice").then_some(())).await;
    alice.client.send(Command::Disconnect);
    bob.wait(|e| matches!(e, Event::HotlistOffline { nick } if nick == "alice").then_some(())).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn login_via_metaserver_and_bad_password() {
    let (server, meta) = start_server().await;
    let mut alice = Peer::new("alice", server, false).await;
    alice.connect().await;
    alice.client.send(Command::Disconnect);
    alice.wait(|e| matches!(e, Event::State(ConnState::Disconnected)).then_some(())).await;

    // same nick, wrong password, through the metaserver
    let dir = tempfile::tempdir().unwrap();
    let config = ClientConfig {
        nick: "alice".into(),
        password: "wrong".into(),
        server: meta.to_string(),
        use_metaserver: true,
        data_port: 0,
        shared_dirs: vec![],
        download_dir: dir.path().to_path_buf(),
        cache_library: false,
        authentic_speeds: false,
        ..ClientConfig::default()
    };
    let (client, mut events) = Client::start(config.clone(), None);
    client.send(Command::Connect);
    let err = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(Event::Error(e)) = events.recv().await {
                return e;
            }
        }
    })
    .await
    .unwrap();
    assert!(err.contains("invalid password"), "{err}");

    // right password via metaserver
    let (client, mut events) = Client::start(ClientConfig { password: "secret".into(), ..config }, None);
    client.send(Command::Connect);
    tokio::time::timeout(WAIT, async {
        loop {
            match events.recv().await {
                Some(Event::State(ConnState::Online { .. })) => return,
                Some(Event::Error(e)) => panic!("{e}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn download_limit_is_enforced() {
    let (server, _) = start_server().await;
    let mut alice = Peer::new("alice", server, false).await;
    // 100 KB/s cap on bob's downloads
    let mut bob = Peer::with("bob", server, false, |c| c.download_limit_kbps = 100).await;
    let data = song(600); // ~250 KB
    alice.share("capped.mp3", &data);
    alice.rescan(1).await;
    alice.connect().await;
    bob.connect().await;
    let r = bob.search_until("capped", 1).await.remove(0);
    let start = std::time::Instant::now();
    let saved = bob.download(&r).await;
    let secs = start.elapsed().as_secs_f64();
    assert_same(&saved, &data);
    // 250 KB at 100 KB/s is ~2.5 s (less a quarter second of burst)
    assert!(secs > 1.8 && secs < 10.0, "took {secs:.2}s");
}

#[tokio::test(flavor = "multi_thread")]
async fn authentic_speeds_follow_line_speed() {
    let (server, _) = start_server().await;
    // a 33.6K modem uploads at ~3.6 KB/s
    let mut alice = Peer::with("alice", server, false, |c| {
        c.authentic_speeds = true;
        c.link_speed = 3;
    })
    .await;
    let mut bob = Peer::new("bob", server, false).await;
    let data = song(20); // ~8 KB
    alice.share("modem.mp3", &data);
    alice.rescan(1).await;
    alice.connect().await;
    bob.connect().await;
    let r = bob.search_until("modem", 1).await.remove(0);
    assert_eq!(r.link_speed, 3);
    let start = std::time::Instant::now();
    let saved = bob.download(&r).await;
    let secs = start.elapsed().as_secs_f64();
    assert_same(&saved, &data);
    // 8.3 KB at 3.6 KB/s is ~2.3 s
    assert!(secs > 1.6 && secs < 10.0, "took {secs:.2}s");
}
