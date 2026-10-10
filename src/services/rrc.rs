mod protocol;

pub use super::reticulum::Network;
use super::*;
use anyhow::{Context, bail, ensure};
use protocol::{Envelope, MessageType};
use rns_runtime::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, RwLock, oneshot};

const SESSION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct Ready {
    handle: LinkSessionHandle,
    joined: HashSet<String>,
}

struct Connection {
    link: LinkSession,
    state: protocol::SessionState,
    buffered: Vec<Incoming>,
}

struct PendingEcho {
    room: String,
    text: String,
    reply: oneshot::Sender<Result<()>>,
}
type EchoMap = std::sync::Mutex<HashMap<[u8; 8], PendingEcho>>;

/// Remove outstanding confirmations even if the retry worker cancels this send.
struct EchoRegistration<'a> {
    echoes: &'a EchoMap,
    id: [u8; 8],
}
impl Drop for EchoRegistration<'_> {
    fn drop(&mut self) {
        self.echoes
            .lock()
            .expect("RRC echo mutex poisoned")
            .remove(&self.id);
    }
}

pub struct Rrc {
    name: String,
    network: Arc<Network>,
    hub: [u8; 16],
    identity: Identity,
    nickname: String,
    rooms: HashSet<String>,
    ready: RwLock<Option<Ready>>,
    initial: Mutex<Option<Connection>>,
    echoes: EchoMap,
}

