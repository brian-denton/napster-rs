use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use napster_server::{Config, Server};

/// Napster-compatible server with a built-in metaserver (redirector).
#[derive(Parser)]
#[command(name = "napd", version)]
struct Args {
    /// Address for the main server.
    #[arg(long, default_value = "0.0.0.0:8888")]
    listen: SocketAddr,
    /// Address for the metaserver the original client contacts first.
    #[arg(long, default_value = "0.0.0.0:8875")]
    meta_listen: SocketAddr,
    /// Don't run the metaserver.
    #[arg(long)]
    no_meta: bool,
    /// Host/IP the metaserver tells clients to connect to (default: the
    /// address the metaserver connection arrived on).
    #[arg(long)]
    public_host: Option<String>,
    /// Directory for db.json (accounts and bans).
    #[arg(long, default_value = "napd-data")]
    data_dir: PathBuf,
    /// Text file with the message of the day (one line per 621 message).
    #[arg(long)]
    motd: Option<PathBuf>,
    /// Predefined channel (repeatable). Replaces the built-in list.
    #[arg(long = "channel")]
    channels: Vec<String>,
    /// Maximum search results per query.
    #[arg(long, default_value_t = 100)]
    max_results: u32,
    /// Nick that is given Elite (full admin) level (repeatable).
    #[arg(long)]
    elite: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let mut config = Config {
        listen: args.listen,
        meta_listen: (!args.no_meta).then_some(args.meta_listen),
        public_host: args.public_host,
        data_dir: Some(args.data_dir),
        max_results: args.max_results,
        elite: args.elite,
        ..Config::default()
    };
    if let Some(path) = args.motd {
        config.motd = std::fs::read_to_string(path)?.lines().map(String::from).collect();
    }
    if !args.channels.is_empty() {
        config.channels = args.channels;
    }
    Server::bind(config).await?.run().await
}
