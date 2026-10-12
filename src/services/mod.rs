pub mod lxmf;
pub mod reticulum;
pub mod rrc;
pub mod telegram;
use crate::config::Endpoint;
use anyhow::Result;
use async_trait::async_trait;
use std::{fmt, time::Duration};
use tokio::sync::mpsc;

/// State of one forwarding operation, retained only until success or retry expiry.
pub struct Delivery {
    pub text: String,
    pub confirmed_parts: usize,
    pub transport_id: Option<String>,
    pub frames: Option<Vec<Vec<u8>>>,
    pub message_thread_id: Option<i32>,
    pub telegram_thread: Option<Option<i32>>,
}
impl Delivery {
    pub fn new(text: String) -> Self {
        Self {
            text,
            confirmed_parts: 0,
            transport_id: None,
            frames: None,
            message_thread_id: None,
            telegram_thread: None,
        }
    }
}

#[derive(Debug)]
pub enum SendFailure {
    Permanent(&'static str),
    RetryAfter(Duration),
}
impl fmt::Display for SendFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Permanent(reason) => f.write_str(reason),
            Self::RetryAfter(delay) => write!(
                f,
                "service rate limit; retry after {} seconds",
                delay.as_secs()
            ),
        }
    }
}
impl std::error::Error for SendFailure {}

#[derive(Clone)]
pub struct Incoming {
    pub origin: Endpoint,
    pub id: String,
    pub username: String,
    pub service_label: &'static str,
    pub text: String,
}

impl Incoming {
    pub fn for_route(&self, configured_source: &Endpoint) -> Self {
        let mut incoming = self.clone();
        incoming.origin = configured_source.clone();
        if incoming.service_label == "LXMF" && configured_source.lxmf_group {
            incoming.username.clear();
        }
        incoming
    }
    pub fn formatted(&self) -> String {
        if self.service_label == "LXMF" && self.origin.lxmf_group {
            return self.text.clone();
        }
        let username: String = self.username.chars().filter(|c| !c.is_control()).collect();
        format!(
            "{username} wrote via {}:\n------\n{}",
            self.service_label, self.text
        )
    }
}
#[async_trait]
pub trait Adapter: Send + Sync {
    async fn receive(&self, tx: mpsc::Sender<Incoming>) -> Result<()>;
    async fn send(&self, chat: &str, text: &str) -> Result<()>;
    async fn send_delivery(&self, chat: &str, delivery: &mut Delivery) -> Result<()> {
        self.send(chat, &delivery.text).await
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
