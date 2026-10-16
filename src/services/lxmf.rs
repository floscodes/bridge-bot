use super::reticulum::{Network, load_identity};
use super::*;
use anyhow::{Context, bail, ensure};
use lxmf_core::{Payload, WireMessage};
use rns_runtime::prelude::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub struct Lxmf {
    pub name: String,
    pub source: [u8; 16],
    identity: Identity,
    network: Arc<Network>,
    destination: Mutex<Option<RegisteredDestination>>,
}

impl Lxmf {
    pub async fn connect(
        name: String,
        network: Arc<Network>,
        identity_path: &str,
        display_name: &str,
    ) -> Result<Self> {
        let identity = load_identity(identity_path)?;
        let app_data = lxmf_core::announce::encode_delivery_display_name_app_data(display_name)?;
        let destination = network
            .handle
            .register_destination(
                identity.clone(),
                "lxmf.delivery",
                DestinationRuntimeOptions {
                    proof_strategy: ProofStrategy::ProveAll,
                    resource_strategy: ResourceStrategy::AcceptApp,
                    resource_accept: Some(ResourceAcceptPolicy::new(|_, advertisement| {
                        advertisement.data_size <= MAX_MESSAGE_BYTES
                    })),
                    default_app_data: Some(app_data),
                    ..Default::default()
                },
            )
            .await?;
        let source = destination.handle.destination_hash();
        destination
            .handle
            .announce(DestinationAnnounceOptions::default())
            .await?;
        tracing::info!(service = %name, destination = %hex::encode(source), "Native LXMF delivery destination registered");
        Ok(Self {
            name,
            source,
            identity,
            network,
            destination: Mutex::new(Some(destination)),
        })
    }

    async fn decode(&self, bytes: &[u8]) -> Result<Option<Incoming>> {
        ensure!(
            bytes.len() <= MAX_MESSAGE_BYTES,
            "LXMF message exceeds inbound size limit"
        );
        let message = WireMessage::unpack(bytes)?;
        ensure!(
            message.destination == self.source,
            "LXMF message addressed to another destination"
        );
        if message.source == self.source {
            return Ok(None);
        }
        let recalled = resolve_destination_on_transport(
            &self.network.handle.transport_tx,
            message.source,
            DestinationResolveOptions::new(Duration::from_secs(5)),
        )
        .await?;
        let sender = Identity::from_public_key(&recalled.public_key)?;
        let sender_destination = Destination::new(
            Some(&sender),
            Direction::Out,
            DestType::Single,
            "lxmf.delivery",
        )?;
        ensure!(
            sender_destination.hash == message.source,
            "LXMF source does not match sender identity"
        );
        let signature = message.signature.context("LXMF signature missing")?;
        ensure!(
            sender.verify(&signing_data(&message)?, &signature),
            "LXMF signature invalid"
        );
        let text = message
            .payload
            .content
            .as_ref()
            .map(|v| std::str::from_utf8(v))
            .transpose()
            .context("LXMF content is not UTF-8")?
            .unwrap_or_default();
        if text.is_empty() {
            return Ok(None);
        }
        let source = hex::encode(message.source);
        let display_name =
            lxmf_core::display_name_from_app_data(recalled.app_data.as_deref()).unwrap_or_default();
        let username = format!(
            "{display_name} [{}..{}]",
            &source[..4],
            &source[source.len() - 2..]
        );
        Ok(Some(Incoming {
            origin: Endpoint {
                lxmf_group: false,
                service: self.name.clone(),
                chat: source,
                room: None,
                message_thread_id: None,
            },
            id: hex::encode(message.try_message_id()?),
            username,
            service_label: "LXMF",
            text: text.into(),
        }))
    }

    async fn forward_incoming(&self, tx: &mpsc::Sender<Incoming>, bytes: &[u8]) -> Result<()> {
        match self.decode(bytes).await {
            Ok(Some(message)) => {
                tx.send(message).await?;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(service = %self.name, %error, "Ignoring invalid or unverifiable LXMF message")
            }
        }
        Ok(())
    }
}

fn signing_data(message: &WireMessage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&message.destination);
    bytes.extend_from_slice(&message.source);
    bytes.extend_from_slice(&message.payload.to_msgpack_without_stamp()?);
    bytes.extend_from_slice(&message.try_message_id()?);
    Ok(bytes)
}

struct LinkGuard(LinkSessionHandle);
impl Drop for LinkGuard {
    fn drop(&mut self) {
        let handle = self.0.clone();
        tokio::spawn(async move {
            handle.close().await;
        });
    }
}

