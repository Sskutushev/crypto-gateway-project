//! Provider call latency, per provider host and endpoint, for this process.

use std::time::Duration;

use gateway_telemetry::{Histogram, LATENCY_BUCKETS_MICROS, Labels};

static REQUEST_DURATION: Histogram = Histogram::new(
    "gateway_chain_source_request_duration_seconds",
    "How long a provider took to answer, by provider host, endpoint and outcome.",
    &LATENCY_BUCKETS_MICROS,
);

/// What a provider call ended in, as it is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// A 2xx answer with a body.
    Ok,
    /// The provider answered with a status outside 2xx.
    Refused,
    /// No answer: connection, TLS, timeout, or an unreadable body.
    Unreachable,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Refused => "refused",
            Self::Unreachable => "unreachable",
        }
    }
}

pub(crate) fn record_request(provider: &str, endpoint: &str, outcome: Outcome, elapsed: Duration) {
    REQUEST_DURATION.observe(
        &Labels::new(&[
            ("provider", provider),
            ("endpoint", endpoint),
            ("outcome", outcome.as_str()),
        ]),
        elapsed,
    );
}

/// The endpoint label: the first two path segments, so
/// `v1/accounts/<address>/transactions/trc20?...` is `v1/accounts` and
/// `wallet/getnowblock` is itself. An address in a label would be a row per
/// collector; the set below is bounded by the code that builds the paths.
pub(crate) fn endpoint_label(path_and_query: &str) -> String {
    path_and_query
        .split('?')
        .next()
        .unwrap_or_default()
        .trim_start_matches('/')
        .split('/')
        .take(2)
        .collect::<Vec<_>>()
        .join("/")
}

/// The provider label: the host of the base URL, which is what an operator
/// recognises on a dashboard. A base URL that is not a URL is labelled as is.
pub(crate) fn provider_label(base_url: &str) -> String {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| base_url.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::integer_division,
        clippy::string_slice
    )]
    use super::{endpoint_label, provider_label};

    #[test]
    fn labels_are_bounded_by_the_code_that_builds_the_paths() {
        assert_eq!(endpoint_label("wallet/getnowblock"), "wallet/getnowblock");
        assert_eq!(
            endpoint_label("v1/accounts/TXYZ123/transactions/trc20?only_to=true&limit=200"),
            "v1/accounts"
        );
        assert_eq!(
            endpoint_label("/walletsolidity/getnowblock"),
            "walletsolidity/getnowblock"
        );
        assert_eq!(provider_label("https://api.trongrid.io"), "api.trongrid.io");
        assert_eq!(
            provider_label("http://tron-node.internal:8090/"),
            "tron-node.internal"
        );
        assert_eq!(provider_label("not a url"), "not a url");
    }
}
