use anyhow::{Context, Result};
use bridge_bot::{config, retry, services};
use clap::Parser;
use config::{Config, Service};
use services::{Adapter, Incoming};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
};
use teloxide::prelude::*;
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinSet,
};
#[derive(Parser)]
#[command(
    version,
    about = "Configurable Telegram / Reticulum LXMF / RRC message bridge"
)]
struct Args {
    #[arg(short, long, default_value = "bridge-bot.toml")]
    config: PathBuf,
    #[arg(long)]
    check: bool,
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "off".into())
                .add_directive("bridge_bot=trace".parse().expect("valid logging directive")),
        )
        .init();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    if config.services.is_empty() {
        tracing::info!("no services registered");
        return Ok(());
    }
    if args.check {
        println!("Configuration valid");
        return Ok(());
    }
    let mut adapters: HashMap<String, Arc<dyn Adapter>> = HashMap::new();
    let mut networks = Vec::new();
    for (name, service) in &config.services {
        let adapter: Arc<dyn Adapter> = match service {
            Service::Telegram { token } => {
                let bot = Bot::new(token.clone());
                let me = bot
                    .get_me()
                    .await
                    .map_err(|_| anyhow::anyhow!("Telegram authentication failed"))?;
                if !me.can_read_all_group_messages {
                    tracing::warn!(service = %name,
                        "Telegram privacy mode enabled; ordinary group messages require bot administrator rights or disabling privacy mode in BotFather");
                }
                Arc::new(services::telegram::Telegram {
                    name: name.clone(),
                    bot,
                    own_id: me.id,
                    sent: Mutex::new(Default::default()),
                })
            }
            Service::Rrc {
                hub,
                identity_path,
                nickname,
                reticulum_config,
                interfaces,
            } => {
                let rooms: HashSet<String> = config
                    .active_bridges()
                    .flat_map(|b| [&b.from, &b.to])
                    .filter(|e| &e.service == name)
                    .filter_map(|e| e.room.clone())
                    .collect();
                if rooms.is_empty() {
                    continue;
                }
                let network = services::reticulum::Network::start_service(
                    name,
                    reticulum_config.as_deref(),
                    interfaces,
                )
                .await?;
                networks.push(network.clone());
                Arc::new(
                    services::rrc::Rrc::connect(
                        name.clone(),
                        network,
                        hub,
                        identity_path,
                        nickname.clone(),
                        rooms,
                    )
                    .await?,
                )
            }
            Service::Lxmf {
                identity_path,
                display_name,
                reticulum_config,
                interfaces,
            } => {
                let network = services::reticulum::Network::start_service(
                    name,
                    reticulum_config.as_deref(),
                    interfaces,
                )
                .await?;
                networks.push(network.clone());
                Arc::new(
                    services::lxmf::Lxmf::connect(
                        name.clone(),
                        network,
                        identity_path,
                        display_name,
                    )
                    .await?,
                )
            }
        };
        adapters.insert(name.clone(), adapter);
    }
    if adapters.is_empty() {
        tracing::info!("no services registered");
        return Ok(());
    }
    let (tx, mut rx) = mpsc::channel::<Incoming>(256);
    let mut tasks = JoinSet::new();
    for adapter in adapters.values() {
        let adapter = adapter.clone();
        let tx = tx.clone();
        tasks.spawn(async move { adapter.receive(tx).await });
    }
    drop(tx);
    let forwarder = retry::Forwarder::start(&config, &adapters, &mut tasks);
    let mut seen = HashSet::new();
    let mut order = VecDeque::new();
    tracing::info!("Bridge running");
    let result = async {
        loop {
            tokio::select! {
                result = tokio::signal::ctrl_c() => { result?; break; }
                result = tasks.join_next() => {
                    if let Some(r) = result { r.context("adapter task crashed")??; }
                    anyhow::bail!("bridge worker stopped unexpectedly");
                }
                message = rx.recv() => {
                    let message = message.context("all adapters disconnected")?;
                    tracing::debug!(service = %message.origin.service, chat = %message.origin.chat, room = ?message.origin.room,
                        message_thread_id = ?message.origin.message_thread_id, message_id = %message.id, "Incoming message received");
                    let key = (message.origin.clone(), message.id.clone());
                    if !seen.insert(key.clone()) { continue; }
                    order.push_back(key);
                    if order.len() > 8192 && let Some(old) = order.pop_front() { seen.remove(&old); }
                    let text = message.formatted();
                    let destinations = config.destinations(&message.origin);
                    if destinations.is_empty() && !config.has_inactive_route(&message.origin) && (message.origin.message_thread_id.is_some()
                        || matches!(config.services.get(&message.origin.service), Some(Service::Rrc { .. }))) {
                        tracing::error!(service = %message.origin.service, chat = %message.origin.chat, room = ?message.origin.room, message_thread_id = ?message.origin.message_thread_id,
                            "No explicit route for source room; message not forwarded");
                    }
                    for destination in destinations {
                        if let Err(error) = forwarder.enqueue(&destination, &text) {
                            tracing::error!(service = %destination.service, chat = %destination.chat, room = ?destination.room, message_thread_id = ?destination.message_thread_id, %error, "Forwarding queue rejected message");
                        }
                    }
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let pending = forwarder.pending_count();
    if pending > 0 {
        tracing::warn!(
            pending,
            "Shutting down; pending in-memory forwarding operations will be discarded"
        );
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    for adapter in adapters.values() {
        if let Err(error) = adapter.shutdown().await {
            tracing::warn!(%error, "Adapter shutdown failed");
        }
    }
    for network in &networks {
        network.handle.shutdown_and_wait().await;
    }
    result
}
