use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode},
};
use gateway_application::IssueQuote;
use gateway_domain::IssuedQuote;
use serde::Deserialize;
use uuid::Uuid;

use crate::{AppState, auth::MerchantAuth, error::ApiError};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueQuoteBody {
    asset_id: Uuid,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<MerchantAuth>,
    Path(intent_id): Path<Uuid>,
    headers: HeaderMap,
    payload: Result<Json<IssueQuoteBody>, JsonRejection>,
) -> Result<(StatusCode, HeaderMap, Json<IssuedQuote>), ApiError> {
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(gateway_application::QuoteServiceError::InvalidIdempotencyKey)?;
    let result = state
        .quotes
        .issue(
            auth.0.merchant_id,
            auth.0.key_id,
            idempotency_key,
            IssueQuote {
                payment_intent_id: intent_id,
                asset_id: body.asset_id,
            },
        )
        .await?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "idempotent-replayed",
        HeaderValue::from_static(if result.replayed { "true" } else { "false" }),
    );
    let status = if result.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, response_headers, Json(result.quote)))
}
