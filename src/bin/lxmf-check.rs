//! Resolve configured LXMF destinations without sending or announcing messages.
use anyhow::{Result, ensure};
use bridge_bot::{
    config::{Config, Service},
    services::rrc::Network,
};
use clap::Parser;
use rns_runtime::prelude::*;
use std::{collections::HashSet, path::PathBuf, time::Duration};

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value = "bridge-bot.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load(&Args::parse().config)?;
    let mut valid = true;
    for (name, service) in &config.services {
        let Service::Lxmf { interfaces, .. } = service else {
            continue;
        };
        // Independent state avoids changing the running bot's cache or identity.
        let directory = std::env::temp_dir().join(format!(
            "bridge-lxmf-check-{}-{}",
            std::process::id(),
            hex::encode(name)
        ));
        let network = Network::start_service("check", directory.to_str(), interfaces).await?;
        let result = async {
            let peers: HashSet<_> = config
                .active_bridges()
                .flat_map(|b| [&b.from, &b.to])
                .filter(|e| &e.service == name)
                .map(|e| e.chat.clone())
                .collect();
            for peer in peers {
                let destination: [u8; 16] = hex::decode(&peer)?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("invalid LXMF address"))?;
                let remote = resolve_destination_on_transport(
                    &network.handle.transport_tx,
                    destination,
                    DestinationResolveOptions::new(Duration::from_secs(30)),
                )
                .await?;
                let identity = Identity::from_public_key(&remote.public_key)?;
                let expected = Destination::new(
                    Some(&identity),
                    Direction::Out,
                    DestType::Single,
                    "lxmf.delivery",
                )?
                .hash;
                println!(
                    "service={name} configured={peer} identity={} lxmf_delivery={} matches={}",
                    hex::encode(identity.hash),
                    hex::encode(expected),
                    expected == destination
                );
                valid &= expected == destination;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        network.handle.shutdown_and_wait().await;
        result?;
    }
    ensure!(
        valid,
        "resolved identity does not match configured LXMF destination"
    );
    Ok(())
}
