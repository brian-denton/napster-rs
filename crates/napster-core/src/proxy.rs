//! SOCKS5 proxy support (RFC 1928, with RFC 1929 user/password auth), the
//! successor to the original client's "Firewall Settings > Proxy Server".
//!
//! Host names are sent to the proxy unresolved so DNS lookups don't leak
//! around it.

use std::io;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Also route outgoing peer connections (downloads, pushed uploads,
    /// browsing) through the proxy. Incoming connections can't be proxied,
    /// so set the data port to 0 if nothing can reach you directly.
    pub transfers: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 1080,
            username: String::new(),
            password: String::new(),
            transfers: true,
        }
    }
}

impl ProxyConfig {
    /// `socks5://user:pass@host:port` for HTTP clients (the update check).
    pub fn url(&self) -> String {
        let auth =
            if self.username.is_empty() { String::new() } else { format!("{}:{}@", self.username, self.password) };
        format!("socks5://{auth}{}:{}", self.host, self.port)
    }
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// Open a TCP connection to `host:port` through the SOCKS5 proxy.
pub async fn connect(proxy: &ProxyConfig, host: &str, port: u16) -> io::Result<TcpStream> {
    let mut s = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .map_err(|e| err(format!("Error connecting socket to proxy {}:{}: {e}", proxy.host, proxy.port)))?;
    s.set_nodelay(true)?;
    let with_auth = !proxy.username.is_empty();
    // greeting: version 5, offered methods
    if with_auth {
        s.write_all(&[5, 2, 0x00, 0x02]).await?;
    } else {
        s.write_all(&[5, 1, 0x00]).await?;
    }
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).await?;
    if reply[0] != 5 {
        return Err(err("Error communicating with proxy! (not a SOCKS5 proxy)"));
    }
    match reply[1] {
        0x00 => {}
        0x02 if with_auth => {
            let (u, p) = (proxy.username.as_bytes(), proxy.password.as_bytes());
            if u.len() > 255 || p.len() > 255 {
                return Err(err("proxy user name or password is too long"));
            }
            let mut msg = vec![1, u.len() as u8];
            msg.extend_from_slice(u);
            msg.push(p.len() as u8);
            msg.extend_from_slice(p);
            s.write_all(&msg).await?;
            let mut r = [0u8; 2];
            s.read_exact(&mut r).await?;
            if r[1] != 0 {
                return Err(err("the proxy rejected the user name or password"));
            }
        }
        0x02 => return Err(err("the proxy requires a user name and password")),
        _ => return Err(err("the proxy accepts none of our authentication methods")),
    }
    // CONNECT request
    let mut req = vec![5, 1, 0];
    match host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            req.push(1);
            req.extend_from_slice(&v4.octets());
        }
        Ok(IpAddr::V6(v6)) => {
            req.push(4);
            req.extend_from_slice(&v6.octets());
        }
        Err(_) => {
            if host.len() > 255 {
                return Err(err("host name too long for SOCKS5"));
            }
            req.push(3);
            req.push(host.len() as u8);
            req.extend_from_slice(host.as_bytes());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0 {
        let why = match head[1] {
            1 => "general failure",
            2 => "connection not allowed by ruleset",
            3 => "network unreachable",
            4 => "host unreachable",
            5 => "connection refused",
            6 => "TTL expired",
            7 => "command not supported",
            8 => "address type not supported",
            _ => "unknown error",
        };
        return Err(err(format!("proxy could not connect to {host}:{port}: {why}")));
    }
    // skip the bound address
    let skip = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut n = [0u8; 1];
            s.read_exact(&mut n).await?;
            n[0] as usize
        }
        _ => return Err(err("malformed reply from proxy")),
    };
    let mut rest = vec![0u8; skip + 2];
    s.read_exact(&mut rest).await?;
    Ok(s)
}

