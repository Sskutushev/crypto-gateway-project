use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use gateway_application::{
    ApiCredential, OperationsRepository, OperatorCredential, PaymentIntentRepository,
};
use sha2::{Digest, Sha256};

use crate::{AppState, error::ApiError};

#[derive(Debug, Clone)]
pub struct MerchantAuth(pub ApiCredential);

/// An operator key. It is never a merchant key: the two credentials open
/// different doors and are stored in different tables.
#[derive(Debug, Clone)]
pub struct OperatorAuth(pub OperatorCredential);

pub async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let header = request
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    let secret = header
        .strip_prefix("Bearer ")
        .filter(|value| (32..=256).contains(&value.len()))
        .ok_or(ApiError::Unauthorized)?;
    let secret_hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
    let credential = state
        .repository
        .authenticate_api_key(&secret_hash)
        .await?
        .ok_or(ApiError::Unauthorized)?;

    request.extensions_mut().insert(MerchantAuth(credential));
    Ok(next.run(request).await)
}

pub async fn authenticate_operator(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let secret = bearer(&request)?;
    let secret_hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
    let credential = state
        .repository
        .authenticate_operator_key(&secret_hash)
        .await?
        .ok_or(ApiError::Unauthorized)?;

    request.extensions_mut().insert(OperatorAuth(credential));
    Ok(next.run(request).await)
}

/// Reads the bearer secret, refusing anything outside the length a generated
/// key has.
fn bearer(request: &Request) -> Result<String, ApiError> {
    let header = request
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    header
        .strip_prefix("Bearer ")
        .filter(|value| (32..=256).contains(&value.len()))
        .map(ToOwned::to_owned)
        .ok_or(ApiError::Unauthorized)
}
