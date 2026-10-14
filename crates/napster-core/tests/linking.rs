//! Two linked napd servers act as one network: users on different servers
//! find and download each other's files, chat, message, and see each
//! other online. A server with the wrong link password is kept out.

use std::net::SocketAddr;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use napster_core::{mp3, Client, ClientConfig, Command, ConnState, Event, TransferState};
use napster_proto::{msg, Message, NapCodec, SearchQuery, SearchResult};
use napster_server::link::LinkPeer;
use napster_server::{Config, Server};
use tokio::sync::mpsc::UnboundedReceiver;

const WAIT: Duration = Duration::from_secs(20);

fn base_config(name: &str) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        meta_listen: None,
        stats_interval: Duration::from_secs(3600),
        server_name: name.into(),
        link_password: Some("linksecret".into()),
        ..Config::default()
    }
}

/// alpha accepts links; beta links to it. Returns their client addresses.
async fn network() -> (SocketAddr, SocketAddr, SocketAddr, String) {
    let alpha = Server::bind(Config { link_listen: Some("127.0.0.1:0".parse().unwrap()), ..base_config("alpha") })
        .await
        .unwrap();
    let (a_addr, a_link, a_fp) =
        (alpha.local_addr(), alpha.link_addr().unwrap(), alpha.tls_fingerprint().unwrap().to_owned());
    alpha.spawn();
    let peer = LinkPeer { host: "127.0.0.1".into(), port: a_link.port(), fingerprint: a_fp.clone() };
    let beta = Server::bind(Config { links: vec![peer], ..base_config("beta") }).await.unwrap();
    let b_addr = beta.local_addr();
    beta.spawn();
    (a_addr, b_addr, a_link, a_fp)
}

struct User {
    client: Client,
    events: UnboundedReceiver<Event>,
    _dir: tempfile::TempDir,
}

