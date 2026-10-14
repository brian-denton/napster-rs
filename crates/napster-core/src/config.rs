//! Client settings (what the original kept under HKLM\Software\Napster).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub nick: String,
    pub password: String,
    pub email: String,
    /// `host:port` of the metaserver (if `use_metaserver`) or of the server.
    pub server: String,
    /// Ask a metaserver (port 8875) for a server first, like the original.
    pub use_metaserver: bool,
    /// Port we listen on for peer transfers. 0 = firewalled (push only).
    pub data_port: u16,
    /// 0..=10, see [`napster_proto::LINK_SPEEDS`].
    pub link_speed: u32,
    pub shared_dirs: Vec<PathBuf>,
    pub download_dir: PathBuf,
    pub max_uploads: u32,
    pub max_downloads: u32,
    pub hotlist: Vec<String>,
    /// File extensions to share (lower case, without the dot).
    pub extensions: Vec<String>,
    /// Join these channels after logging in.
    pub auto_join: Vec<String>,
    /// Interface the data port listens on.
    pub bind_host: String,
    /// Remember hashes between scans.
    pub cache_library: bool,
    /// Connect when the app starts (the original did).
    pub auto_connect: bool,
    /// Limit transfers to what `link_speed` really delivered around
    /// 1999-2001 (see [`PERIOD_RATES`]). Overrides the manual limits.
    pub authentic_speeds: bool,
    /// Total download speed cap in KB/s across all transfers (0 = unlimited).
    /// Used when `authentic_speeds` is off.
    pub download_limit_kbps: u32,
    /// Total upload speed cap in KB/s across all transfers (0 = unlimited).
    /// Used when `authentic_speeds` is off.
    pub upload_limit_kbps: u32,
    /// Look for a new signed release on GitHub at startup.
    pub check_for_updates: bool,
    /// Encrypt the server connection (napd's TLS port, 8889 by default).
    pub tls: bool,
    /// Where pinned server certificates are kept (default: next to the
    /// config file, `known_servers.json`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_servers: Option<PathBuf>,
    /// SOCKS5 proxy for the server connection (and optionally transfers).
    pub proxy: crate::proxy::ProxyConfig,
}

/// Typical sustained throughput in KB/s (download, upload) for each link
/// type, indexed like [`napster_proto::LINK_SPEEDS`]. Modems never reached
/// their rated speed, and 56K modems uploaded at 33.6K at best; home
/// DSL and cable of the era had small upload caps.
pub const PERIOD_RATES: [(f32, f32); 11] = [
    (0.0, 0.0),       // Unknown: unlimited
    (1.6, 1.6),       // 14.4 Kbps
    (3.2, 3.2),       // 28.8 Kbps
    (3.8, 3.6),       // 33.6 Kbps
    (5.0, 3.5),       // 56K (V.90)
    (7.5, 7.5),       // ISDN-64K
    (15.0, 15.0),     // ISDN-128K
    (150.0, 16.0),    // Cable (~1.5 Mbps / 128 Kbps)
    (90.0, 15.0),     // DSL (~768 / 128 Kbps)
    (180.0, 180.0),   // T1 (1.544 Mbps)
    (5000.0, 5000.0), // T3 (45 Mbps)
];

impl Default for ClientConfig {
    fn default() -> Self {
        let download_dir =
            dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")).join("Napster");
        let shared_dirs = dirs::audio_dir().into_iter().collect();
        Self {
            nick: String::new(),
            password: String::new(),
            email: "anon@napster.com".into(),
            server: format!("127.0.0.1:{}", napster_proto::SERVER_PORT),
            use_metaserver: false,
            data_port: napster_proto::DATA_PORT,
            link_speed: 8,
            shared_dirs,
            download_dir,
            max_uploads: 4,
            max_downloads: 4,
            hotlist: vec![],
            extensions: vec!["mp3".into(), "mp2".into(), "wma".into()],
            auto_join: vec![],
            bind_host: "0.0.0.0".into(),
            cache_library: true,
            auto_connect: true,
            authentic_speeds: true,
            download_limit_kbps: 0,
            upload_limit_kbps: 0,
            check_for_updates: true,
            tls: false,
            known_servers: None,
            proxy: crate::proxy::ProxyConfig::default(),
        }
    }
}

impl ClientConfig {
    /// Directory where partial downloads live until complete.
    pub fn incomplete_dir(&self) -> PathBuf {
        self.download_dir.join("Incomplete")
    }

    /// The proxy, if one is turned on.
    pub fn active_proxy(&self) -> Option<&crate::proxy::ProxyConfig> {
        self.proxy.enabled.then_some(&self.proxy)
    }

    /// Effective (download, upload) caps in bytes per second; 0 = none.
    pub fn rate_limits(&self) -> (u64, u64) {
        if self.authentic_speeds {
            let (down, up) = PERIOD_RATES.get(self.link_speed as usize).copied().unwrap_or((0.0, 0.0));
            ((down * 1024.0) as u64, (up * 1024.0) as u64)
        } else {
            (self.download_limit_kbps as u64 * 1024, self.upload_limit_kbps as u64 * 1024)
        }
    }

    pub fn default_path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("napster-rs").join("config.json")
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}
