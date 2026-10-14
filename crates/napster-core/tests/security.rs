//! Security behavior against a real `napd`: TLS pinning, login lockout,
//! connection/flood/share limits, and refusing non-audio downloads.

use std::net::SocketAddr;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use napster_core::{Client, ClientConfig, Command, ConnState, Event};
use napster_proto::{msg, Message, NapCodec};
use napster_server::{Config, Server};
use tokio_util::codec::Framed;

const WAIT: Duration = Duration::from_secs(15);

async fn server_with(tweak: impl FnOnce(&mut Config)) -> Server {
    let mut config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        meta_listen: None,
        stats_interval: Duration::from_secs(3600),
        ..Config::default()
    };
    tweak(&mut config);
    Server::bind(config).await.unwrap()
}

type Raw = Framed<tokio::net::TcpStream, NapCodec>;

async fn raw(addr: SocketAddr) -> Raw {
    Framed::new(tokio::net::TcpStream::connect(addr).await.unwrap(), NapCodec)
}

/// Next message, or None once the server has closed the connection.
async fn next(c: &mut Raw) -> Option<Message> {
    tokio::time::timeout(WAIT, c.next()).await.expect("server reply").and_then(Result::ok)
}

async fn raw_login(addr: SocketAddr, nick: &str, pass: &str) -> (Raw, Message) {
    let mut c = raw(addr).await;
    c.send(Message::new(msg::LOGIN, format!("{nick} {pass} 0 \"test\" 0"))).await.unwrap();
    let m = next(&mut c).await.expect("login reply");
    (c, m)
}

fn client_config(dir: &std::path::Path, server: SocketAddr) -> ClientConfig {
    ClientConfig {
        nick: "secure".into(),
        password: "pw".into(),
        server: server.to_string(),
        data_port: 0,
        shared_dirs: vec![],
        download_dir: dir.join("dl"),
        cache_library: false,
        check_for_updates: false,
        authentic_speeds: false,
        known_servers: Some(dir.join("known_servers.json")),
        ..ClientConfig::default()
    }
}

