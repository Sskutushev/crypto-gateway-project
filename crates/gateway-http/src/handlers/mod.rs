pub mod health;
mod operator;
mod payment_intents;
mod quotes;

pub use operator::status_for;

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
        .route(
            "/v1/payment-intents/{intent_id}/quotes",
            post(quotes::create),
        )
}

/// The operator surface: evidence in, and the switch that closes a rail.
pub fn operator_routes() -> Router<AppState> {
    Router::new()
        .route("/v1/operator/price-snapshots", post(operator::submit_price))
        .route(
            "/v1/operator/rail-health",
            post(operator::submit_rail_health),
        )
        .route("/v1/operator/rail-stops", post(operator::open_rail_stop))
        .route(
            "/v1/operator/rail-stops/{asset_id}/clear",
            post(operator::clear_rail_stop),
        )
        .route(
            "/v1/operator/transfers/{transfer_id}/risk-evaluations",
            post(operator::submit_risk_evaluation),
        )
}
