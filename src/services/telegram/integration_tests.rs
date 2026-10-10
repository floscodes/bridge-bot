use super::*;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

async fn mock(responses: Vec<Value>) -> (Bot, JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            let (start, length) = loop {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                data.extend_from_slice(&buffer[..count]);
                if let Some(end) = data.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&data[..end]).to_lowercase();
                    let length = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .trim()
                        .parse::<usize>()
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while data.len() < start + length {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                data.extend_from_slice(&buffer[..count]);
            }
            requests.push(serde_json::from_slice(&data[start..start + length]).unwrap());
            let body = response.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (
        Bot::new("TEST_TOKEN").set_api_url(format!("http://{address}/").parse().unwrap()),
        server,
    )
}

fn chat(forum: bool) -> Value {
    json!({"ok": true, "result": {
        "id": -100123, "type": "supergroup", "title": "Test", "is_forum": forum,
        "max_reaction_count": 11,
        "accepted_gift_types": {"unlimited_gifts": true, "limited_gifts": true, "unique_gifts": true, "premium_subscription": true}
    }})
}

fn sent(id: i32) -> Value {
    json!({"ok": true, "result": {"message_id": id, "date": 1700000000, "chat": {"id": -100123, "type": "supergroup", "title": "Test"}, "text": "sent"}})
}

fn adapter(bot: Bot) -> Telegram {
    Telegram {
        name: "telegram".into(),
        bot,
        own_id: teloxide::types::UserId(1),
        sent: Mutex::new(Default::default()),
    }
}

#[tokio::test]
async fn sends_all_parts_in_configured_topic_and_normal_groups_without_thread() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let (bot, server) = mock(vec![chat(true), sent(10), sent(11)]).await;
        let telegram = adapter(bot);
        let mut delivery = Delivery::new("x".repeat(5000));
        delivery.message_thread_id = Some(42);
        telegram
            .send_delivery("-100123", &mut delivery)
            .await
            .unwrap();
        assert_eq!(delivery.confirmed_parts, 2);
        let requests = server.await.unwrap();
        assert_eq!(requests[1]["message_thread_id"], 42);
        assert_eq!(requests[2]["message_thread_id"], 42);
        let (bot, server) = mock(vec![chat(false), sent(12)]).await;
        let telegram = adapter(bot);
        telegram
            .send_delivery("-100123", &mut Delivery::new("hello".into()))
            .await
            .unwrap();
        assert!(server.await.unwrap()[1].get("message_thread_id").is_none());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn retry_keeps_topic_and_skips_confirmed_parts() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let (bot, server) = mock(vec![
            chat(true),
            sent(10),
            json!({"ok": false, "error_code": 500, "description": "Internal Server Error"}),
            sent(11),
        ])
        .await;
        let telegram = adapter(bot);
        let mut delivery = Delivery::new("x".repeat(5000));
        delivery.message_thread_id = Some(42);
        assert!(
            telegram
                .send_delivery("-100123", &mut delivery)
                .await
                .is_err()
        );
        assert_eq!(delivery.confirmed_parts, 1);
        assert_eq!(delivery.telegram_thread, Some(Some(42)));
        telegram
            .send_delivery("-100123", &mut delivery)
            .await
            .unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[2]["text"], requests[3]["text"]);
        assert_eq!(requests[3]["message_thread_id"], 42);
        assert_eq!(delivery.confirmed_parts, 2);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn missing_room_in_forum_and_room_in_plain_group_never_send() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for (forum, room) in [(true, None), (false, Some(42))] {
            let (bot, server) = mock(vec![chat(forum)]).await;
            let telegram = adapter(bot);
            let mut delivery = Delivery::new("hello".into());
            delivery.message_thread_id = room;
            let error = telegram
                .send_delivery("-100123", &mut delivery)
                .await
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<SendFailure>(),
                Some(SendFailure::Permanent(_))
            ));
            assert_eq!(delivery.confirmed_parts, 0);
            assert_eq!(server.await.unwrap().len(), 1);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn rejects_unavailable_topic_without_redirecting() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let (bot, server) = mock(vec![
            chat(true),
            json!({"ok": false, "error_code": 400, "description": "Bad Request: TOPIC_CLOSED"}),
        ])
        .await;
        let telegram = adapter(bot);
        let mut delivery = Delivery::new("hello".into());
        delivery.message_thread_id = Some(42);
        let error = telegram
            .send_delivery("-100123", &mut delivery)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SendFailure>(),
            Some(SendFailure::Permanent(_))
        ));
        assert_eq!(delivery.confirmed_parts, 0);
        assert_eq!(delivery.telegram_thread, Some(Some(42)));
        let requests = server.await.unwrap();
        assert_eq!(requests[1]["message_thread_id"], 42);
        assert_eq!(requests.len(), 2);
    })
    .await
    .unwrap();
}
