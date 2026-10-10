//! Connectivity checks; optionally send an explicitly requested RRC test message.
use anyhow::{Context, Result};
use bridge_bot::config::{Config, Service};
use bridge_bot::services::{
    Adapter,
    rrc::{Network, Rrc},
};
use clap::Parser;
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use teloxide::prelude::*;
use tokio::sync::mpsc;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value = "bridge-bot.toml")]
    config: PathBuf,
    /// Also verify that the configured RRC hub accepts the room joins; sends no chat messages.
    #[arg(long)]
    rrc: bool,
    /// Send this test message to the single configured RRC room after connecting.
    #[arg(long, requires = "rrc")]
    rrc_test_message: Option<String>,
    /// Use the configured bot identity instead of the separate check identity; stop the bot first.
    #[arg(long, requires = "rrc_test_message")]
    rrc_use_configured_identity: bool,
    /// Wait for another RRC participant's message containing this test marker.
    #[arg(long, requires = "rrc")]
    expect_rrc_text: Option<String>,
    #[arg(long, default_value_t = 180)]
    wait_secs: u64,
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
    for (name, service) in &config.services {
        let Service::Telegram { token } = service else {
            continue;
        };
        let bot = Bot::new(token.clone());
        let me = bot
            .get_me()
            .await
            .map_err(|_| anyhow::anyhow!("Telegram authentication check failed"))?;
        println!(
            "Telegram service {name}: authenticated as @{}",
            me.user.username.as_deref().unwrap_or("unknown")
        );
        println!(
            "Receives all group messages: {}",
            me.can_read_all_group_messages
        );
        let webhook = bot
            .get_webhook_info()
            .await
            .map_err(|_| anyhow::anyhow!("Telegram webhook check failed"))?;
        println!("Telegram webhook active: {}", webhook.url.is_some());
        let endpoints: HashSet<_> = config
            .bridges
            .iter()
            .flat_map(|route| [&route.from, &route.to])
            .filter(|endpoint| &endpoint.service == name)
            .collect();
        for endpoint in endpoints {
            let id: i64 = endpoint.chat.parse().context("invalid Telegram chat ID")?;
            let chat = bot.get_chat(ChatId(id)).await.map_err(|_| {
                anyhow::anyhow!("Telegram chat accessibility check failed for {id}")
            })?;
            let forum = matches!(&chat.kind, teloxide::types::ChatFullInfoKind::Public(chat)
                if matches!(&chat.kind, teloxide::types::ChatFullInfoPublicKind::Supergroup(group) if group.is_forum));
            let membership = bot
                .get_chat_member(ChatId(id), me.user.id)
                .await
                .map_err(|_| anyhow::anyhow!("Telegram bot membership check failed for {id}"))?;
            println!(
                "Telegram chat {id}: forum={forum}, configured message_thread_id={:?}, bot member={:?}",
                endpoint.message_thread_id, membership.kind
            );
            anyhow::ensure!(
                !forum || endpoint.message_thread_id.is_some(),
                "Forum {id} requires message_thread_id in its route"
            );
        }
    }
    if args.rrc {
        for (name, service) in &config.services {
            let Service::Rrc {
                hub,
                identity_path,
                nickname,
                reticulum_config,
                interfaces,
            } = service
            else {
                continue;
            };
            let rooms: HashSet<String> = config
                .active_bridges()
                .flat_map(|bridge| [&bridge.from, &bridge.to])
                .filter(|endpoint| &endpoint.service == name)
                .filter_map(|endpoint| endpoint.room.clone())
                .collect();
            anyhow::ensure!(
                !rooms.is_empty(),
                "RRC service {name} has no configured rooms to check"
            );
            let network = Network::start_service(
                &format!("{name}.check"),
                reticulum_config.as_deref(),
                interfaces,
            )
            .await?;
            let result = async {
                let identity = if args.rrc_use_configured_identity {
                    identity_path.clone()
                } else {
                    format!("{identity_path}.check")
                };
                let adapter = Arc::new(
                    Rrc::connect(
                        name.clone(),
                        network.clone(),
                        hub,
                        &identity,
                        nickname.clone(),
                        rooms.clone(),
                    )
                    .await?,
                );
                println!("RRC service {name}: hub {hub} connected, joined rooms {rooms:?}");
                if args.rrc_test_message.is_some() || args.expect_rrc_text.is_some() {
                    anyhow::ensure!(
                        rooms.len() == 1,
                        "RRC message test requires exactly one configured room"
                    );
                    let room = rooms.iter().next().expect("one room");
                    let (tx, mut incoming) = mpsc::channel::<bridge_bot::services::Incoming>(256);
                    let (matched_tx, matched_rx) = tokio::sync::oneshot::channel();
                    let expected = args.expect_rrc_text.clone();
                    let monitor = tokio::spawn(async move {
                        while let Some(message) = incoming.recv().await {
                            if expected
                                .as_ref()
                                .is_some_and(|text| message.text.contains(text))
                            {
                                println!(
                                    "RRC receive test passed in {}: {}",
                                    message.origin.address(),
                                    message.text
                                );
                                let _ = matched_tx.send(());
                                return;
                            }
                        }
                    });
                    let peer = adapter.clone();
                    let receiver = tokio::spawn(async move { peer.receive(tx).await });
                    let result = async {
                        if let Some(text) = &args.rrc_test_message {
                            adapter.send(room, text).await?;
                            println!("RRC test message accepted by hub in {room}: {text}");
                        }
                        if let Some(expected) = &args.expect_rrc_text {
                            println!("Waiting for RRC test marker {expected} in {room}");
                            tokio::time::timeout(
                                std::time::Duration::from_secs(args.wait_secs),
                                matched_rx,
                            )
                            .await
                            .context(
                                "Expected RRC test message was not received within the time limit",
                            )??;
                        }
                        Ok::<(), anyhow::Error>(())
                    }
                    .await;
                    receiver.abort();
                    let _ = receiver.await;
                    monitor.abort();
                    let _ = monitor.await;
                    adapter.shutdown().await?;
                    result?;
                    Ok(())
                } else {
                    adapter.shutdown().await
                }
            }
            .await;
            network.handle.shutdown_and_wait().await;
            result?;
        }
    }
    Ok(())
}