/// Connect; returns the result, any Info messages, and the client.
async fn connect(config: ClientConfig) -> (Result<(), String>, Vec<String>, Client) {
    let (client, mut events) = Client::start(config, None);
    client.send(Command::Connect);
    let mut info = vec![];
    let result = tokio::time::timeout(WAIT, async {
        loop {
            match events.recv().await.unwrap() {
                Event::State(ConnState::Online { .. }) => return Ok(()),
                Event::Error(e) => return Err(e),
                Event::Info(i) => info.push(i),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    (result, info, client)
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_pins_certificate_and_detects_impersonation() {
    let server = server_with(|c| c.tls_listen = Some("127.0.0.1:0".parse().unwrap())).await;
    let tls_addr = server.tls_addr().unwrap();
    let fp = server.tls_fingerprint().unwrap().to_owned();
    server.spawn();
    let dir = tempfile::tempdir().unwrap();
    let config = ClientConfig { tls: true, ..client_config(dir.path(), tls_addr) };
    let known = config.known_servers.clone().unwrap();

    // first contact: trusted and pinned
    let (r, info, client) = connect(config.clone()).await;
    r.unwrap();
    assert!(info.iter().any(|i| i.contains(&fp)), "{info:?}");
    let pins: std::collections::BTreeMap<String, String> =
        serde_json::from_slice(&std::fs::read(&known).unwrap()).unwrap();
    assert_eq!(pins.get(&tls_addr.to_string()), Some(&fp));
    client.send(Command::Disconnect);

    // second contact: same certificate, accepted quietly
    let (r, info, client) = connect(config.clone()).await;
    r.unwrap();
    assert!(!info.iter().any(|i| i.contains("trusting")), "{info:?}");
    client.send(Command::Disconnect);

    // a different certificate at the same address is refused
    let fake = serde_json::json!({ tls_addr.to_string(): "00:11:22" });
    std::fs::write(&known, serde_json::to_vec(&fake).unwrap()).unwrap();
    let (r, _, _) = connect(config).await;
    let err = r.unwrap_err();
    assert!(err.contains("certificate has changed"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_client_cannot_talk_to_tls_port() {
    let server = server_with(|c| c.tls_listen = Some("127.0.0.1:0".parse().unwrap())).await;
    let tls_addr = server.tls_addr().unwrap();
    server.spawn();
    let dir = tempfile::tempdir().unwrap();
    let (r, _, _) = connect(client_config(dir.path(), tls_addr)).await;
    assert!(r.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_logins_lock_out_the_address() {
    let server = server_with(|_| {}).await;
    let addr = server.local_addr();
    server.spawn();
    let (_, m) = raw_login(addr, "victim", "right").await;
    assert_eq!(m.kind, msg::LOGIN_ACK);
    for _ in 0..5 {
        let (_, m) = raw_login(addr, "victim", "wrong").await;
        assert_eq!((m.kind, m.data.as_str()), (msg::ERROR, "invalid password"));
    }
    // even the right password is refused until the lockout expires
    let (_, m) = raw_login(addr, "victim", "right").await;
    assert_eq!(m.kind, msg::ERROR);
    assert!(m.data.contains("Too many failed logins"), "{}", m.data);
}

#[tokio::test(flavor = "multi_thread")]
async fn signups_per_address_are_limited() {
    let server = server_with(|_| {}).await;
    let addr = server.local_addr();
    server.spawn();
    for i in 0..5 {
        let (_, m) = raw_login(addr, &format!("new{i}"), "pw").await;
        assert_eq!(m.kind, msg::LOGIN_ACK);
    }
    let (_, m) = raw_login(addr, "new5", "pw").await;
    assert!(m.data.contains("Too many new accounts"), "{}", m.data);
    // existing accounts can still log in
    let (_, m) = raw_login(addr, "new0", "pw").await;
    assert_eq!(m.kind, msg::LOGIN_ACK);
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_per_address_are_limited() {
    let server = server_with(|c| c.max_conns_per_ip = 3).await;
    let addr = server.local_addr();
    server.spawn();
    let mut open = vec![];
    for _ in 0..3 {
        open.push(raw(addr).await);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut extra = raw(addr).await;
    assert!(next(&mut extra).await.is_none(), "4th connection should be closed");
    // closing one frees a slot
    drop(open.pop());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, m) = raw_login(addr, "late", "pw").await;
    assert_eq!(m.kind, msg::LOGIN_ACK);
}

#[tokio::test(flavor = "multi_thread")]
async fn flooding_is_throttled_then_disconnected() {
    let server = server_with(|c| {
        c.msg_rate = 1.0;
        c.msg_burst = 10.0;
    })
    .await;
    let addr = server.local_addr();
    server.spawn();
    let (mut c, m) = raw_login(addr, "flooder", "pw").await;
    assert_eq!(m.kind, msg::LOGIN_ACK);
    for _ in 0..400 {
        if c.send(Message::empty(msg::SERVER_STATS)).await.is_err() {
            break;
        }
    }
    let (mut warned, mut cut) = (false, false);
    while let Some(m) = next(&mut c).await {
        warned |= m.data.contains("too fast");
        cut |= m.data.contains("flooding");
    }
    assert!(warned && cut, "warned={warned} cut={cut}");
}

#[tokio::test(flavor = "multi_thread")]
async fn shares_per_user_are_capped() {
    let server = server_with(|c| c.max_shares = 3).await;
    let addr = server.local_addr();
    server.spawn();
    let (mut c, _) = raw_login(addr, "hoarder", "pw").await;
    for i in 0..10 {
        c.send(Message::new(msg::SHARE_FILE, format!("\"song{i}.mp3\" abc 1000 128 44100 60"))).await.unwrap();
    }
    let mut warned = false;
    while let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_millis(500), c.next()).await {
        warned |= m.data.contains("at most 3 files");
    }
    assert!(warned);
    let (mut c2, _) = raw_login(addr, "browser", "pw").await;
    c2.send(Message::new(msg::BROWSE, "hoarder")).await.unwrap();
    let mut files = 0;
    loop {
        let m = next(&mut c2).await.unwrap();
        match m.kind {
            msg::BROWSE_RESULT => files += 1,
            msg::BROWSE_END => break,
            _ => {}
        }
    }
    assert_eq!(files, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn non_audio_downloads_are_refused() {
    let server = server_with(|_| {}).await;
    let addr = server.local_addr();
    server.spawn();
    let dir = tempfile::tempdir().unwrap();
    let (client, mut events) = Client::start(client_config(dir.path(), addr), None);
    let file = napster_proto::FileInfo {
        filename: "C:\\MP3\\Hit Song.mp3.exe".into(),
        md5: "x".into(),
        size: 1000,
        bitrate: 128,
        frequency: 44100,
        seconds: 60,
    };
    client.send(Command::Download { nick: "mallory".into(), file });
    let err = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(Event::Error(e)) = events.recv().await {
                return e;
            }
        }
    })
    .await
    .unwrap();
    assert!(err.contains("only downloads audio"), "{err}");
}
