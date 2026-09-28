use crate::{AppState, auth::OperatorAuth, error::ApiError};
use axum::{
    Extension,
    extract::State,
    http::{HeaderValue, header},
    response::IntoResponse,
};
use gateway_application::{Overview, QUOTE_LATENCY_BUCKETS, QUOTE_OUTCOMES, QuoteMetricsSnapshot};
use gateway_scheduler::RunMetricsSnapshot;
use gateway_storage::CollectorOccupancy;
use serde::Serialize;
use std::fmt::Write;
use time::OffsetDateTime;

#[derive(Serialize)]
pub struct OverviewResponse {
    components: Vec<ComponentResponse>,
    open_rail_stops: Vec<RailResponse>,
    latest_reconciliation: Vec<RunSummaryResponse>,
    open_discrepancies: Vec<AggregateResponse>,
    transfers_by_processing_state: Vec<CountResponse>,
    payment_intents_by_status: Vec<CountResponse>,
    outbox_pending: u64,
    outbox_dead_lettered: u64,
    observation_conflicts_open: u64,
}
#[derive(Serialize)]
struct ComponentResponse {
    component: String,
    state: String,
    detail: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    since: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}
#[derive(Serialize)]
struct RailResponse {
    id: uuid::Uuid,
    asset_id: uuid::Uuid,
    reason_code: String,
    detail: Option<String>,
    opened_by: String,
    #[serde(with = "time::serde::rfc3339")]
    opened_at: OffsetDateTime,
}
#[derive(Serialize)]
struct RunSummaryResponse {
    id: uuid::Uuid,
    kind: String,
    status: String,
    #[serde(with = "time::serde::rfc3339")]
    started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    finished_at: Option<OffsetDateTime>,
}
#[derive(Serialize)]
struct AggregateResponse {
    kind: String,
    money_affected: bool,
    count: u64,
}
#[derive(Serialize)]
struct CountResponse {
    state: String,
    count: u64,
}
impl From<Overview> for OverviewResponse {
    fn from(v: Overview) -> Self {
        Self {
            components: v
                .components
                .into_iter()
                .map(|x| ComponentResponse {
                    component: x.component,
                    state: x.state.as_str().to_owned(),
                    detail: x.detail,
                    since: x.since,
                    updated_at: x.updated_at,
                })
                .collect(),
            open_rail_stops: v
                .open_rail_stops
                .into_iter()
                .map(|x| RailResponse {
                    id: x.id,
                    asset_id: x.asset_id,
                    reason_code: x.reason_code,
                    detail: x.detail,
                    opened_by: x.opened_by,
                    opened_at: x.opened_at,
                })
                .collect(),
            latest_reconciliation: v
                .latest_reconciliation
                .into_iter()
                .map(|x| RunSummaryResponse {
                    id: x.id,
                    kind: x.kind,
                    status: x.status,
                    started_at: x.started_at,
                    finished_at: x.finished_at,
                })
                .collect(),
            open_discrepancies: v
                .open_discrepancies
                .into_iter()
                .map(|x| AggregateResponse {
                    kind: x.kind,
                    money_affected: x.money_affected,
                    count: x.count,
                })
                .collect(),
            transfers_by_processing_state: v
                .transfers_by_processing_state
                .into_iter()
                .map(|x| CountResponse {
                    state: x.state,
                    count: x.count,
                })
                .collect(),
            payment_intents_by_status: v
                .payment_intents_by_status
                .into_iter()
                .map(|x| CountResponse {
                    state: x.state,
                    count: x.count,
                })
                .collect(),
            outbox_pending: v.outbox_pending,
            outbox_dead_lettered: v.outbox_dead_lettered,
            observation_conflicts_open: v.observation_conflicts_open,
        }
    }
}

