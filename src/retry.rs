use crate::{
    config::{Config, Endpoint, RetryConfig},
    services::{Adapter, Delivery, SendFailure},
};
use anyhow::{Context, Result};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    task::JoinSet,
    time::Instant,
};

const MAX_PENDING: usize = 4096;
const MAX_PENDING_BYTES: usize = 16 * 1024 * 1024;

struct Job {
    delivery: Delivery,
    created: Instant,
    _permit: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

pub struct Forwarder {
    queues: HashMap<Endpoint, mpsc::Sender<Job>>,
    permits: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

impl Forwarder {
    pub fn pending_count(&self) -> usize {
        MAX_PENDING - self.permits.available_permits()
    }
    pub fn start(
        config: &Config,
        adapters: &HashMap<String, Arc<dyn Adapter>>,
        tasks: &mut JoinSet<Result<()>>,
    ) -> Self {
        let mut endpoints = HashSet::new();
        for bridge in config.active_bridges() {
            endpoints.insert(bridge.to.clone());
            if bridge.bidirectional {
                endpoints.insert(bridge.from.clone());
            }
        }
        let mut queues = HashMap::new();
        for endpoint in endpoints {
            let (tx, mut rx) = mpsc::channel::<Job>(config.retry.queue_capacity);
            let adapter = adapters[&endpoint.service].clone();
            let destination = endpoint.clone();
            let policy = config.retry.clone();
            tasks.spawn(async move {
                while let Some(mut job) = rx.recv().await {
                    forward(
                        adapter.as_ref(),
                        &destination,
                        &mut job.delivery,
                        job.created,
                        &policy,
                    )
                    .await;
                }
                Ok(())
            });
            queues.insert(endpoint, tx);
        }
        Self {
            queues,
            permits: Arc::new(Semaphore::new(MAX_PENDING)),
            bytes: Arc::new(Semaphore::new(MAX_PENDING_BYTES)),
        }
    }

    pub fn enqueue(&self, destination: &Endpoint, text: &str) -> Result<()> {
        let queue = self
            .queues
            .get(destination)
            .context("forwarding destination not registered")?;
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .context("forwarding limit of 4096 pending operations reached")?;
        let size = u32::try_from(text.len().max(1)).context("forwarding message too large")?;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(size)
            .context("forwarding pending-text limit of 16 MiB reached")?;
        let mut delivery = Delivery::new(text.into());
        delivery.message_thread_id = destination.message_thread_id;
        let job = Job {
            delivery,
            created: Instant::now(),
            _permit: permit,
            _bytes: bytes,
        };
        queue.try_send(job).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                anyhow::anyhow!("forwarding destination queue is full")
            }
            mpsc::error::TrySendError::Closed(_) => {
                anyhow::anyhow!("forwarding destination worker stopped")
            }
        })
    }
}

fn backoff(policy: &RetryConfig, failed_attempt: u32) -> Duration {
    let base = policy
        .initial_delay_secs
        .saturating_mul(1u64 << failed_attempt.saturating_sub(1).min(32))
        .min(policy.max_delay_secs);
    // Positive jitter avoids synchronized retry bursts; still honor the configured cap.
    let jitter = f64::from(rand::random::<u16>()) / f64::from(u16::MAX) * 0.2;
    Duration::from_secs_f64((base as f64 * (1.0 + jitter)).min(policy.max_delay_secs as f64))
}