/// A minimal SOCKS5 server for tests: CONNECT only, optional user/pass.
/// Returns its address and a counter of connections it relayed.
#[doc(hidden)]
pub async fn test_proxy(
    auth: Option<(String, String)>,
) -> (std::net::SocketAddr, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::Ordering;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = count.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let (auth, c) = (auth.clone(), c.clone());
            tokio::spawn(async move {
                let r: io::Result<()> = async {
                    let mut h = [0u8; 2];
                    s.read_exact(&mut h).await?;
                    let mut methods = vec![0u8; h[1] as usize];
                    s.read_exact(&mut methods).await?;
                    if let Some((user, pass)) = &auth {
                        if !methods.contains(&2) {
                            s.write_all(&[5, 0xff]).await?;
                            return Ok(());
                        }
                        s.write_all(&[5, 2]).await?;
                        let mut v = [0u8; 2];
                        s.read_exact(&mut v).await?;
                        let mut u = vec![0u8; v[1] as usize];
                        s.read_exact(&mut u).await?;
                        let mut pl = [0u8; 1];
                        s.read_exact(&mut pl).await?;
                        let mut p = vec![0u8; pl[0] as usize];
                        s.read_exact(&mut p).await?;
                        let ok = u == user.as_bytes() && p == pass.as_bytes();
                        s.write_all(&[1, if ok { 0 } else { 1 }]).await?;
                        if !ok {
                            return Ok(());
                        }
                    } else {
                        s.write_all(&[5, 0]).await?;
                    }
                    let mut req = [0u8; 4];
                    s.read_exact(&mut req).await?;
                    let host = match req[3] {
                        1 => {
                            let mut a = [0u8; 4];
                            s.read_exact(&mut a).await?;
                            std::net::Ipv4Addr::from(a).to_string()
                        }
                        3 => {
                            let mut n = [0u8; 1];
                            s.read_exact(&mut n).await?;
                            let mut name = vec![0u8; n[0] as usize];
                            s.read_exact(&mut name).await?;
                            String::from_utf8_lossy(&name).into_owned()
                        }
                        _ => return Ok(()),
                    };
                    let mut port = [0u8; 2];
                    s.read_exact(&mut port).await?;
                    let port = u16::from_be_bytes(port);
                    match TcpStream::connect((host.as_str(), port)).await {
                        Ok(mut target) => {
                            c.fetch_add(1, Ordering::SeqCst);
                            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                            let _ = tokio::io::copy_bidirectional(&mut s, &mut target).await;
                        }
                        Err(_) => s.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await?,
                    }
                    Ok(())
                }
                .await;
                let _ = r;
            });
        }
    });
    (addr, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    async fn echo_server() -> std::net::SocketAddr {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        addr
    }

    fn cfg(addr: std::net::SocketAddr, user: &str, pass: &str) -> ProxyConfig {
        ProxyConfig {
            enabled: true,
            host: addr.ip().to_string(),
            port: addr.port(),
            username: user.into(),
            password: pass.into(),
            transfers: true,
        }
    }

    #[tokio::test]
    async fn connects_through_proxy_with_and_without_auth() {
        let echo = echo_server().await;
        for auth in [None, Some(("bob".to_owned(), "secret".to_owned()))] {
            let (proxy, count) = test_proxy(auth.clone()).await;
            let (u, p) = auth.clone().unwrap_or_default();
            // by name (resolved by the proxy) and by IP
            for host in ["localhost", "127.0.0.1"] {
                let mut s = connect(&cfg(proxy, &u, &p), host, echo.port()).await.unwrap();
                s.write_all(b"hello").await.unwrap();
                let mut buf = [0u8; 5];
                s.read_exact(&mut buf).await.unwrap();
                assert_eq!(&buf, b"hello");
            }
            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn reports_bad_credentials_and_refusals() {
        let (proxy, _) = test_proxy(Some(("bob".into(), "secret".into()))).await;
        let e = connect(&cfg(proxy, "bob", "wrong"), "127.0.0.1", 1).await.unwrap_err();
        assert!(e.to_string().contains("rejected the user name"), "{e}");
        let e = connect(&cfg(proxy, "", ""), "127.0.0.1", 1).await.unwrap_err();
        assert!(e.to_string().contains("none of our authentication"), "{e}");
        let (open, _) = test_proxy(None).await;
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = connect(&cfg(open, "", ""), "127.0.0.1", closed).await.unwrap_err();
        assert!(e.to_string().contains("connection refused"), "{e}");
    }

    #[test]
    fn url() {
        let mut c = ProxyConfig { host: "p.example".into(), port: 1080, ..ProxyConfig::default() };
        assert_eq!(c.url(), "socks5://p.example:1080");
        c.username = "u".into();
        c.password = "p".into();
        assert_eq!(c.url(), "socks5://u:p@p.example:1080");
    }
}