impl Rrc {
    pub async fn connect(
        name: String,
        network: Arc<Network>,
        hub: &str,
        identity_path: &str,
        nickname: String,
        rooms: HashSet<String>,
    ) -> Result<Self> {
        ensure!(
            !rooms.is_empty(),
            "RRC service {name} has no configured rooms"
        );
        let path = Path::new(identity_path);
        let identity = match std::fs::metadata(path) {
            Ok(_) => Identity::from_file(path).context("loading persistent RRC identity")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).context("creating RRC identity directory")?;
                }
                let identity = Identity::new();
                identity
                    .to_file(path)
                    .context("saving persistent RRC identity")?;
                identity
            }
            Err(error) => return Err(error).context("reading RRC identity metadata"),
        };
        ensure!(
            identity.has_private_key(),
            "RRC identity has no private key"
        );
        let adapter = Self {
            name,
            network,
            hub: hex::decode(hub)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid RRC hub hash"))?,
            identity,
            nickname,
            rooms,
            ready: RwLock::new(None),
            initial: Mutex::new(None),
            echoes: std::sync::Mutex::new(HashMap::new()),
        };
        let connection = adapter.establish().await?;
        *adapter.initial.lock().await = Some(connection);
        Ok(adapter)
    }

    async fn establish(&self) -> Result<Connection> {
        let link = self
            .network
            .handle
            .connect_link(
                self.hub,
                self.identity.clone(),
                LinkConnectOptions {
                    path_timeout: Duration::from_secs(30),
                    establishment_timeout: Some(SESSION_TIMEOUT),
                    client_label: format!("bridge-bot.rrc.{}", self.name),
                    identify: true,
                    ..Default::default()
                },
            )
            .await
            .context("connecting RRC hub Link")?;
        let result = self.accept_session(link).await;
        if result.is_err() {
            *self.ready.write().await = None;
        }
        result
    }

    async fn accept_session(&self, link: LinkSession) -> Result<Connection> {
        let remote = self
            .network
            .handle
            .recall(self.hub)
            .await?
            .context("RRC hub identity unavailable after Link establishment")?;
        let destination = Destination::new(
            Some(&remote.identity),
            Direction::Out,
            DestType::Single,
            "rrc.hub",
        )?;
        if destination.hash != self.hub {
            link.handle.close().await;
            bail!("RRC hub destination does not use the rrc.hub aspect");
        }
        let mut connection = Connection {
            link,
            state: protocol::SessionState::new(
                remote.identity.hash,
                self.identity.hash,
                self.rooms.clone(),
            ),
            buffered: Vec::new(),
        };
        let handshake = async {
            connection.link.handle.send_packet(Envelope::hello(self.identity.hash, &self.nickname).encode()?).await?;
            let deadline = tokio::time::Instant::now() + SESSION_TIMEOUT;
            while !connection.state.is_ready() {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => bail!("RRC WELCOME/JOINED timeout"),
                    offer = connection.link.resource_offers.recv() => {
                        if let Some(offer) = offer { offer.reject().await?; }
                    }
                    event = connection.link.events.recv() => {
                        match event.context("RRC Link closed during handshake")? {
                            LinkSessionEvent::Packet { data, .. } => {
                                if let Some(message) = self.process_packet(&mut connection, &data).await? {
                                    ensure!(connection.buffered.len() < 256, "RRC startup buffer full");
                                    connection.buffered.push(message);
                                }
                            }
                            LinkSessionEvent::Closed { reason } => bail!("RRC Link closed during handshake: {reason:?}"),
                            _ => {}
                        }
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        }.await;
        if let Err(error) = handshake {
            connection.link.handle.close().await;
            return Err(error);
        }
        *self.ready.write().await = Some(Ready {
            handle: connection.link.handle.clone(),
            joined: connection.state.joined.clone(),
        });
        Ok(connection)
    }

    async fn process_packet(
        &self,
        connection: &mut Connection,
        data: &[u8],
    ) -> Result<Option<Incoming>> {
        let envelope = match Envelope::decode(data) {
            Ok(envelope) => envelope,
            Err(error) => {
                tracing::warn!(service = %self.name, %error, "Ignoring malformed RRC envelope");
                return Ok(None);
            }
        };
        tracing::trace!(service = %self.name, kind = ?envelope.kind, source = %hex::encode(envelope.source), room = ?envelope.room,
            "RRC envelope received");
        if envelope.kind == MessageType::Joined
            && connection.state.is_hub(envelope.source)
            && let Some(ciborium::Value::Array(entries)) = &envelope.body
        {
            let members: Vec<_> = entries
                .iter()
                .filter_map(|entry| match entry {
                    ciborium::Value::Bytes(hash) if hash.len() == 16 => Some(hex::encode(hash)),
                    _ => None,
                })
                .collect();
            tracing::debug!(service = %self.name, room = ?envelope.room, ?members,
                "RRC hub reported room members");
        }
        if envelope.kind == MessageType::Message && envelope.source == self.identity.hash {
            let mut echoes = self.echoes.lock().expect("RRC echo mutex poisoned");
            if echoes.get(&envelope.id).is_some_and(|pending| {
                envelope.room.as_ref().map(|r| r.to_lowercase()).as_deref()
                    == Some(pending.room.as_str())
                    && envelope.text() == Some(pending.text.as_str())
            }) && let Some(pending) = echoes.remove(&envelope.id)
            {
                let _ = pending.reply.send(Ok(()));
            }
            return Ok(None);
        }
        if envelope.kind == MessageType::Error
            && connection.state.is_ready()
            && connection.state.is_hub(envelope.source)
        {
            let reason = envelope.text().unwrap_or("non-text hub error");
            let reason_lower = reason.to_lowercase();
            let transient = reason_lower.contains("rate limit")
                || reason_lower.contains("rate-limit")
                || reason_lower.contains("rate_limited")
                || reason_lower.contains("too many");
            tracing::warn!(service = %self.name, %reason, "RRC hub rejected a send");
            let mut echoes = self.echoes.lock().expect("RRC echo mutex poisoned");
            let affected: Vec<_> = echoes
                .iter()
                .filter(|(_, pending)| {
                    envelope
                        .room
                        .as_ref()
                        .is_none_or(|r| r.to_lowercase() == pending.room)
                })
                .map(|(id, _)| *id)
                .collect();
            for id in affected {
                if let Some(pending) = echoes.remove(&id) {
                    let error = if transient {
                        anyhow::anyhow!("RRC hub rate limit")
                    } else {
                        SendFailure::Permanent(
                            "RRC hub rejected the message; check room permissions or limits",
                        )
                        .into()
                    };
                    let _ = pending.reply.send(Err(error));
                }
            }
            return Ok(None);
        }
        let outcome = connection.state.apply(&envelope)?;
        for room in outcome.join {
            let packet = Envelope::new(
                MessageType::Join,
                self.identity.hash,
                &self.nickname,
                Some(room),
                None,
            )
            .encode()?;
            connection.link.handle.send_packet(packet).await?;
        }
        if outcome.pong {
            let pong = Envelope::new(
                MessageType::Pong,
                self.identity.hash,
                &self.nickname,
                None,
                envelope.body.clone(),
            )
            .encode()?;
            connection.link.handle.send_packet(pong).await?;
        }
        if let Some(ref mut ready) = *self.ready.write().await {
            ready.joined.clone_from(&connection.state.joined);
        }
        if !outcome.chat {
            if matches!(envelope.kind, MessageType::Message | MessageType::Action) {
                tracing::debug!(service = %self.name, room = ?envelope.room, joined = ?connection.state.joined,
                    "RRC message ignored: source identity or joined room does not match");
            }
            return Ok(None);
        }
        let mut username = envelope
            .nickname
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_default();
        let identity = hex::encode(envelope.source);
        let identity_head = identity.get(..4).unwrap_or_default();
        let identity_tail = identity
            .get(identity.len() - 2..identity.len())
            .unwrap_or_default();

        // Add shortened identity hash to username
        username.push_str(" [");
        username.push_str(identity_head);
        username.push_str("..");
        username.push_str(identity_tail);
        username.push(']');

        let text = envelope.text().context("RRC text missing")?;
        Ok(Some(Incoming {
            origin: Endpoint {
                message_thread_id: None,
                service: self.name.clone(),
                chat: String::new(),
                room: Some(
                    envelope
                        .room
                        .clone()
                        .context("RRC room missing")?
                        .to_lowercase(),
                ),
            },
            id: format!(
                "{}:{}",
                hex::encode(envelope.source),
                hex::encode(envelope.id)
            ),
            username,
            service_label: "RRC",
            text: if envelope.kind == MessageType::Action {
                format!("* {text}")
            } else {
                text.into()
            },
        }))
    }
}

#[async_trait]
impl Adapter for Rrc {
    async fn receive(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        let mut connection = self
            .initial
            .lock()
            .await
            .take()
            .context("RRC receive already started")?;
        for message in connection.buffered.drain(..) {
            tx.try_send(message)
                .context("RRC inbound routing queue full or closed")?;
        }
        loop {
            tokio::select! {
                offer = connection.link.resource_offers.recv() => {
                    if let Some(offer) = offer { offer.reject().await?; }
                }
                event = connection.link.events.recv() => {
                    match event {
                        Some(LinkSessionEvent::Packet { data, .. }) => {
                            if let Some(message) = self.process_packet(&mut connection, &data).await? {
                                // Never block Link control and sends behind router backpressure.
                                tx.try_send(message).context("RRC inbound routing queue full or closed")?;
                            }
                        }
                        Some(LinkSessionEvent::Closed { .. }) | None => {
                            *self.ready.write().await = None;
                            self.echoes.lock().expect("RRC echo mutex poisoned").clear();
                            tracing::warn!(service = %self.name, "RRC Link lost; reconnecting and rejoining rooms");
                            let mut delay = 1;
                            loop {
                                tokio::time::sleep(Duration::from_secs(delay)).await;
                                let result = async {
                                    self.network.handle.request_path(self.hub, Default::default()).await?;
                                    self.establish().await
                                }.await;
                                match result {
                                    Ok(reconnected) => { connection = reconnected; break; }
                                    Err(error) if matches!(error.downcast_ref::<SendFailure>(), Some(SendFailure::Permanent(_))) => return Err(error),
                                    Err(error) => tracing::warn!(service = %self.name, %error, "RRC reconnect failed; retrying"),
                                }
                                delay = (delay * 2).min(30);
                            }
                            for message in connection.buffered.drain(..) {
                                tx.try_send(message).context("RRC inbound routing queue full or closed")?;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    async fn send(&self, chat: &str, text: &str) -> Result<()> {
        self.send_delivery(chat, &mut Delivery::new(text.into()))
            .await
    }
    async fn send_delivery(&self, chat: &str, delivery: &mut Delivery) -> Result<()> {
        let ready = self
            .ready
            .read()
            .await
            .clone()
            .context("RRC hub disconnected")?;
        if !ready.joined.contains(chat) {
            return Err(SendFailure::Permanent("RRC room is not joined").into());
        }
        if delivery.frames.is_none() {
            delivery.frames = Some(protocol::text_packets(
                self.identity.hash,
                &self.nickname,
                chat,
                &delivery.text,
                ready.handle.mdu(),
            )?);
        }
        let frames = delivery.frames.as_ref().context("RRC frames missing")?;
        for packet in frames.iter().skip(delivery.confirmed_parts) {
            let envelope = Envelope::decode(packet)?;
            let (reply, confirmation) = oneshot::channel();
            self.echoes.lock().expect("RRC echo mutex poisoned").insert(
                envelope.id,
                PendingEcho {
                    room: chat.into(),
                    text: envelope.text().context("RRC outgoing text missing")?.into(),
                    reply,
                },
            );
            let _registration = EchoRegistration {
                echoes: &self.echoes,
                id: envelope.id,
            };
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                ready.handle.send_packet(packet.clone()).await?;
                confirmation
                    .await
                    .context("RRC Link lost before echo confirmation")??;
                Ok::<_, anyhow::Error>(())
            })
            .await
            .context("RRC hub echo confirmation timed out; delivery is uncertain")
            .and_then(|r| r);
            result?;
            delivery.confirmed_parts += 1;
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        self.echoes.lock().expect("RRC echo mutex poisoned").clear();
        if let Some(ready) = self.ready.write().await.take() {
            for room in ready.joined {
                let packet = Envelope::new(
                    MessageType::Part,
                    self.identity.hash,
                    &self.nickname,
                    Some(room),
                    None,
                )
                .encode()?;
                let _ = ready.handle.send_packet(packet).await;
            }
            ready.handle.close().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod integration_tests;
