use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::{IssueQuoteResult, QuoteServiceError, RepositoryError};

/// Upper bounds, in seconds, of the quote latency histogram buckets.
pub const QUOTE_LATENCY_BUCKETS: [f64; 10] =
    [0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];

/// Outcomes a quote request can end in, as they are counted.
pub const QUOTE_OUTCOMES: [&str; 9] = [
    "issued",
    "replayed",
    "slots_exhausted",
    "capacity_exhausted",
    "collector_unavailable",
    "rail_stopped",
    "not_quotable",
    "evidence_refused",
    "failed",
];

/// In-process counters for quote issuance. They describe this process only;
/// a scrape of each replica is summed by the monitoring system.
#[derive(Debug, Default)]
pub struct QuoteMetrics {
    outcomes: [AtomicU64; QUOTE_OUTCOMES.len()],
    /// Quotes that moved past the merchant's least loaded address because the
    /// amounts near the price were taken there.
    pub(crate) spillovers: AtomicU64,
    buckets: [AtomicU64; QUOTE_LATENCY_BUCKETS.len()],
    latency_count: AtomicU64,
    latency_sum_micros: AtomicU64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteMetricsSnapshot {
    /// One count per entry of [`QUOTE_OUTCOMES`], in the same order.
    pub outcomes: Vec<u64>,
    pub spillovers: u64,
    /// Cumulative counts per entry of [`QUOTE_LATENCY_BUCKETS`].
    pub latency_buckets: Vec<u64>,
    pub latency_count: u64,
    pub latency_sum_micros: u64,
}

impl QuoteMetrics {
    pub(crate) fn record(
        &self,
        result: &Result<IssueQuoteResult, QuoteServiceError>,
        elapsed: Duration,
    ) {
        let outcome = outcome(result);
        if let Some(index) = QUOTE_OUTCOMES.iter().position(|name| *name == outcome) {
            self.outcomes[index].fetch_add(1, Ordering::Relaxed);
        }
        let seconds = elapsed.as_secs_f64();
        for (bound, bucket) in QUOTE_LATENCY_BUCKETS.iter().zip(&self.buckets) {
            if seconds <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        self.latency_sum_micros.fetch_add(
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    #[must_use]
    pub fn snapshot(&self) -> QuoteMetricsSnapshot {
        QuoteMetricsSnapshot {
            outcomes: self
                .outcomes
                .iter()
                .map(|value| value.load(Ordering::Relaxed))
                .collect(),
            spillovers: self.spillovers.load(Ordering::Relaxed),
            latency_buckets: self
                .buckets
                .iter()
                .map(|value| value.load(Ordering::Relaxed))
                .collect(),
            latency_count: self.latency_count.load(Ordering::Relaxed),
            latency_sum_micros: self.latency_sum_micros.load(Ordering::Relaxed),
        }
    }
}

const fn outcome(result: &Result<IssueQuoteResult, QuoteServiceError>) -> &'static str {
    match result {
        Ok(IssueQuoteResult { replayed: true, .. }) => "replayed",
        Ok(_) => "issued",
        Err(QuoteServiceError::Repository(RepositoryError::AmountSlotsExhausted)) => {
            "slots_exhausted"
        }
        Err(QuoteServiceError::CapacityExhausted) => "capacity_exhausted",
        Err(QuoteServiceError::Repository(RepositoryError::CollectorUnavailable)) => {
            "collector_unavailable"
        }
        Err(QuoteServiceError::RailStopped(_)) => "rail_stopped",
        Err(
            QuoteServiceError::Repository(RepositoryError::PaymentIntentNotQuotable)
            | QuoteServiceError::PaymentIntentNotFound,
        ) => "not_quotable",
        Err(QuoteServiceError::Quote(_)) => "evidence_refused",
        Err(_) => "failed",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{QUOTE_OUTCOMES, QuoteMetrics};
    use crate::{QuoteServiceError, RepositoryError};

    #[test]
    fn each_refusal_is_counted_under_its_own_outcome_and_latency_is_cumulative() {
        let metrics = QuoteMetrics::default();
        metrics.record(
            &Err(QuoteServiceError::Repository(
                RepositoryError::AmountSlotsExhausted,
            )),
            Duration::from_millis(30),
        );
        metrics.record(
            &Err(QuoteServiceError::CapacityExhausted),
            Duration::from_secs(20),
        );
        let snapshot = metrics.snapshot();
        let count = |name: &str| {
            QUOTE_OUTCOMES
                .iter()
                .position(|outcome| *outcome == name)
                .map(|index| snapshot.outcomes[index])
        };
        assert_eq!(count("slots_exhausted"), Some(1));
        assert_eq!(count("capacity_exhausted"), Some(1));
        assert_eq!(count("issued"), Some(0));
        // 30 ms lands in the 0.05 s bucket and every larger one; 20 s in none.
        assert_eq!(snapshot.latency_buckets, vec![0, 0, 1, 1, 1, 1, 1, 1, 1, 1]);
        assert_eq!(snapshot.latency_count, 2);
        assert_eq!(snapshot.latency_sum_micros, 20_030_000);
    }
}
