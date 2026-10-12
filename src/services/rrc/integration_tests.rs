//! Exercise the production adapter over two native runtimes and real loopback TCP.
use super::*;
use crate::config::ReticulumInterface;
use ciborium::Value;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Member {
    identity: [u8; 16],
    nickname: String,
    rooms: HashSet<String>,
}

async fn hub_task(
    mut destination: RegisteredDestination,
    hub_identity: [u8; 16],
    disconnect_after: Arc<AtomicUsize>,
) -> Result<()> {
    let mut members: HashMap<[u8; 16], Member> = HashMap::new();
    loop {
        tokio::select! {
            biased;
            identified = destination.events.links_identified.recv() => {
                let Some((link, identity)) = identified else { break; };
                members.entry(link).or_default().identity = identity;
            }
            packet = destination.events.link_packets.recv() => {
                let Some((data, link)) = packet else { break; };
                let mut message = Envelope::decode(&data)?;
                let member = members.get_mut(&link).context("client sent data before identifying")?;
                ensure!(message.source == member.identity, "client source does not match Link identity");
                match message.kind {
                    MessageType::Hello => {
                        member.nickname = message.nickname.unwrap_or_default();
                        let welcome = Envelope::new(MessageType::Welcome, hub_identity, "hub", None, None);
                        destination.handle.send_link_packet(link, welcome.encode()?).await?;
                    }
                    MessageType::Join => {
                        let room = message.room.context("JOIN without room")?;
                        member.rooms.insert(room.clone());
                        let joined = Envelope::new(MessageType::Joined, hub_identity, "hub", Some(room.clone()), None);
                        destination.handle.send_link_packet(link, joined.encode()?).await?;
                        let notice = Envelope::new(MessageType::Notice, hub_identity, "hub", Some(room), Some(Value::Text("system notice".into())));
                        destination.handle.send_link_packet(link, notice.encode()?).await?;
                    }
                    MessageType::Message | MessageType::Action => {
                        if disconnect_after.load(Ordering::SeqCst) > 0 && disconnect_after.fetch_sub(1, Ordering::SeqCst) == 1 {
                            destination.handle.close_link(link, CloseReason::DestinationClosed, true).await?;
                            continue;
                        }
                        let room = message.room.as_ref().context("MSG without room")?;
                        ensure!(member.rooms.contains(room), "client sent to unjoined room");
                        message.nickname = Some(member.nickname.clone());
                        let encoded = message.encode()?;
                        // Deliberately include the sender: production must filter its own echo.
                        for (target, target_member) in &members {
                            if target_member.rooms.contains(room) {
                                destination.handle.send_link_packet(*target, encoded.clone()).await?;
                            }
                        }
                    }
                    MessageType::Part => {
                        if let Some(room) = message.room { member.rooms.remove(&room); }
                    }
                    _ => {}
                }
            }
            closed = destination.events.links_closed.recv() => {
                let Some(link) = closed else { break; };
                members.remove(&link);
            }
        }
    }
    Ok(())
}

async fn adapter(
    name: &str,
    network: Arc<Network>,
    hub: &str,
    path: &Path,
    rooms: &[&str],
) -> Result<Arc<Rrc>> {
    Ok(Arc::new(
        Rrc::connect(
            name.into(),
            network,
            hub,
            path.to_str().unwrap(),
            name.into(),
            rooms.iter().map(|r| (*r).into()).collect(),
        )
        .await?,
    ))
}

