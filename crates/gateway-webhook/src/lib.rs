//! Outgoing HTTPS delivery of signed merchant events.
//!
//! The sender does one thing: it posts one event once and reports exactly what
//! happened. Retries, backoff and dead-lettering belong to the outbox, so a
//! hidden retry inside a client cannot turn one obligation into two.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use gateway_application::{DeliveryResult, WebhookEndpoint, WebhookSender};
use reqwest::{Client, StatusCode, header};
use uuid::Uuid;

/// How much of a rejecting endpoint's answer is kept for the operator.
const MAX_DETAIL_CHARS: usize = 500;

/// Posts signed events over HTTPS.
#[derive(Debug, Clone)]
pub struct HttpWebhookSender {
    client: Client,
}

impl HttpWebhookSender {
    /// Builds a sender with explicit timeouts and no automatic redirects.
    ///
    /// A redirect would send a signed merchant event to an address the
    /// merchant never registered, so it is refused rather than followed.
    ///
    /// # Errors
    ///
    /// Returns [`SenderError`] when the HTTP client cannot be built, which
    /// means the TLS stack is unusable and delivery must not be attempted.
    pub fn new(timeout: Duration, user_agent: &str) -> Result<Self, SenderError> {
        let client = Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .user_agent(user_agent.to_owned())
            .build()
            .map_err(|error| SenderError::Unusable(error.to_string()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl WebhookSender for HttpWebhookSender {
    async fn deliver(
        &self,
        endpoint: &WebhookEndpoint,
        event_id: Uuid,
        body: &[u8],
        signature: &str,
    ) -> DeliveryResult {
        let started = Instant::now();
        let response = self
            .client
            .post(&endpoint.url)
            .header(header::CONTENT_TYPE, "application/json")
            .header("gateway-signature", signature)
            .header("gateway-event-id", event_id.to_string())
            .body(body.to_vec())
            .send()
            .await;

        let elapsed = elapsed_ms(started);
        match response {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    DeliveryResult::Accepted {
                        status: status.as_u16(),
                        duration_ms: elapsed,
                    }
                } else {
                    DeliveryResult::Refused {
                        status: status.as_u16(),
                        duration_ms: elapsed_ms(started),
                        detail: detail(status, response.text().await.ok()),
                    }
                }
            }
            Err(error) => DeliveryResult::Unreachable {
                detail: truncate(&error.to_string()),
                duration_ms: elapsed,
            },
        }
    }
}

fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

fn detail(status: StatusCode, body: Option<String>) -> String {
    match body {
        Some(body) if !body.trim().is_empty() => truncate(&format!("{status}: {body}")),
        _ => status.to_string(),
    }
}

fn truncate(value: &str) -> String {
    value.chars().take(MAX_DETAIL_CHARS).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum SenderError {
    #[error("the HTTP client could not be built: {0}")]
    Unusable(String),
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gateway_application::{DeliveryResult, WebhookEndpoint, WebhookSender};
    use uuid::Uuid;

    use super::HttpWebhookSender;

    fn endpoint(url: &str) -> WebhookEndpoint {
        WebhookEndpoint {
            id: Uuid::from_u128(1),
            merchant_id: Uuid::from_u128(2),
            url: url.to_owned(),
            secret_version: 1,
            secret_fingerprint: [0_u8; 32],
        }
    }

    #[tokio::test]
    async fn a_plain_http_endpoint_is_never_called() -> Result<(), Box<dyn std::error::Error>> {
        let sender = HttpWebhookSender::new(Duration::from_secs(5), "gateway-test")?;

        let result = sender
            .deliver(
                &endpoint("http://merchant.example/hooks"),
                Uuid::from_u128(3),
                b"{}",
                "t=1,v1=deadbeef",
            )
            .await;

        // The signed event never leaves over an unencrypted connection.
        assert!(matches!(result, DeliveryResult::Unreachable { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_is_reported_not_swallowed()
    -> Result<(), Box<dyn std::error::Error>> {
        let sender = HttpWebhookSender::new(Duration::from_secs(2), "gateway-test")?;

        let result = sender
            .deliver(
                // Reserved by RFC 6761 to never resolve.
                &endpoint("https://nonexistent.invalid/hooks"),
                Uuid::from_u128(3),
                b"{}",
                "t=1,v1=deadbeef",
            )
            .await;

        let DeliveryResult::Unreachable { detail, .. } = result else {
            return Err("an unreachable endpoint must not look like a success".into());
        };
        assert!(!detail.is_empty());
        Ok(())
    }
}
