//! Outgoing HTTPS delivery of signed merchant events.
//!
//! The sender does one thing: it posts one event once and reports exactly what
//! happened. Retries, backoff and dead-lettering belong to the outbox, so a
//! hidden retry inside a client cannot turn one obligation into two.

use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use gateway_application::{DeliveryResult, WebhookEndpoint, WebhookSender};
use reqwest::{Client, StatusCode, Url, header};
use uuid::Uuid;

/// How much of a rejecting endpoint's answer is kept for the operator.
const MAX_DETAIL_CHARS: usize = 500;
/// No rejecting peer may make the worker buffer an unbounded response.
const MAX_DETAIL_BYTES: usize = 2_048;
const WEBHOOK_PORT: u16 = 443;

/// Posts signed events over HTTPS.
#[derive(Debug, Clone)]
pub struct HttpWebhookSender {
    timeout: Duration,
    user_agent: String,
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
        // Build once so invalid TLS/client configuration fails at startup, not
        // only when the first outbox event is due. Delivery builds another
        // client with the freshly approved DNS answers pinned into it.
        build_client(timeout, user_agent, None)?;
        Ok(Self {
            timeout,
            user_agent: user_agent.to_owned(),
        })
    }

    async fn approved_request(&self, raw_url: &str) -> Result<(Client, Url), SafeDeliveryError> {
        let parsed = validate_url(raw_url)?;
        let host = parsed
            .host_str()
            .ok_or(SafeDeliveryError::InvalidEndpoint)?
            .to_owned();

        let answers = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, WEBHOOK_PORT)]
        } else {
            tokio::net::lookup_host((host.as_str(), WEBHOOK_PORT))
                .await
                .map_err(|_| SafeDeliveryError::ResolutionFailed)?
                .collect()
        };
        let approved = approve_answers(answers)?;
        let client = build_client(self.timeout, &self.user_agent, Some((&host, &approved)))
            .map_err(|_| SafeDeliveryError::ClientUnavailable)?;
        Ok((client, parsed))
    }
}

fn build_client(
    timeout: Duration,
    user_agent: &str,
    pinned: Option<(&str, &[SocketAddr])>,
) -> Result<Client, SenderError> {
    let mut builder = Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout.min(Duration::from_secs(10)))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .user_agent(user_agent.to_owned());
    if let Some((host, addresses)) = pinned {
        // Reqwest still receives the original URL below. Only name resolution
        // is overridden, so TLS SNI and certificate verification continue to
        // use the merchant hostname rather than the selected IP address.
        builder = builder.resolve_to_addrs(host, addresses);
    }
    builder
        .build()
        .map_err(|error| SenderError::Unusable(error.to_string()))
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
        let (client, url) = match self.approved_request(&endpoint.url).await {
            Ok(request) => request,
            Err(error) => {
                return DeliveryResult::Unreachable {
                    detail: error.to_string(),
                    duration_ms: elapsed_ms(started),
                };
            }
        };
        let response = client
            .post(url)
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
                        detail: detail(status, bounded_response_body(response).await),
                    }
                }
            }
            Err(error) => DeliveryResult::Unreachable {
                detail: safe_reqwest_error(&error).to_owned(),
                duration_ms: elapsed,
            },
        }
    }
}

fn validate_url(raw_url: &str) -> Result<Url, SafeDeliveryError> {
    let authority = raw_url
        .strip_prefix("https://")
        .ok_or(SafeDeliveryError::InvalidEndpoint)?;
    if authority.is_empty() || authority.starts_with('/') {
        return Err(SafeDeliveryError::InvalidEndpoint);
    }
    let url = Url::parse(raw_url).map_err(|_| SafeDeliveryError::InvalidEndpoint)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() != Some(WEBHOOK_PORT)
    {
        return Err(SafeDeliveryError::InvalidEndpoint);
    }
    Ok(url)
}

fn approve_answers(answers: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, SafeDeliveryError> {
    let unique: BTreeSet<_> = answers.into_iter().collect();
    if unique.is_empty() {
        return Err(SafeDeliveryError::NoAddresses);
    }
    if unique
        .iter()
        .any(|address| address.port() != WEBHOOK_PORT || !is_public_ip(address.ip()))
    {
        // Reject the whole answer set. Choosing only its public subset would
        // make a mixed/rebinding answer depend on resolver ordering.
        return Err(SafeDeliveryError::NonPublicAddress);
    }
    Ok(unique.into_iter().collect())
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => mapped_ipv4(ip).map_or_else(|| is_public_ipv6(ip), is_public_ipv4),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, d] = ip.octets();
    !matches!(
        (a, b, c, d),
        (0 | 10 | 127 | 224..=255, _, _, _)
            | (100, 64..=127, _, _)
            | (169, 254, _, _)
            | (172, 16..=31, _, _)
            | (192, 0, 0 | 2, _)
            | (192, 88, 99, _)
            | (192, 168, _, _)
            | (198, 18..=19, _, _)
            | (198, 51, 100, _)
            | (203, 0, 113, _)
    )
}

