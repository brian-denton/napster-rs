//! Napster desktop client.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod classic;
mod snapshot;

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,eframe=warn".into()),
        )
        .init();
    // optional: napster --config path\to\config.json
    let mut args = std::env::args().skip(1);
    let mut config_path = napster_core::ClientConfig::default_path();
    while let Some(a) = args.next() {
        if a == "--config" {
            config_path = args.next().map(PathBuf::from).unwrap_or(config_path);
        }
    }

    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Napster v2.0 BETA 10 (napster-rs)")
            .with_inner_size([960.0, 680.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Napster",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, config_path)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
