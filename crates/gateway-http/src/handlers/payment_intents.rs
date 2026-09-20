use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode},
};
use gateway_application::CreatePaymentIntent;
use gateway_domain::PaymentIntent;
use uuid::Uuid;

use crate::{AppState, auth::MerchantAuth, error::ApiError};

pub async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<MerchantAuth>,
    headers: HeaderMap,
    payload: Result<Json<CreatePaymentIntent>, JsonRejection>,
) -> Result<(StatusCode, HeaderMap, Json<PaymentIntent>), ApiError> {
    let Json(input) = payload.map_err(|_| ApiError::InvalidJson)?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(gateway_application::ServiceError::InvalidIdempotencyKey)?;
    let result = state
        .payment_intents
        .create(auth.0.merchant_id, auth.0.key_id, idempotency_key, input)
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
    Ok((status, response_headers, Json(result.intent)))
}

pub async fn get_by_id(
    State(state): State<AppState>,
    Extension(auth): Extension<MerchantAuth>,
    Path(intent_id): Path<Uuid>,
) -> Result<Json<PaymentIntent>, ApiError> {
    let intent = state
        .payment_intents
        .get(auth.0.merchant_id, intent_id)
        .await?;
    Ok(Json(intent))
}
