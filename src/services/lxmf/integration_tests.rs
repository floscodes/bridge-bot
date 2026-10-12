use super::*;
use crate::config::ReticulumInterface;

#[tokio::test]
async fn native_lxmf_stamp_preserves_signature_and_message_id() -> Result<()> {
    let identity = Identity::new();
    let mut message = WireMessage::new(
        [1; 16],
        [2; 16],
        Payload::new(
            1234.5,
            Some(b"hello".to_vec()),
            Some(Vec::new()),
            Some(rmpv::Value::Map(Vec::new())),
            None,
        ),
    );
    let signed = signing_data(&message)?;
    message.signature = identity.sign(&signed);
    let id = message.try_message_id()?;
    let mut frame = message.pack()?;
    add_stamp(&mut frame, 1).await?;
    let stamped = WireMessage::unpack(&frame)?;
    assert_eq!(stamped.try_message_id()?, id);
    assert!(identity.verify(&signing_data(&stamped)?, &stamped.signature.unwrap()));
    assert!(
        lxmf_core::stamp::validate_stamp(
            stamped.payload.stamp.as_deref().map(|s| s.as_ref()),
            &id,
            1,
            &[]
        )
        .is_some()
    );
    assert!(
        add_stamp(&mut frame, 256)
            .await
            .unwrap_err()
            .downcast_ref::<SendFailure>()
            .is_some()
    );
    Ok(())
}

async fn next(rx: &mut mpsc::Receiver<Incoming>) -> Result<Incoming> {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await?
        .context("receiver stopped")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_lxmf_independent_interfaces_signed_resources_and_retry_identity() -> Result<()> {
    let root = tempfile::tempdir()?;
    let directory = root.path().to_str().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let server = Network::start_service(
        "lxmf-server",
        Some(directory),
        &[ReticulumInterface::TcpServer {
            listen: "127.0.0.1".into(),
            port,
        }],
    )
    .await?;
    let client = Network::start_service(
        "lxmf-client",
        Some(directory),
        &[ReticulumInterface::TcpClient {
            host: "127.0.0.1".into(),
            port,
        }],
    )
    .await?;
    let isolated = Network::start_service("rrc-isolated", Some(directory), &[]).await?;
    let result = tokio::time::timeout(Duration::from_secs(50), async {
        let alice_path = root.path().join("alice");
        let bob_path = root.path().join("bob");
        let alice = Arc::new(
            Lxmf::connect(
                "alice".into(),
                client.clone(),
                alice_path.to_str().unwrap(),
                "Alice",
            )
            .await?,
        );
        let bob = Arc::new(
            Lxmf::connect(
                "bob".into(),
                server.clone(),
                bob_path.to_str().unwrap(),
                "Bob",
            )
            .await?,
        );
        let (alice_tx, mut alice_rx) = mpsc::channel(256);
        let (bob_tx, mut bob_rx) = mpsc::channel(256);
        let a = alice.clone();
        let b = bob.clone();
        let alice_reader = tokio::spawn(async move { a.receive(alice_tx).await });
        let bob_reader = tokio::spawn(async move { b.receive(bob_tx).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        let destination = hex::encode(bob.source);
        let mut delivery = Delivery::new("Hallo 🌍 aus dem eingebauten Stack!".repeat(100));
        alice.send_delivery(&destination, &mut delivery).await?;
        let received = next(&mut bob_rx).await?;
        assert_eq!(received.text, delivery.text);
        let source = hex::encode(alice.source);
        assert_eq!(
            received.username,
            format!("Alice [{}..{}]", &source[..4], &source[source.len() - 2..])
        );
        assert!(!received.origin.lxmf_group);
        let mut configured_source = received.origin.clone();
        configured_source.lxmf_group = true;
        let mut group_incoming = received.clone();
        group_incoming.origin = configured_source;
        assert_eq!(group_incoming.username, received.username);
        assert!(group_incoming.origin.lxmf_group);
        assert_eq!(
            group_incoming.formatted(),
            format!("Message via LXMF-Group:\n------\n{}", delivery.text)
        );
        let direct_incoming = received.clone();
        assert_eq!(direct_incoming.username, received.username);
        assert_eq!(direct_incoming.formatted(), received.formatted());
        assert_eq!(received.origin.chat, hex::encode(alice.source));
        assert_eq!(Some(received.id.clone()), delivery.transport_id);
        assert!(isolated.handle.recall(bob.source).await?.is_none());
        let frames = delivery.frames.clone();
        alice.send_delivery(&destination, &mut delivery).await?;
        assert_eq!(next(&mut bob_rx).await?.id, received.id);
        assert_eq!(delivery.frames, frames);
        bob.send(&hex::encode(alice.source), "Antwort").await?;
        let reply = next(&mut alice_rx).await?;
        assert_eq!(reply.text, "Antwort");
        let source = hex::encode(bob.source);
        assert_eq!(
            reply.username,
            format!("Bob [{}..{}]", &source[..4], &source[source.len() - 2..])
        );
        assert!(!reply.origin.lxmf_group);
        assert_eq!(
            reply.formatted(),
            format!("{} wrote via LXMF:\n------\nAntwort", reply.username)
        );
        let mut invalid = frames.unwrap()[0].clone();
        invalid[32] ^= 1;
        assert!(bob.decode(&invalid).await.is_err());
        let stored = load_identity(alice_path.to_str().unwrap())?;
        assert_eq!(
            Destination::new(
                Some(&stored),
                Direction::In,
                DestType::Single,
                "lxmf.delivery"
            )?
            .hash,
            alice.source
        );
        alice_reader.abort();
        bob_reader.abort();
        let _ = alice_reader.await;
        let _ = bob_reader.await;
        alice.shutdown().await?;
        bob.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    })
    .await;
    client.handle.shutdown_and_wait().await;
    server.handle.shutdown_and_wait().await;
    isolated.handle.shutdown_and_wait().await;
    result??;
    Ok(())
}
