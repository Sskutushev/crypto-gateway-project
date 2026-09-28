//! The settlement export, as JSON or as CSV.

use axum::{
    Extension, Json,
    extract::{Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use gateway_application::{AccountingExport, csv_file_name, to_csv};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppState, auth::OperatorAuth, error::ApiError};

/// What the numbers are, said in the response so nobody reads them as
/// something else.
const BASIS: &str = "payment_settlement_decisions with outcome settled, overpaid or partial, \
     grouped by the UTC day of decided_at; payments and fiat_minor count completed obligations \
     only; control sums payment_allocations behind the same decisions";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportQuery {
    from: String,
    to: String,
    #[serde(default)]
    merchant_id: Option<Uuid>,
    #[serde(default)]
    format: Option<String>,
}

#[derive(Debug, Serialize)]
struct DayResponse {
    day: String,
    merchant_id: Uuid,
    merchant_external_id: String,
    asset_id: Uuid,
    fiat_currency: String,
    payments: u64,
    partial_payments: u64,
    overpaid_payments: u64,
    allocated_raw: String,
    fiat_minor: String,
    remainder_raw: String,
}

#[derive(Debug, Serialize)]
struct TotalResponse {
    asset_id: Uuid,
    fiat_currency: String,
    payments: u64,
    partial_payments: u64,
    overpaid_payments: u64,
    allocated_raw: String,
    fiat_minor: String,
    remainder_raw: String,
}

#[derive(Debug, Serialize)]
struct ControlAllocationResponse {
    asset_id: Uuid,
    allocation_rows: u64,
    allocated_raw: String,
}

#[derive(Debug, Serialize)]
struct ControlResponse {
    balanced: bool,
    allocations: Vec<ControlAllocationResponse>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExportResponse {
    from: String,
    to: String,
    merchant_id: Option<Uuid>,
    basis: &'static str,
    days: Vec<DayResponse>,
    totals: Vec<TotalResponse>,
    control: ControlResponse,
}

impl From<AccountingExport> for ExportResponse {
    fn from(v: AccountingExport) -> Self {
        Self {
            from: v.period.from.to_string(),
            to: v.period.to.to_string(),
            merchant_id: v.merchant_id,
            basis: BASIS,
            days: v
                .days
                .into_iter()
                .map(|d| DayResponse {
                    day: d.day.to_string(),
                    merchant_id: d.merchant_id,
                    merchant_external_id: d.merchant_external_id,
                    asset_id: d.asset_id,
                    fiat_currency: d.fiat_currency,
                    payments: d.payments,
                    partial_payments: d.partial_payments,
                    overpaid_payments: d.overpaid_payments,
                    allocated_raw: d.allocated_raw.to_string(),
                    fiat_minor: d.fiat_minor.to_string(),
                    remainder_raw: d.remainder_raw.to_string(),
                })
                .collect(),
            totals: v
                .totals
                .into_iter()
                .map(|t| TotalResponse {
                    asset_id: t.asset_id,
                    fiat_currency: t.fiat_currency,
                    payments: t.payments,
                    partial_payments: t.partial_payments,
                    overpaid_payments: t.overpaid_payments,
                    allocated_raw: t.allocated_raw.to_string(),
                    fiat_minor: t.fiat_minor.to_string(),
                    remainder_raw: t.remainder_raw.to_string(),
                })
                .collect(),
            control: ControlResponse {
                balanced: v.control.balanced,
                allocations: v
                    .control
                    .allocations
                    .into_iter()
                    .map(|c| ControlAllocationResponse {
                        asset_id: c.asset_id,
                        allocation_rows: c.allocation_rows,
                        allocated_raw: c.allocated_raw.to_string(),
                    })
                    .collect(),
            },
        }
    }
}

/// CSV when asked for by `format=csv` or by an `Accept` that names
/// `text/csv`; `format` wins when both are given.
fn wants_csv(format: Option<&str>, headers: &HeaderMap) -> Result<bool, ApiError> {
    match format {
        Some("csv") => Ok(true),
        Some("json") => Ok(false),
        Some(_) => Err(ApiError::InvalidJson),
        None => Ok(headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| {
                accept
                    .split(',')
                    .any(|media| media.trim().starts_with("text/csv"))
            })),
    }
}

pub async fn settlements(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    headers: HeaderMap,
    Query(query): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let csv = wants_csv(query.format.as_deref(), &headers)?;
    let export = state
        .operator_reads
        .settlement_export(&auth.0, &query.from, &query.to, query.merchant_id)
        .await?;
    if csv {
        let disposition = format!("attachment; filename=\"{}\"", csv_file_name(&export.period));
        return Ok((
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
                (header::CONTENT_DISPOSITION, disposition),
            ],
            to_csv(&export),
        )
            .into_response());
    }
    Ok(Json(ExportResponse::from(export)).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::wants_csv;

    #[test]
    fn the_format_parameter_wins_and_accept_decides_otherwise()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut csv_accept = HeaderMap::new();
        csv_accept.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json;q=0.5, text/csv"),
        );
        assert!(wants_csv(None, &csv_accept).map_err(|e| e.to_string())?);
        assert!(!wants_csv(Some("json"), &csv_accept).map_err(|e| e.to_string())?);
        assert!(wants_csv(Some("csv"), &HeaderMap::new()).map_err(|e| e.to_string())?);
        assert!(!wants_csv(None, &HeaderMap::new()).map_err(|e| e.to_string())?);
        assert!(wants_csv(Some("xlsx"), &HeaderMap::new()).is_err());
        Ok(())
    }
}
