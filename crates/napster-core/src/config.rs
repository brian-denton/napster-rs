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
    /// Cap on each upload's speed in KB/s (0 = unlimited).
    pub upload_limit_kbps: u32,
}

impl Default for ClientConfig {
    fn default() -> Self {
        let download_dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Napster");
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
            upload_limit_kbps: 0,
        }
    }
}

impl ClientConfig {
    /// Directory where partial downloads live until complete.
    pub fn incomplete_dir(&self) -> PathBuf {
        self.download_dir.join("Incomplete")
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
