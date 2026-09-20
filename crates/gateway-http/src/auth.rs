use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use gateway_application::{ApiCredential, PaymentIntentRepository};
use sha2::{Digest, Sha256};

use crate::{AppState, error::ApiError};

#[derive(Debug, Clone)]
pub struct MerchantAuth(pub ApiCredential);

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
