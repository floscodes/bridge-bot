use super::*;
use crate::config::Endpoint;
use anyhow::Context;
use std::collections::{HashSet, VecDeque};
use teloxide::{
    RequestError,
    prelude::*,
    types::{AllowedUpdate, UpdateKind},
};
use tokio::sync::Mutex;
#[cfg(test)]
mod integration_tests;

type SentCache = (HashSet<(i64, i32)>, VecDeque<(i64, i32)>);

pub struct Telegram {
    pub name: String,
    pub bot: Bot,
    pub own_id: teloxide::types::UserId,
    pub sent: Mutex<SentCache>,
}
#[async_trait]
impl Adapter for Telegram {
    async fn receive(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        let mut offset = 0;
        loop {
            let updates = match self
                .bot
                .get_updates()
                .offset(offset)
                // teloxide's HTTP client times out after 17s; its timeout hint is
                // currently ignored. Keep long polling below that deadline.
                .timeout(10)
                .allowed_updates(vec![AllowedUpdate::Message, AllowedUpdate::ChannelPost])
                .await
            {
                Ok(u) => u,
                Err(_) => {
                    tracing::warn!(service = %self.name, "Telegram polling failed; retrying in 5 seconds");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };
            for update in updates {
                offset = i32::try_from(update.id.0)?
                    .checked_add(1)
                    .context("Telegram update ID overflow")?;
                let message = match update.kind {
                    UpdateKind::Message(m) | UpdateKind::ChannelPost(m) => m,
                    _ => continue,
                };
                if message.from.as_ref().is_some_and(|u| u.id == self.own_id)
                    || self
                        .sent
                        .lock()
                        .await
                        .0
                        .contains(&(message.chat.id.0, message.id.0))
                {
                    continue;
                }
                let Some(text) = message.text().or_else(|| message.caption()) else {
                    continue;
                };
                let username = message
                    .sender_chat
                    .as_ref()
                    .and_then(|c| c.title().map(str::to_owned))
                    .or_else(|| {
                        message
                            .from
                            .as_ref()
                            .map(|u| u.username.clone().unwrap_or_else(|| u.full_name()))
                    })
                    .unwrap_or_else(|| message.chat.id.to_string());
                tx.send(Incoming {
                    origin: source_endpoint(&self.name, &message),
                    id: message.id.to_string(),
                    username,
                    service_label: "Telegram",
                    text: text.into(),
                })
                .await?;
            }
        }
    }
    async fn send(&self, chat: &str, text: &str) -> Result<()> {
        self.send_delivery(chat, &mut Delivery::new(text.into()))
            .await
    }
    async fn send_delivery(&self, chat: &str, delivery: &mut Delivery) -> Result<()> {
        let chat_id = chat
            .parse()
            .map_err(|_| SendFailure::Permanent("invalid Telegram chat ID"))?;
        if delivery.telegram_thread.is_none() {
            let info = self
                .bot
                .get_chat(ChatId(chat_id))
                .await
                .map_err(send_error)?;
            let forum = matches!(&info.kind, teloxide::types::ChatFullInfoKind::Public(chat)
                if matches!(&chat.kind, teloxide::types::ChatFullInfoPublicKind::Supergroup(group) if group.is_forum));
            let thread = match (forum, delivery.message_thread_id) {
                (true, Some(id)) if id > 0 => Some(id),
                (true, _) => return Err(SendFailure::Permanent(
                    "Telegram forum requires an explicit room (message_thread_id) in the route; message not sent",
                ).into()),
                (false, Some(_)) => return Err(SendFailure::Permanent(
                    "Telegram route specifies a room but the destination group has no forum topics; message not sent",
                ).into()),
                (false, None) => None,
            };
            delivery.telegram_thread = Some(thread);
        }
        let parts = chunks(&delivery.text);
        for chunk in parts.iter().skip(delivery.confirmed_parts) {
            let mut request = self.bot.send_message(ChatId(chat_id), chunk.clone());
            if let Some(Some(thread)) = delivery.telegram_thread {
                // Omitting the thread selects General; its synthetic ID is 1.
                if thread != 1 {
                    request = request.message_thread_id(teloxide::types::ThreadId(
                        teloxide::types::MessageId(thread),
                    ));
                }
            }
            let message = match request.await {
                Ok(message) => message,
                Err(error) => {
                    if topic_unavailable(&error) {
                        return Err(SendFailure::Permanent(
                            "configured Telegram room is closed or unavailable; message not redirected",
                        ).into());
                    }
                    return Err(send_error(error));
                }
            };
            delivery.confirmed_parts += 1;
            let mut sent = self.sent.lock().await;
            let key = (message.chat.id.0, message.id.0);
            sent.0.insert(key);
            sent.1.push_back(key);
            if sent.1.len() > 4096
                && let Some(old) = sent.1.pop_front()
            {
                sent.0.remove(&old);
            }
        }
        Ok(())
    }
}

fn source_endpoint(service: &str, message: &Message) -> Endpoint {
    let forum = matches!(&message.chat.kind, teloxide::types::ChatKind::Public(chat)
        if matches!(&chat.kind, teloxide::types::PublicChatKind::Supergroup(group) if group.is_forum));
    Endpoint {
        service: service.into(),
        chat: message.chat.id.to_string(),
        room: None,
        message_thread_id: if forum {
            Some(if message.is_topic_message {
                message.thread_id.map(|id| id.0.0).unwrap_or(1)
            } else {
                1
            })
        } else {
            None
        },
    }
}

fn topic_unavailable(error: &RequestError) -> bool {
    matches!(error, RequestError::Api(teloxide::ApiError::Unknown(reason))
        if reason.to_lowercase().contains("topic_closed") || reason.to_lowercase().contains("message thread not found"))
}

fn send_error(error: RequestError) -> anyhow::Error {
    // Do not expose reqwest URLs: Telegram embeds the bot token in the URL.
    match error {
        RequestError::RetryAfter(delay) => SendFailure::RetryAfter(delay.duration()).into(),
        RequestError::Api(teloxide::ApiError::Unknown(_)) => {
            anyhow::anyhow!("Telegram API send failed")
        }
        RequestError::Api(_) | RequestError::MigrateToChatId(_) => SendFailure::Permanent(
            "Telegram rejected the chat, permissions, or message; check configuration",
        )
        .into(),
        _ => anyhow::anyhow!("Telegram network/response send failed"),
    }
}
fn chunks(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > 4096 {
            result.push(std::mem::take(&mut chunk));
            units = 0;
        }
        chunk.push(c);
        units += c.len_utf16();
    }
    if !chunk.is_empty() {
        result.push(chunk);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::{chunks, send_error};
    use crate::services::SendFailure;
    use teloxide::{ApiError, RequestError, types::Seconds};
    #[test]
    fn distinguishes_forum_rooms_general_and_ordinary_reply_threads() {
        let make = |forum, topic, thread| {
            serde_json::from_value::<teloxide::types::Message>(serde_json::json!({
                "message_id": 99, "date": 1700000000,
                "chat": {"id": -100123, "type": "supergroup", "is_forum": forum, "title": "Test"},
                "is_topic_message": topic, "message_thread_id": thread, "text": "hello"
            }))
            .unwrap()
        };
        assert_eq!(
            super::source_endpoint("telegram", &make(true, true, 42)).message_thread_id,
            Some(42)
        );
        assert_eq!(
            super::source_endpoint("telegram", &make(true, false, 99)).message_thread_id,
            Some(1)
        );
        assert_eq!(
            super::source_endpoint("telegram", &make(false, false, 99)).message_thread_id,
            None
        );
    }
    #[test]
    fn splits_utf16_without_losing_unicode() {
        let text = format!("{}{}", "a".repeat(4095), "🦀".repeat(3000));
        let parts = chunks(&text);
        assert_eq!(parts.concat(), text);
        assert!(parts.iter().all(|p| p.encode_utf16().count() <= 4096));
        assert!(chunks("").is_empty());
    }
    #[test]
    fn distinguishes_rate_limits_permissions_and_transient_api_failures() {
        let error = send_error(RequestError::RetryAfter(Seconds::from_seconds(45)));
        assert!(
            matches!(error.downcast_ref::<SendFailure>(), Some(SendFailure::RetryAfter(delay)) if delay.as_secs() == 45)
        );
        let error = send_error(RequestError::Api(ApiError::BotKicked));
        assert!(matches!(
            error.downcast_ref::<SendFailure>(),
            Some(SendFailure::Permanent(_))
        ));
        let error = send_error(RequestError::Api(ApiError::Unknown("server error".into())));
        assert!(error.downcast_ref::<SendFailure>().is_none());
    }
}
