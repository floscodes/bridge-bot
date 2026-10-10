//! Run a native Reticulum instance using a project-local configuration.
use anyhow::{Context, Result};
use bridge_bot::services::rrc::Network;
use clap::Parser;
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(about = "Start native Rust Reticulum and report reachable RRC hubs")]
struct Args {
    #[arg(long, default_value = ".bridge-bot/reticulum")]
    config_dir: PathBuf,
    /// Start the instance, report its interfaces, then shut it down.
    #[arg(long, conflicts_with = "scan_secs")]
    check: bool,
    /// Listen for RRC hub announces for this many seconds, then shut down.
    #[arg(long)]
    scan_secs: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into())
                .add_directive(
                    "reticulum_node=trace"
                        .parse()
                        .expect("valid logging directive"),
                ),
        )
        .init();
    let args = Args::parse();
    std::fs::create_dir_all(&args.config_dir)
        .context("creating Reticulum configuration directory")?;
    let directory = args.config_dir.canonicalize()?;
    let network = Network::start(directory.to_str()).await?;
    let result = async {
        let stats = network.handle.interface_stats().await?;
        tracing::info!(config = %directory.join("config").display(), interfaces = stats.interfaces.len(), "Reticulum instance started");
        for interface in stats.interfaces {
            tracing::info!(name = %interface.name, online = interface.online, "Reticulum interface");
        }
        if args.check { return Ok::<_, anyhow::Error>(()); }
        let mut hubs = network.handle.subscribe_announces(Some("rrc.hub".into()), true).await?;
        let deadline = tokio::time::sleep(Duration::from_secs(args.scan_secs.unwrap_or(u32::MAX.into())));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                result = tokio::signal::ctrl_c() => { result?; break; }
                _ = &mut deadline, if args.scan_secs.is_some() => { break; }
                event = hubs.recv() => {
                    let event = event.context("Reticulum announce subscription closed")?;
                    tracing::info!(hub = %hex::encode(event.destination_hash), hops = event.hops, "RRC hub discovered");
                }
            }
        }
        hubs.close().await?;
        Ok(())
    }.await;
    network.handle.shutdown_and_wait().await;
    tracing::info!("Reticulum instance stopped");
    result
}