async fn incoming(rx: &mut mpsc::Receiver<Incoming>) -> Incoming {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_rrc_links_rooms_echoes_and_reconnect() -> Result<()> {
    let root = tempfile::tempdir()?;
    let directory = root.path().to_str();
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let hub_network = Network::start_service(
        "rrc-hub",
        directory,
        &[ReticulumInterface::TcpServer {
            listen: "127.0.0.1".into(),
            port,
        }],
    )
    .await?;
    let client_network = Network::start_service(
        "rrc-client",
        directory,
        &[ReticulumInterface::TcpClient {
            host: "127.0.0.1".into(),
            port,
        }],
    )
    .await?;
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        let hub_identity = Identity::new();
        let destination = hub_network
            .handle
            .register_destination(
                hub_identity.clone(),
                "rrc.hub",
                DestinationRuntimeOptions::default(),
            )
            .await?;
        let destination_handle = destination.handle.clone();
        let hub = hex::encode(destination_handle.destination_hash());
        let disconnect_after = Arc::new(AtomicUsize::new(0));
        let hub_runner = tokio::spawn(hub_task(
            destination,
            hub_identity.hash,
            disconnect_after.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;
        destination_handle
            .announce(DestinationAnnounceOptions::default())
            .await?;

        let alice = adapter(
            "alice",
            client_network.clone(),
            &hub,
            &root.path().join("alice"),
            &["#test"],
        )
        .await?;
        let bob = adapter(
            "bob",
            client_network.clone(),
            &hub,
            &root.path().join("bob"),
            &["#test"],
        )
        .await?;
        let bridge = adapter(
            "bridge",
            client_network.clone(),
            &hub,
            &root.path().join("bridge"),
            &["#test", "#other"],
        )
        .await?;
        let dave = adapter(
            "dave",
            client_network.clone(),
            &hub,
            &root.path().join("dave"),
            &["#other"],
        )
        .await?;
        let (alice_tx, mut alice_rx) = mpsc::channel(256);
        let (bob_tx, mut bob_rx) = mpsc::channel(256);
        let (bridge_tx, mut bridge_rx) = mpsc::channel(256);
        let (dave_tx, mut dave_rx) = mpsc::channel(256);
        let mut readers = Vec::new();
        for (adapter, tx) in [
            (alice.clone(), alice_tx),
            (bob.clone(), bob_tx),
            (bridge.clone(), bridge_tx),
            (dave.clone(), dave_tx),
        ] {
            readers.push(tokio::spawn(async move { adapter.receive(tx).await }));
        }
        let text = "Hallo 🌍 äöü! ".repeat(90);
        alice.send("#test", &text).await?;
        let mut received = String::new();
        while received.len() < text.len() {
            let message = incoming(&mut bob_rx).await;
            let identity = hex::encode(alice.identity.hash);
            assert_eq!(
                message.username,
                format!(
                    "alice [{}..{}]",
                    &identity[..4],
                    &identity[identity.len() - 2..]
                )
            );
            assert_eq!(message.origin.room.as_deref(), Some("#test"));
            received.push_str(&message.text);
            let bridged = incoming(&mut bridge_rx).await;
            assert_eq!(bridged.text, message.text);
        }
        assert_eq!(received, text);
        assert!(alice.send("#unknown", "bad").await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(200), alice_rx.recv())
                .await
                .is_err(),
            "own echo or notice leaked"
        );

        // Exercise the same formatting path as the shared router in both directions.
        alice.send("#test", "to bridge").await?;
        let source = incoming(&mut bridge_rx).await;
        bridge.send("#other", &source.formatted()).await?;
        assert_eq!(
            incoming(&mut dave_rx).await.text,
            format!(
                "alice [{}..{}] wrote via RRC:\n------\nto bridge",
                &hex::encode(alice.identity.hash)[..4],
                &hex::encode(alice.identity.hash)[30..]
            )
        );
        incoming(&mut bob_rx).await;
        dave.send("#other", "return").await?;
        let source = incoming(&mut bridge_rx).await;
        bridge.send("#test", &source.formatted()).await?;
        assert_eq!(
            incoming(&mut alice_rx).await.text,
            format!(
                "dave [{}..{}] wrote via RRC:\n------\nreturn",
                &hex::encode(dave.identity.hash)[..4],
                &hex::encode(dave.identity.hash)[30..]
            )
        );
        incoming(&mut bob_rx).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), bridge_rx.recv())
                .await
                .is_err(),
            "bridge loop"
        );

        // Force a Link break and wait for a new Link with both rooms rejoined.
        let previous = bridge.ready.read().await.as_ref().unwrap().handle.link_id();
        destination_handle
            .close_link(previous, CloseReason::DestinationClosed, true)
            .await?;
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let ready = bridge.ready.read().await.clone();
                if ready.is_some_and(|r| r.handle.link_id() != previous && r.joined.len() == 2) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("RRC did not reconnect and rejoin")?;
        assert_eq!(
            Identity::from_file(&root.path().join("bridge"))?.hash,
            bridge.identity.hash
        );
        alice.send("#test", "after reconnect").await?;
        assert_eq!(incoming(&mut bridge_rx).await.text, "after reconnect");
        incoming(&mut bob_rx).await;

        // Drop the second part before forwarding it, then reconnect and retry the
        // same operation. The already-confirmed first part must not be repeated.
        disconnect_after.store(2, Ordering::SeqCst);
        let retry_text = "retry after disconnect 🌍 ".repeat(70);
        let mut delivery = Delivery::new(retry_text.clone());
        let target = Endpoint {
            lxmf_group: false,
            message_thread_id: None,
            room: Some("#other".into()),
            service: "bridge".into(),
            chat: String::new(),
        };
        assert!(
            crate::retry::forward(
                bridge.as_ref(),
                &target,
                &mut delivery,
                tokio::time::Instant::now(),
                &crate::config::RetryConfig::default()
            )
            .await
        );
        let mut received = String::new();
        while received.len() < retry_text.len() {
            received.push_str(&incoming(&mut dave_rx).await.text);
        }
        assert_eq!(
            received, retry_text,
            "confirmed parts were duplicated or a failed part was lost"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), dave_rx.recv())
                .await
                .is_err()
        );
        assert!(
            bridge.echoes.lock().unwrap().is_empty(),
            "confirmation registry leaked"
        );

        for reader in &readers {
            reader.abort();
        }
        for reader in readers {
            let _ = reader.await;
        }
        for adapter in [alice, bob, bridge, dave] {
            adapter.shutdown().await?;
        }
        hub_runner.abort();
        let _ = hub_runner.await;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("native RRC integration test timed out")
    .and_then(|r| r);
    client_network.handle.shutdown_and_wait().await;
    hub_network.handle.shutdown_and_wait().await;
    result
}
