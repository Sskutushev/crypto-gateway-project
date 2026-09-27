//! The hosted payment page: public, read-only, opened by an unguessable
//! per-attempt token.
//!
//! Every response forbids caching and referrers (the token is in the URL), and
//! the page runs under a content security policy that allows only its own
//! script, style and QR image: no inline code, nothing third party.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use gateway_application::CheckoutView;
use qrcode::{QrCode, render::svg};

use crate::{AppState, error::ApiError};

const PAGE: &str = include_str!("../checkout/page.html");
const SCRIPT: &str = include_str!("../checkout/checkout.js");
const STYLE: &str = include_str!("../checkout/checkout.css");

const PAGE_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'";

fn private_headers(content_type: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers
}

async fn view(state: &AppState, token: &str) -> Result<CheckoutView, ApiError> {
    state
        .checkout
        .view(token)
        .await?
        .ok_or(ApiError::CheckoutNotFound)
}

/// `GET /v1/checkout/{checkout_token}`: the buyer-facing state as JSON.
pub async fn status(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<(HeaderMap, Json<CheckoutView>), ApiError> {
    let view = view(&state, &token).await?;
    Ok((private_headers("application/json"), Json(view)))
}

/// `GET /v1/checkout/{checkout_token}/qr.svg`: the payment address as a QR code.
///
/// It encodes the address only. Wallets differ in which payment URI fields
/// they honour, and an amount a wallet silently dropped would be worse than
/// the buyer typing the exact amount shown beside it.
pub async fn qr(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, ApiError> {
    let view = view(&state, &token).await?;
    let code = QrCode::new(view.collector_address.as_bytes())
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let image = code
        .render::<svg::Color<'_>>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .build();
    Ok((private_headers("image/svg+xml"), image).into_response())
}

/// `GET /checkout/{checkout_token}`: the page. It is the same document for
/// every token; the script reads the token from the address and asks the API.
pub async fn page(Path(_token): Path<String>) -> Response {
    let mut headers = private_headers("text/html; charset=utf-8");
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PAGE_POLICY),
    );
    (StatusCode::OK, headers, PAGE).into_response()
}

pub async fn script() -> Response {
    (private_headers("text/javascript; charset=utf-8"), SCRIPT).into_response()
}

pub async fn style() -> Response {
    (private_headers("text/css; charset=utf-8"), STYLE).into_response()
}
