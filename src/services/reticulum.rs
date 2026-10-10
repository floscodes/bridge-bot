use crate::config::ReticulumInterface;
use anyhow::{Context, Result, ensure};
use rns_runtime::prelude::*;
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

pub struct Network {
    pub handle: ReticulumHandle,
}
impl Network {
    /// Low-level constructor retained for the standalone diagnostic tool and fixtures.
    pub async fn start(config: Option<&str>) -> Result<Arc<Self>> {
        let handle = init(
            config,
            None,
            ShutdownSignal::new(),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .context("starting native Reticulum runtime")?;
        Ok(Arc::new(Self { handle }))
    }

    pub async fn start_service(
        name: &str,
        directory: Option<&str>,
        interfaces: &[ReticulumInterface],
    ) -> Result<Arc<Self>> {
        // Never attach to another service's shared instance or replace an existing user config.
        let root = directory.unwrap_or(".bridge-bot/reticulum");
        let directory = Path::new(root).join(format!("managed-{}", hex::encode(name)));
        std::fs::create_dir_all(&directory).context("creating native service state directory")?;
        let mut config = String::from(
            "[reticulum]\nenable_transport = False\nshare_instance = No\n[logging]\nloglevel = 4\n[interfaces]\n",
        );
        for (index, interface) in interfaces.iter().enumerate() {
            interface.validate()?;
            config.push_str(&interface.render(index));
        }
        std::fs::write(directory.join("config"), config)
            .context("writing managed Reticulum interface config")?;
        let network = Self::start(directory.to_str()).await?;
        if !network.handle.startup_interface_failures().is_empty() {
            let failures = format!("{:?}", network.handle.startup_interface_failures());
            network.handle.shutdown_and_wait().await;
            anyhow::bail!("Reticulum interfaces failed to start for {name}: {failures}");
        }
        ensure!(
            network.handle.instance_mode == rns_runtime::reticulum::InstanceMode::Standalone,
            "service Reticulum runtime must own its interfaces"
        );
        tracing::info!(
            service = name,
            interfaces = interfaces.len(),
            "Native Reticulum stack started with service-owned interfaces"
        );
        Ok(network)
    }
}
impl Drop for Network {
    fn drop(&mut self) {
        self.handle.shutdown.trigger();
    }
}

pub fn load_identity(path: &str) -> Result<Identity> {
    let path = Path::new(path);
    match std::fs::metadata(path) {
        Ok(_) => return Identity::from_file(path).context("loading Reticulum service identity"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("reading Reticulum service identity metadata"),
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let identity = Identity::new();
    identity
        .to_file(path)
        .context("saving Reticulum service identity")?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_is_created_once_and_invalid_files_are_preserved() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("nested/identity");
        let first = load_identity(path.to_str().unwrap())?;
        let stored = std::fs::read(&path)?;
        let second = load_identity(path.to_str().unwrap())?;
        assert_eq!(first.hash, second.hash);
        assert_eq!(std::fs::read(&path)?, stored);
        std::fs::write(&path, b"invalid existing identity")?;
        assert!(load_identity(path.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read(&path)?, b"invalid existing identity");
        Ok(())
    }
}
