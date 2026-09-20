pub mod health;
mod payment_intents;

use axum::{
    Router,
    routing::{get, post},
};

use crate::AppState;

pub fn payment_intent_routes() -> Router<AppState> {
    Router::new()
        .route("/v1/payment-intents", post(payment_intents::create))
        .route(
            "/v1/payment-intents/{intent_id}",
            get(payment_intents::get_by_id),
        )
}
