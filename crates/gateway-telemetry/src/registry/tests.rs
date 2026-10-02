use std::time::Duration;

use super::{Counter, Gauge, Histogram, Labels, render, seconds};

static REQUESTS: Counter = Counter::new("test_requests_total", "Requests seen.");
static IN_FLIGHT: Gauge = Gauge::new("test_in_flight", "Requests in flight.");
static LATENCY: Histogram = Histogram::new(
    "test_latency_seconds",
    "How long a request took.",
    &[1_000, 10_000, 1_000_000],
);

#[test]
fn durations_render_as_seconds_without_floating_point() {
    assert_eq!(seconds(0), "0");
    assert_eq!(seconds(1_500), "0.0015");
    assert_eq!(seconds(2_000_000), "2");
    assert_eq!(seconds(2_000_001), "2.000001");
    assert_eq!(seconds(600_000_000), "600");
}

#[test]
fn a_histogram_is_cumulative_and_a_label_value_is_escaped() {
    // The three statics are shared by every test in this file, so each
    // assertion reads only its own label set.
    let labels = Labels::new(&[("route", "/v1/\"quoted\""), ("status", "200")]);
    LATENCY.observe(&labels, Duration::from_micros(5_000));
    LATENCY.observe(&labels, Duration::from_micros(20_000));
    LATENCY.observe(&labels, Duration::from_secs(3));
    let text = render();
    let line = |suffix: &str| {
        format!("test_latency_seconds{suffix}{{route=\"/v1/\\\"quoted\\\"\",status=\"200\"")
    };
    assert!(
        text.contains("# TYPE test_latency_seconds histogram"),
        "{text}"
    );
    assert!(
        text.contains(&format!("{},le=\"0.001\"}} 0\n", line("_bucket"))),
        "{text}"
    );
    assert!(
        text.contains(&format!("{},le=\"0.01\"}} 1\n", line("_bucket"))),
        "{text}"
    );
    assert!(
        text.contains(&format!("{},le=\"1\"}} 2\n", line("_bucket"))),
        "{text}"
    );
    assert!(
        text.contains(&format!("{},le=\"+Inf\"}} 3\n", line("_bucket"))),
        "{text}"
    );
    assert!(
        text.contains(&format!("{}}} 3.025\n", line("_sum"))),
        "{text}"
    );
    assert!(
        text.contains(&format!("{}}} 3\n", line("_count"))),
        "{text}"
    );
}

#[test]
fn counters_and_gauges_keep_one_row_per_label_set() {
    let ok = Labels::new(&[("outcome", "ok")]);
    let failed = Labels::new(&[("outcome", "failed")]);
    REQUESTS.increment(&ok, 2);
    REQUESTS.increment(&ok, 1);
    REQUESTS.increment(&failed, 1);
    IN_FLIGHT.add(&Labels::none(), 2);
    IN_FLIGHT.add(&Labels::none(), -1);
    let text = render();
    assert!(
        text.contains("# TYPE test_requests_total counter"),
        "{text}"
    );
    assert!(
        text.contains("test_requests_total{outcome=\"ok\"} 3\n"),
        "{text}"
    );
    assert!(
        text.contains("test_requests_total{outcome=\"failed\"} 1\n"),
        "{text}"
    );
    assert!(
        text.contains("# TYPE test_in_flight gauge\ntest_in_flight 1\n"),
        "{text}"
    );
    IN_FLIGHT.set(&Labels::none(), 7);
    assert!(render().contains("test_in_flight 7\n"));
}

#[test]
fn a_metric_never_written_is_absent_and_a_sum_saturates() {
    static UNTOUCHED: Counter = Counter::new("test_untouched_total", "Never written.");
    static WIDE: Histogram = Histogram::new("test_wide_seconds", "Saturating sum.", &[1]);
    let _ = &UNTOUCHED;
    assert!(!render().contains("test_untouched_total"));
    WIDE.observe(&Labels::none(), Duration::MAX);
    WIDE.observe(&Labels::none(), Duration::from_secs(1));
    let text = render();
    assert!(
        text.contains(&format!("test_wide_seconds_sum {}\n", seconds(u64::MAX))),
        "{text}"
    );
    assert!(text.contains("test_wide_seconds_count 2\n"), "{text}");
}