struct CancelWork(Arc<AtomicBool>);
impl Drop for CancelWork {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

async fn add_stamp(frame: &mut Vec<u8>, cost: u32) -> Result<()> {
    if cost > lxmf_core::stamp::MAX_MINEABLE_STAMP_COST {
        return Err(
            SendFailure::Permanent("LXMF destination requires an unattainable stamp cost").into(),
        );
    }
    let mut message = WireMessage::unpack(frame)?;
    let id = message.try_message_id()?;
    let existing = message.payload.stamp.as_ref().map(|stamp| stamp.to_vec());
    let cancel = Arc::new(AtomicBool::new(false));
    let _guard = CancelWork(cancel.clone());
    let stamp = tokio::task::spawn_blocking(move || {
        if lxmf_core::stamp::validate_stamp(existing.as_deref(), &id, cost, &[]).is_some() {
            return existing;
        }
        lxmf_core::stamp::generate_stamp_until_cancelled(&id, cost, || {
            cancel.load(Ordering::Relaxed)
        })
    })
    .await?
    .context("LXMF stamp generation cancelled")?;
    message.payload.stamp = Some(stamp.into());
    *frame = message.pack()?;
    Ok(())
}

#[async_trait]
impl Adapter for Lxmf {
    async fn receive(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        let mut destination = self
            .destination
            .lock()
            .await
            .take()
            .context("LXMF receiver already started")?;
        let mut announces = tokio::time::interval(Duration::from_secs(600));
        announces.tick().await;
        loop {
            tokio::select! {
                packet = destination.events.packets.recv() => {
                    let packet = packet.context("LXMF destination stopped")?;
                    let mut bytes = self.source.to_vec();
                    bytes.extend_from_slice(&packet.data);
                    self.forward_incoming(&tx, &bytes).await?;
                }
                packet = destination.events.link_packets.recv() => {
                    let (bytes, _) = packet.context("LXMF destination stopped")?;
                    self.forward_incoming(&tx, &bytes).await?;
                }
                resource = destination.events.resource_completions.recv() => {
                    let resource = resource.context("LXMF destination stopped")?;
                    self.forward_incoming(&tx, &resource.data).await?;
                }
                _ = destination.events.links_established.recv() => {}
                _ = destination.events.links_identified.recv() => {}
                _ = destination.events.links_closed.recv() => {}
                _ = destination.events.resource_events.recv() => {}
                _ = announces.tick() => { destination.handle.announce(DestinationAnnounceOptions::default()).await?; }
            }
        }
    }

    async fn send(&self, chat: &str, text: &str) -> Result<()> {
        self.send_delivery(chat, &mut Delivery::new(text.into()))
            .await
    }

    async fn send_delivery(&self, chat: &str, delivery: &mut Delivery) -> Result<()> {
        let destination: [u8; 16] = hex::decode(chat)?
            .try_into()
            .map_err(|_| SendFailure::Permanent("invalid LXMF destination"))?;
        ensure!(
            destination != self.source,
            "LXMF peer cannot be the bot itself"
        );
        if delivery.frames.is_none() {
            let payload = Payload::new(
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64(),
                Some(delivery.text.as_bytes().to_vec()),
                Some(Vec::new()),
                Some(rmpv::Value::Map(Vec::new())),
                None,
            );
            let mut message = WireMessage::new(destination, self.source, payload);
            message.signature = Some(
                self.identity
                    .sign(&signing_data(&message)?)
                    .context("LXMF signing key unavailable")?,
            );
            let packed = message.pack()?;
            if packed.len() > MAX_MESSAGE_BYTES {
                return Err(SendFailure::Permanent("LXMF message exceeds 1 MiB size limit").into());
            }
            delivery.transport_id = Some(hex::encode(message.try_message_id()?));
            delivery.frames = Some(vec![packed]);
        }
        let remote = resolve_destination_on_transport(
            &self.network.handle.transport_tx,
            destination,
            DestinationResolveOptions::new(Duration::from_secs(15)),
        )
        .await?;
        let remote_identity = Identity::from_public_key(&remote.public_key)?;
        let expected_destination = Destination::new(
            Some(&remote_identity),
            Direction::Out,
            DestType::Single,
            "lxmf.delivery",
        )?
        .hash;
        ensure!(
            expected_destination == destination,
            "LXMF destination/identity mismatch: service={}, configured={}, identity={}, expected_lxmf_delivery={}; resolved identity does not match the configured LXMF address",
            self.name,
            hex::encode(destination),
            hex::encode(remote_identity.hash),
            hex::encode(expected_destination),
        );
        if let Some(cost) =
            lxmf_core::stamp_cost_from_app_data(remote.app_data.as_deref()).filter(|cost| *cost > 0)
        {
            let cost = u32::try_from(cost).map_err(|_| {
                SendFailure::Permanent("LXMF destination advertised an invalid stamp cost")
            })?;
            add_stamp(
                &mut delivery.frames.as_mut().context("LXMF frame missing")?[0],
                cost,
            )
            .await?;
        }
        let mut session = self
            .network
            .handle
            .connect_link(
                destination,
                self.identity.clone(),
                LinkConnectOptions {
                    path_timeout: Duration::from_secs(15),
                    establishment_timeout: Some(Duration::from_secs(30)),
                    client_label: format!("bridge-bot.lxmf.{}", self.name),
                    identify: true,
                    ..Default::default()
                },
            )
            .await?;
        let _guard = LinkGuard(session.handle.clone());
        let resource = session
            .handle
            .send_resource_bytes(
                delivery.frames.as_ref().context("LXMF frame missing")?[0].clone(),
                ResourceOptions::default(),
            )
            .await?;
        let completion = resource.concluded();
        tokio::pin!(completion);
        loop {
            tokio::select! {
                result = &mut completion => { result?; return Ok(()); }
                event = session.events.recv() => {
                    if matches!(event, None | Some(LinkSessionEvent::Closed { .. })) { bail!("LXMF delivery link closed before confirmation"); }
                }
                offer = session.resource_offers.recv() => {
                    if let Some(offer) = offer { offer.reject().await?; }
                }
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        if let Some(destination) = self.destination.lock().await.take() {
            destination.close().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod integration_tests;