pub async fn metrics(
    State(s): State<AppState>,
    Extension(a): Extension<OperatorAuth>,
) -> Result<impl IntoResponse, ApiError> {
    // Metrics reveal cross-merchant operational state, so the scrape uses the
    // same operator `read` scope as the incident-response API.
    // Do not render a partial snapshot: the architecture requires storage
    // failure to fail the scrape instead of presenting missing data as health.
    let overview = s.operator_reads.overview(&a.0).await?;
    let occupancy = s.repository.collector_occupancy().await?;
    let mut body = render(&overview, s.expiry_metrics.as_ref().map(|m| m.snapshot()));
    render_capacity(&mut body, &occupancy, &s.quote_metrics.snapshot());
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )],
        body,
    ))
}
fn esc(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
fn family(out: &mut String, name: &str, help: &str, kind: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}
#[allow(clippy::too_many_lines)]
pub fn render(o: &Overview, expiry: Option<RunMetricsSnapshot>) -> String {
    let mut x = String::new();
    family(
        &mut x,
        "gateway_component_state",
        "Current component state.",
        "gauge",
    );
    for c in &o.components {
        // One-hot states preserve label meaning and allow aggregation; a
        // numeric enum would make dashboards depend on an undocumented code.
        for state in ["ok", "degraded", "unavailable", "diverged", "stopped"] {
            let _ = writeln!(
                x,
                "gateway_component_state{{component=\"{}\",state=\"{state}\"}} {}",
                esc(&c.component),
                u8::from(c.state.as_str() == state)
            );
        }
    }
    family(
        &mut x,
        "gateway_component_state_since_seconds",
        "Unix time when the component entered its state.",
        "gauge",
    );
    for c in &o.components {
        let _ = writeln!(
            x,
            "gateway_component_state_since_seconds{{component=\"{}\"}} {}",
            esc(&c.component),
            c.since.unix_timestamp()
        );
    }
    family(
        &mut x,
        "gateway_rail_stops_open",
        "Open rail stops.",
        "gauge",
    );
    for r in &o.open_rail_stops {
        let _ = writeln!(
            x,
            "gateway_rail_stops_open{{asset_id=\"{}\"}} 1",
            r.asset_id
        );
    }
    family(
        &mut x,
        "gateway_reconciliation_last_run_timestamp_seconds",
        "Last reconciliation run time.",
        "gauge",
    );
    family(
        &mut x,
        "gateway_reconciliation_last_run_status",
        "Last reconciliation status.",
        "gauge",
    );
    for r in &o.latest_reconciliation {
        let _ = writeln!(
            x,
            "gateway_reconciliation_last_run_timestamp_seconds{{kind=\"{}\"}} {}",
            esc(&r.kind),
            r.started_at.unix_timestamp()
        );
        for status in ["ok", "drift", "hard_stop"] {
            let _ = writeln!(
                x,
                "gateway_reconciliation_last_run_status{{kind=\"{}\",status=\"{status}\"}} {}",
                esc(&r.kind),
                u8::from(r.status == status)
            );
        }
    }
    family(
        &mut x,
        "gateway_reconciliation_open_discrepancies",
        "Open reconciliation discrepancies.",
        "gauge",
    );
    for d in &o.open_discrepancies {
        let _ = writeln!(
            x,
            "gateway_reconciliation_open_discrepancies{{kind=\"{}\",money_affected=\"{}\"}} {}",
            esc(&d.kind),
            d.money_affected,
            d.count
        );
    }
    render_counts(
        &mut x,
        "gateway_transfers_processing",
        "Transfers by processing state.",
        "state",
        &o.transfers_by_processing_state,
    );
    render_counts(
        &mut x,
        "gateway_payment_intents",
        "Payment intents by status.",
        "status",
        &o.payment_intents_by_status,
    );
    for (name, help, value) in [
        (
            "gateway_outbox_events_pending",
            "Pending outbox events.",
            o.outbox_pending,
        ),
        (
            "gateway_outbox_events_dead_lettered",
            "Dead-lettered outbox events.",
            o.outbox_dead_lettered,
        ),
        (
            "gateway_observation_conflicts_open",
            "Open observation conflicts.",
            o.observation_conflicts_open,
        ),
    ] {
        family(&mut x, name, help, "gauge");
        let _ = writeln!(x, "{name} {value}");
    }
    // When the scheduler is disabled there is no producer for these values;
    // omitting the families distinguishes that from a running zero counter.
    if let Some(m) = expiry {
        family(
            &mut x,
            "gateway_expiry_runs_total",
            "Expiry scheduler runs.",
            "counter",
        );
        for (result, value) in [
            ("succeeded", m.runs_succeeded),
            ("failed", m.runs_failed),
            ("skipped_overlapping", m.runs_skipped_overlapping),
            ("not_leader", m.runs_not_leader),
        ] {
            let _ = writeln!(
                x,
                "gateway_expiry_runs_total{{result=\"{result}\"}} {value}"
            );
        }
        family(
            &mut x,
            "gateway_expiry_items_processed_total",
            "Expiry items processed.",
            "counter",
        );
        let _ = writeln!(
            x,
            "gateway_expiry_items_processed_total {}",
            m.items_processed
        );
        if let Some(value) = m.last_success_unix {
            family(
                &mut x,
                "gateway_expiry_last_success_timestamp_seconds",
                "Last successful expiry run.",
                "gauge",
            );
            let _ = writeln!(x, "gateway_expiry_last_success_timestamp_seconds {value}");
        }
    }
    x
}
/// Address load from the database and quote outcomes from this process.
pub fn render_capacity(
    out: &mut String,
    occupancy: &[CollectorOccupancy],
    quotes: &QuoteMetricsSnapshot,
) {
    family(
        out,
        "gateway_collector_open_leases",
        "Amount reservations held per receiving address, including those kept for late money.",
        "gauge",
    );
    family(
        out,
        "gateway_collector_live_leases",
        "Amount reservations per receiving address whose quote is still payable.",
        "gauge",
    );
    for c in occupancy {
        let labels = format!(
            "collector_id=\"{}\",asset_id=\"{}\",merchant_id=\"{}\",state=\"{}\"",
            c.collector_id,
            c.asset_id,
            c.merchant_id
                .map_or_else(|| "shared".to_owned(), |id| id.to_string()),
            esc(&c.state)
        );
        let _ = writeln!(
            out,
            "gateway_collector_open_leases{{{labels}}} {}",
            c.open_leases
        );
        let _ = writeln!(
            out,
            "gateway_collector_live_leases{{{labels}}} {}",
            c.live_leases
        );
    }
    family(
        out,
        "gateway_quote_requests_total",
        "Quote requests handled by this process, by outcome.",
        "counter",
    );
    for (outcome, value) in QUOTE_OUTCOMES.iter().zip(&quotes.outcomes) {
        let _ = writeln!(
            out,
            "gateway_quote_requests_total{{outcome=\"{outcome}\"}} {value}"
        );
    }
    family(
        out,
        "gateway_quote_collector_spillovers_total",
        "Quotes issued on a merchant address other than its least loaded one.",
        "counter",
    );
    let _ = writeln!(
        out,
        "gateway_quote_collector_spillovers_total {}",
        quotes.spillovers
    );
    family(
        out,
        "gateway_quote_duration_seconds",
        "Time to answer a quote request, including refusals.",
        "histogram",
    );
    for (bound, value) in QUOTE_LATENCY_BUCKETS.iter().zip(&quotes.latency_buckets) {
        let _ = writeln!(
            out,
            "gateway_quote_duration_seconds_bucket{{le=\"{bound}\"}} {value}"
        );
    }
    let _ = writeln!(
        out,
        "gateway_quote_duration_seconds_bucket{{le=\"+Inf\"}} {}",
        quotes.latency_count
    );
    let _ = writeln!(
        out,
        "gateway_quote_duration_seconds_sum {}.{:06}",
        quotes.latency_sum_micros / 1_000_000,
        quotes.latency_sum_micros % 1_000_000
    );
    let _ = writeln!(
        out,
        "gateway_quote_duration_seconds_count {}",
        quotes.latency_count
    );
}

fn render_counts(
    out: &mut String,
    name: &str,
    help: &str,
    label: &str,
    values: &[gateway_application::StateCount],
) {
    family(out, name, help, "gauge");
    for v in values {
        let _ = writeln!(out, "{name}{{{label}=\"{}\"}} {}", esc(&v.state), v.count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gateway_application::{ComponentStatus, Overview};
    use time::macros::datetime;

    fn overview() -> Overview {
        Overview {
            components: vec![ComponentStatus {
                component: "bad\\\"\nname".into(),
                state: gateway_application::ComponentState::Degraded,
                detail: None,
                since: datetime!(2026-09-23 00:00 UTC),
                updated_at: datetime!(2026-09-23 00:00 UTC),
            }],
            open_rail_stops: vec![],
            latest_reconciliation: vec![],
            open_discrepancies: vec![],
            transfers_by_processing_state: vec![],
            payment_intents_by_status: vec![],
            outbox_pending: 0,
            outbox_dead_lettered: 0,
            observation_conflicts_open: 0,
        }
    }
    #[test]
    fn escapes_labels_and_writes_metadata_once() {
        let text = render(&overview(), None);
        assert!(text.contains("component=\"bad\\\\\\\"\\nname\""));
        assert_eq!(text.matches("# HELP gateway_component_state ").count(), 1);
        assert!(!text.contains("gateway_expiry_runs_total"));
    }
    #[test]
    fn capacity_is_rendered_per_address_and_the_histogram_is_complete() {
        let mut text = String::new();
        render_capacity(
            &mut text,
            &[CollectorOccupancy {
                collector_id: uuid::Uuid::from_u128(1),
                asset_id: uuid::Uuid::from_u128(2),
                merchant_id: None,
                state: "active".into(),
                open_leases: 7,
                live_leases: 3,
            }],
            &QuoteMetricsSnapshot {
                outcomes: vec![5, 1, 2, 0, 0, 0, 0, 0, 0],
                spillovers: 2,
                latency_buckets: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 8],
                latency_count: 8,
                latency_sum_micros: 1_500_000,
            },
        );
        assert!(text.contains("merchant_id=\"shared\",state=\"active\"} 7"));
        assert!(text.contains("gateway_collector_live_leases{"));
        assert!(text.contains("gateway_quote_requests_total{outcome=\"slots_exhausted\"} 2"));
        assert!(text.contains("gateway_quote_duration_seconds_bucket{le=\"+Inf\"} 8"));
        assert!(text.contains("gateway_quote_duration_seconds_sum 1.500000"));
        assert!(text.contains("gateway_quote_collector_spillovers_total 2"));
    }
    #[test]
    fn omits_last_success_until_one_exists() {
        let snapshot = RunMetricsSnapshot {
            runs_started: 0,
            runs_succeeded: 0,
            runs_failed: 0,
            runs_skipped_overlapping: 0,
            batches_executed: 0,
            items_processed: 0,
            transient_retries: 0,
            consecutive_failures: 0,
            backlog_left: 0,
            runs_not_leader: 0,
            last_success_unix: None,
        };
        let text = render(&overview(), Some(snapshot));
        assert!(!text.contains("gateway_expiry_last_success_timestamp_seconds"));
    }
}