fn mapped_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = ip.segments();
    (segments[..5] == [0, 0, 0, 0, 0] && segments[5] == u16::MAX).then(|| {
        let high = segments[6].to_be_bytes();
        let low = segments[7].to_be_bytes();
        Ipv4Addr::new(high[0], high[1], low[0], low[1])
    })
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    // Public unicast is currently allocated from 2000::/3. Keep this an
    // allow-range so future special allocations fail closed until reviewed.
    if !(0x2000..=0x3fff).contains(&segments[0]) {
        return false;
    }
    // IANA special-purpose and documentation ranges within 2000::/3.
    let iana_special_2001 = segments[0] == 0x2001 && segments[1] <= 0x01ff;
    let documentation_2001 = segments[0] == 0x2001 && segments[1] == 0x0db8;
    let documentation_3fff = segments[0] == 0x3fff && segments[1] <= 0x0fff;
    !iana_special_2001 && !documentation_2001 && !documentation_3fff
}

async fn bounded_response_body(mut response: reqwest::Response) -> Option<String> {
    let mut body = Vec::with_capacity(MAX_DETAIL_BYTES.min(512));
    while body.len() < MAX_DETAIL_BYTES {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => return None,
        };
        if append_bounded(&mut body, &chunk) {
            break;
        }
    }
    Some(String::from_utf8_lossy(&body).into_owned())
}

/// Appends no more than the remaining diagnostic budget and reports when the
/// budget is exhausted. The response reader stops immediately on `true`.
fn append_bounded(body: &mut Vec<u8>, chunk: &[u8]) -> bool {
    let remaining = MAX_DETAIL_BYTES.saturating_sub(body.len());
    body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    body.len() == MAX_DETAIL_BYTES
}

fn safe_reqwest_error(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "webhook request timed out"
    } else if error.is_connect() {
        "webhook endpoint could not be reached"
    } else if error.is_body() {
        "webhook request body failed"
    } else {
        "webhook request failed"
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

#[derive(Debug, thiserror::Error)]
enum SafeDeliveryError {
    #[error("webhook endpoint URL is not allowed")]
    InvalidEndpoint,
    #[error("webhook endpoint hostname could not be resolved")]
    ResolutionFailed,
    #[error("webhook endpoint hostname resolved to no addresses")]
    NoAddresses,
    #[error("webhook endpoint resolved to a non-public address")]
    NonPublicAddress,
    #[error("webhook HTTP client is unavailable")]
    ClientUnavailable,
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        time::Duration,
    };

    use gateway_application::{DeliveryResult, WebhookEndpoint, WebhookSender};
    use uuid::Uuid;

    use super::{
        HttpWebhookSender, MAX_DETAIL_BYTES, append_bounded, approve_answers, is_public_ip,
        validate_url,
    };

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

    #[test]
    fn endpoint_url_policy_is_strict() {
        assert!(validate_url("https://merchant.example/hooks").is_ok());
        for forbidden in [
            "http://merchant.example/hooks",
            "https://user:secret@merchant.example/hooks",
            "https://merchant.example:8443/hooks",
            "https://merchant.example/hooks?token=secret",
            "https://merchant.example/hooks#fragment",
            "https:///hooks",
        ] {
            assert!(validate_url(forbidden).is_err(), "accepted {forbidden}");
        }
    }

    #[test]
    fn only_public_addresses_are_approved() -> Result<(), Box<dyn std::error::Error>> {
        for public in [
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            IpAddr::V6("2606:4700:4700::1111".parse::<Ipv6Addr>()?),
            IpAddr::V6("::ffff:8.8.8.8".parse::<Ipv6Addr>()?),
        ] {
            assert!(is_public_ip(public), "rejected public {public}");
        }

        for special in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::1",
            "2001:db8::1",
            "3fff::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
        ] {
            let ip = special.parse::<IpAddr>()?;
            assert!(!is_public_ip(ip), "accepted special {special}");
        }
        Ok(())
    }

    #[test]
    fn an_empty_or_mixed_dns_answer_is_refused_as_a_whole() {
        assert!(approve_answers(Vec::new()).is_err());
        let mixed = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
        ];
        assert!(approve_answers(mixed).is_err());
    }

    #[test]
    fn response_diagnostic_never_exceeds_its_byte_budget() {
        let mut body = vec![b'a'; MAX_DETAIL_BYTES - 3];
        assert!(append_bounded(&mut body, &[b'b'; 64]));
        assert_eq!(body.len(), MAX_DETAIL_BYTES);
        assert_eq!(&body[MAX_DETAIL_BYTES - 3..], b"bbb");

        assert!(append_bounded(&mut body, &[b'c'; 64]));
        assert_eq!(body.len(), MAX_DETAIL_BYTES);
        assert_eq!(&body[MAX_DETAIL_BYTES - 3..], b"bbb");
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