impl User {
    async fn new(nick: &str, server: SocketAddr, songs: &[(&str, Vec<u8>)]) -> User {
        let dir = tempfile::tempdir().unwrap();
        let share = dir.path().join("share");
        std::fs::create_dir_all(&share).unwrap();
        for (name, data) in songs {
            std::fs::write(share.join(name), data).unwrap();
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (client, events) = Client::start(
            ClientConfig {
                nick: nick.into(),
                password: "pw".into(),
                server: server.to_string(),
                data_port: port,
                bind_host: "127.0.0.1".into(),
                shared_dirs: vec![share],
                download_dir: dir.path().join("dl"),
                cache_library: false,
                check_for_updates: false,
                authentic_speeds: false,
                ..ClientConfig::default()
            },
            None,
        );
        let mut u = User { client, events, _dir: dir };
        let n = songs.len();
        u.wait(|e| matches!(e, Event::Library(f) if f.len() == n).then_some(())).await;
        u.client.send(Command::Connect);
        u.wait(|e| matches!(e, Event::State(ConnState::Online { .. })).then_some(())).await;
        u
    }

    async fn wait<T>(&mut self, mut f: impl FnMut(&Event) -> Option<T>) -> T {
        tokio::time::timeout(WAIT, async {
            loop {
                let e = self.events.recv().await.expect("engine alive");
                if let Some(t) = f(&e) {
                    return t;
                }
            }
        })
        .await
        .expect("timed out waiting for event")
    }

    async fn search(&mut self, text: &str) -> Vec<SearchResult> {
        self.client.send(Command::Search(SearchQuery::new(text)));
        let mut out = vec![];
        self.wait(|e| match e {
            Event::SearchResult(r) => {
                out.push(r.clone());
                None
            }
            Event::SearchEnd => Some(()),
            _ => None,
        })
        .await;
        out
    }

    /// Search until the network has linked and indexed the files.
    async fn search_until(&mut self, text: &str, n: usize) -> Vec<SearchResult> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let r = self.search(text).await;
            if r.len() >= n || tokio::time::Instant::now() > deadline {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn linked_servers_share_users_search_downloads_and_chat() {
    let (alpha, beta, _, _) = network().await;
    let song = mp3::synth_mp3(400, 9);
    let mut alice = User::new("alice", alpha, &[("Linked Up - Across Servers.mp3", song.clone())]).await;
    let mut bob = User::new("bob", beta, &[("Beta Band - Other Side.mp3", mp3::synth_mp3(100, 9))]).await;

    // search reaches across the link, both ways
    let found = bob.search_until("across servers", 1).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].nick, "alice");
    assert_eq!(alice.search_until("other side", 1).await[0].nick, "bob");

    // download from a user on the other server
    bob.client.send(Command::Download { nick: "alice".into(), file: found[0].file.clone() });
    let saved = bob
        .wait(|e| match e {
            Event::Transfer(t) if t.state == TransferState::Complete => t.saved_to.clone(),
            Event::Transfer(t) => match &t.state {
                TransferState::Failed(why) => panic!("download failed: {why}"),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(std::fs::read(saved).unwrap(), song);

    // browse a remote user through the server
    bob.client.send(Command::Raw(msg::BROWSE, "alice".into()));
    let browsed = bob.wait(|e| if let Event::BrowseResult(r) = e { Some(r.nick.clone()) } else { None }).await;
    assert_eq!(browsed, "alice");

    // private messages and pings cross the link
    bob.client.send(Command::PrivateMessage { to: "alice".into(), text: "hi from beta".into() });
    let (from, text) = alice
        .wait(|e| if let Event::PrivateMessage { from, text } = e { Some((from.clone(), text.clone())) } else { None })
        .await;
    assert_eq!((from.as_str(), text.as_str()), ("bob", "hi from beta"));
    alice.client.send(Command::Ping("bob".into()));
    alice.wait(|e| matches!(e, Event::Pong { nick } if nick == "bob").then_some(())).await;

    // whois knows bob is on another server
    alice.client.send(Command::Whois("bob".into()));
    let fields = alice.wait(|e| if let Event::Whois(f) = e { Some(f.clone()) } else { None }).await;
    assert!(fields.iter().any(|(k, v)| k == "Status" && v == "Remote"), "{fields:?}");

    // shared chat channel
    alice.client.send(Command::Join("Lobby".into()));
    alice.wait(|e| matches!(e, Event::Joined { .. }).then_some(())).await;
    bob.client.send(Command::Join("Lobby".into()));
    let users = bob
        .wait(|e| match e {
            Event::ChannelUsers { users, .. } => Some(users.iter().map(|u| u.nick.clone()).collect::<Vec<_>>()),
            _ => None,
        })
        .await;
    assert!(users.contains(&"alice".to_owned()) && users.contains(&"bob".to_owned()), "{users:?}");
    alice.wait(|e| matches!(e, Event::UserJoined { user, .. } if user.nick == "bob").then_some(())).await;
    bob.client.send(Command::Say { channel: "Lobby".into(), text: "hello network".into() });
    let (nick, text) = alice
        .wait(|e| match e {
            Event::ChannelMessage { nick, text, .. } if nick == "bob" => Some((nick.clone(), text.clone())),
            _ => None,
        })
        .await;
    assert_eq!((nick.as_str(), text.as_str()), ("bob", "hello network"));

    // hot list follows users on the other server
    alice.client.send(Command::HotlistAdd("bob".into()));
    alice.wait(|e| matches!(e, Event::HotlistOnline { nick, .. } if nick == "bob").then_some(())).await;
    bob.client.send(Command::Disconnect);
    alice.wait(|e| matches!(e, Event::HotlistOffline { nick } if nick == "bob").then_some(())).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nicks_are_unique_across_the_network() {
    let (alpha, beta, _, _) = network().await;
    let carol = User::new("carol", alpha, &[("x.mp3", mp3::synth_mp3(10, 9))]).await;
    // make sure the servers are linked before trying the duplicate
    let mut dave = User::new("dave", beta, &[]).await;
    dave.search_until("x", 1).await;
    let mut c = tokio_util::codec::Framed::new(tokio::net::TcpStream::connect(beta).await.unwrap(), NapCodec);
    c.send(Message::new(msg::LOGIN, "carol pw 0 \"test\" 0")).await.unwrap();
    let m = tokio::time::timeout(WAIT, c.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(m.kind, msg::ERROR);
    assert!(m.data.contains("already logged in on server alpha"), "{}", m.data);
    carol.client.send(Command::Disconnect);
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_link_password_or_fingerprint_is_refused() {
    let (alpha, _, a_link, a_fp) = network().await;
    let _alice = User::new("alice", alpha, &[("secret song.mp3", mp3::synth_mp3(10, 9))]).await;
    // a server with the wrong password
    let peer = LinkPeer { host: "127.0.0.1".into(), port: a_link.port(), fingerprint: a_fp };
    let intruder = Server::bind(Config {
        links: vec![peer.clone()],
        link_password: Some("guess".into()),
        ..base_config("intruder")
    })
    .await
    .unwrap();
    let i_addr = intruder.local_addr();
    intruder.spawn();
    // a server that pins the wrong certificate (it would be talking to an impostor)
    let wrong_fp = (0..32).map(|_| "00").collect::<Vec<_>>().join(":");
    let wary = Server::bind(Config { links: vec![LinkPeer { fingerprint: wrong_fp, ..peer }], ..base_config("wary") })
        .await
        .unwrap();
    let w_addr = wary.local_addr();
    wary.spawn();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut eve = User::new("eve", i_addr, &[]).await;
    assert!(eve.search("secret song").await.is_empty());
    let mut walt = User::new("walt", w_addr, &[]).await;
    assert!(walt.search("secret song").await.is_empty());
}
