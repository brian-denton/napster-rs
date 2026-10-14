//! A client behind a SOCKS5 proxy logs in, searches and downloads.

use std::sync::atomic::Ordering;
use std::time::Duration;

use napster_core::proxy::{test_proxy, ProxyConfig};
use napster_core::{mp3, Client, ClientConfig, Command, ConnState, Event, TransferState};
use napster_proto::SearchQuery;
use napster_server::{Config, Server};

const WAIT: Duration = Duration::from_secs(15);

async fn wait<T>(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>, mut f: impl FnMut(&Event) -> Option<T>) -> T {
    tokio::time::timeout(WAIT, async {
        loop {
            let e = rx.recv().await.unwrap();
            if let Event::Error(text) = &e {
                eprintln!("engine error: {text}");
            }
            if let Some(t) = f(&e) {
                return t;
            }
        }
    })
    .await
    .expect("timed out")
}

#[tokio::test(flavor = "multi_thread")]
async fn login_search_and_download_through_socks5() {
    let server = Server::bind(Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        meta_listen: None,
        stats_interval: Duration::from_secs(3600),
        ..Config::default()
    })
    .await
    .unwrap();
    let addr = server.local_addr();
    server.spawn();
    let (proxy_addr, relayed) = test_proxy(Some(("user".into(), "pass".into()))).await;

    let dir = tempfile::tempdir().unwrap();
    let base = |nick: &str, port: u16| ClientConfig {
        nick: nick.into(),
        password: "pw".into(),
        server: addr.to_string(),
        data_port: port,
        bind_host: "127.0.0.1".into(),
        shared_dirs: vec![dir.path().join(nick)],
        download_dir: dir.path().join(format!("{nick}-dl")),
        cache_library: false,
        check_for_updates: false,
        authentic_speeds: false,
        ..ClientConfig::default()
    };
    // alice shares a song, directly
    std::fs::create_dir_all(dir.path().join("alice")).unwrap();
    let data = mp3::synth_mp3(300, 9);
    std::fs::write(dir.path().join("alice").join("Proxy Song.mp3"), &data).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (alice, mut a_rx) = Client::start(base("alice", port), None);
    wait(&mut a_rx, |e| matches!(e, Event::Library(f) if f.len() == 1).then_some(())).await;
    alice.send(Command::Connect);
    wait(&mut a_rx, |e| matches!(e, Event::State(ConnState::Online { .. })).then_some(())).await;

    // bob goes through the proxy for everything
    let bob_cfg = ClientConfig {
        proxy: ProxyConfig {
            enabled: true,
            host: proxy_addr.ip().to_string(),
            port: proxy_addr.port(),
            username: "user".into(),
            password: "pass".into(),
            transfers: true,
        },
        ..base("bob", 0)
    };
    let (bob, mut b_rx) = Client::start(bob_cfg, None);
    bob.send(Command::Connect);
    wait(&mut b_rx, |e| matches!(e, Event::State(ConnState::Online { .. })).then_some(())).await;
    assert_eq!(relayed.load(Ordering::SeqCst), 1, "server connection went through the proxy");

    let result = loop {
        bob.send(Command::Search(SearchQuery::new("proxy song")));
        let mut found = None;
        wait(&mut b_rx, |e| match e {
            Event::SearchResult(r) => {
                found = Some(r.clone());
                None
            }
            Event::SearchEnd => Some(()),
            _ => None,
        })
        .await;
        if let Some(r) = found {
            break r;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    bob.send(Command::Download { nick: result.nick.clone(), file: result.file.clone() });
    let saved = wait(&mut b_rx, |e| match e {
        Event::Transfer(t) if t.state == TransferState::Complete => t.saved_to.clone(),
        Event::Transfer(t) => match &t.state {
            TransferState::Failed(why) => panic!("download failed: {why}"),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(std::fs::read(saved).unwrap(), data);
    assert_eq!(relayed.load(Ordering::SeqCst), 2, "the download went through the proxy too");
}