/// One FIFO worker per destination: failures delay that chat, never other chats.
pub(crate) async fn forward(
    adapter: &dyn Adapter,
    destination: &Endpoint,
    delivery: &mut Delivery,
    created: Instant,
    policy: &RetryConfig,
) -> bool {
    let deadline = created + Duration::from_secs(policy.max_age_secs);
    for attempt in 1..=policy.max_attempts {
        if Instant::now() >= deadline {
            tracing::error!(service = %destination.service, chat = %destination.chat, room = ?destination.room, message_thread_id = ?destination.message_thread_id, "Forwarding expired in memory");
            return false;
        }
        let attempt_deadline =
            deadline.min(Instant::now() + Duration::from_secs(policy.attempt_timeout_secs));
        let result = match tokio::time::timeout_at(
            attempt_deadline,
            adapter.send_delivery(destination.address(), delivery),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "send/confirmation timed out; delivery is uncertain"
            )),
        };
        let error = match result {
            Ok(()) => {
                tracing::debug!(service = %destination.service, chat = %destination.chat, room = ?destination.room, message_thread_id = ?destination.message_thread_id, attempt, "Forwarding confirmed");
                return true;
            }
            Err(error) => error,
        };
        if matches!(
            error.downcast_ref::<SendFailure>(),
            Some(SendFailure::Permanent(_))
        ) || attempt == policy.max_attempts
        {
            tracing::error!(service = %destination.service, chat = %destination.chat, room = ?destination.room, message_thread_id = ?destination.message_thread_id, attempt, %error, "Forwarding abandoned");
            return false;
        }
        let mut delay = backoff(policy, attempt);
        if let Some(SendFailure::RetryAfter(minimum)) = error.downcast_ref::<SendFailure>() {
            delay = delay.max(*minimum);
        }
        if Instant::now() + delay >= deadline {
            tracing::error!(service = %destination.service, chat = %destination.chat, room = ?destination.room, message_thread_id = ?destination.message_thread_id, attempt, %error, "Forwarding retry would exceed expiry; abandoned");
            return false;
        }
        tracing::warn!(service = %destination.service, chat = %destination.chat, room = ?destination.room, message_thread_id = ?destination.message_thread_id, attempt, delay_ms = delay.as_millis() as u64, %error, "Forwarding failed; retry scheduled");
        tokio::time::sleep(delay).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::Incoming;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    struct Flaky {
        calls: AtomicUsize,
        fail: usize,
        permanent: bool,
        delay: Option<Duration>,
        seen: Mutex<Vec<Instant>>,
        texts: Mutex<Vec<String>>,
    }
    impl Flaky {
        fn new(fail: usize) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail,
                permanent: false,
                delay: None,
                seen: Mutex::new(Vec::new()),
                texts: Mutex::new(Vec::new()),
            }
        }
    }
    #[async_trait]
    impl Adapter for Flaky {
        async fn receive(&self, _: mpsc::Sender<Incoming>) -> Result<()> {
            Ok(())
        }
        async fn send(&self, _: &str, text: &str) -> Result<()> {
            self.seen.lock().await.push(Instant::now());
            self.texts.lock().await.push(text.into());
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.permanent {
                return Err(SendFailure::Permanent("forbidden").into());
            }
            if let Some(delay) = self.delay {
                return Err(SendFailure::RetryAfter(delay).into());
            }
            if attempt < self.fail {
                anyhow::bail!("temporary failure");
            }
            Ok(())
        }
    }
    fn destination() -> Endpoint {
        Endpoint {
            message_thread_id: None,
            room: None,
            service: "mock".into(),
            chat: "chat".into(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn recovers_after_transient_errors_with_backoff() {
        let adapter = Flaky::new(2);
        let now = Instant::now();
        assert!(
            forward(
                &adapter,
                &destination(),
                &mut Delivery::new("hello".into()),
                now,
                &RetryConfig::default()
            )
            .await
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 3);
        let seen = adapter.seen.lock().await;
        assert!(seen[1] - seen[0] >= Duration::from_secs(2));
        assert!(seen[2] - seen[1] >= Duration::from_secs(4));
    }
    #[tokio::test(start_paused = true)]
    async fn stops_after_attempt_limit_and_does_not_retry_permanent_errors() {
        let adapter = Flaky::new(usize::MAX);
        assert!(
            !forward(
                &adapter,
                &destination(),
                &mut Delivery::new("hello".into()),
                Instant::now(),
                &RetryConfig::default()
            )
            .await
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 6);
        let mut adapter = Flaky::new(0);
        adapter.permanent = true;
        assert!(
            !forward(
                &adapter,
                &destination(),
                &mut Delivery::new("hello".into()),
                Instant::now(),
                &RetryConfig::default()
            )
            .await
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn honors_service_retry_after_and_expires_queued_messages() {
        let mut adapter = Flaky::new(0);
        adapter.delay = Some(Duration::from_secs(25));
        let policy = RetryConfig {
            max_attempts: 2,
            ..Default::default()
        };
        assert!(
            !forward(
                &adapter,
                &destination(),
                &mut Delivery::new("hello".into()),
                Instant::now(),
                &policy
            )
            .await
        );
        let seen = adapter.seen.lock().await;
        assert!(seen[1] - seen[0] >= Duration::from_secs(25));
        let adapter = Flaky::new(0);
        let created = Instant::now() - Duration::from_secs(601);
        assert!(
            !forward(
                &adapter,
                &destination(),
                &mut Delivery::new("expired".into()),
                created,
                &policy
            )
            .await
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test(start_paused = true)]
    async fn another_destination_progresses_while_one_is_retrying() {
        let slow = Flaky::new(2);
        let fast = Flaky::new(0);
        let mut first = Delivery::new("slow".into());
        let mut second = Delivery::new("fast".into());
        let policy = RetryConfig::default();
        let now = Instant::now();
        let dest = destination();
        let (a, b) = tokio::join!(
            forward(&slow, &dest, &mut first, now, &policy),
            forward(&fast, &dest, &mut second, now, &policy)
        );
        assert!(a && b);
        assert_eq!(fast.seen.lock().await[0], now);
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_queue_preserves_fifo_and_releases_memory_permits() {
        let mut config: Config = toml::from_str(
            r#"
            [services.mock]
            type = "telegram"
            token = "TOKEN"
            [[bridges]]
            from = { service = "mock", chat = "99" }
            to = { service = "mock", chat = "1" }
        "#,
        )
        .unwrap();
        config.retry.queue_capacity = 2;
        let adapter = Arc::new(Flaky::new(2));
        let adapters: HashMap<String, Arc<dyn Adapter>> =
            HashMap::from([("mock".into(), adapter.clone() as Arc<dyn Adapter>)]);
        let mut tasks = JoinSet::new();
        let forwarder = Forwarder::start(&config, &adapters, &mut tasks);
        let target = config.bridges[0].to.clone();
        forwarder.enqueue(&target, "first").unwrap();
        forwarder.enqueue(&target, "second").unwrap();
        assert!(forwarder.enqueue(&target, "overflow").is_err());
        assert_eq!(forwarder.permits.available_permits(), MAX_PENDING - 2);
        tokio::time::timeout(Duration::from_secs(20), async {
            while adapter.calls.load(Ordering::SeqCst) < 4 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            *adapter.texts.lock().await,
            ["first", "first", "first", "second"]
        );
        assert_eq!(forwarder.permits.available_permits(), MAX_PENDING);
        assert_eq!(forwarder.bytes.available_permits(), MAX_PENDING_BYTES);
        tasks.shutdown().await;
    }

    struct Hanging;
    #[async_trait]
    impl Adapter for Hanging {
        async fn receive(&self, _: mpsc::Sender<Incoming>) -> Result<()> {
            Ok(())
        }
        async fn send(&self, _: &str, _: &str) -> Result<()> {
            std::future::pending().await
        }
    }
    #[tokio::test(start_paused = true)]
    async fn stuck_sends_have_a_deadline() {
        let policy = RetryConfig {
            max_attempts: 1,
            attempt_timeout_secs: 5,
            ..Default::default()
        };
        let now = Instant::now();
        assert!(
            !forward(
                &Hanging,
                &destination(),
                &mut Delivery::new("hello".into()),
                now,
                &policy
            )
            .await
        );
        assert_eq!(Instant::now() - now, Duration::from_secs(5));
    }
}
