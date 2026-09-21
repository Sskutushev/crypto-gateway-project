use axum::{Json, http::StatusCode, response::IntoResponse};
use gateway_application::{QuoteServiceError, RepositoryError, ServiceError};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("authentication failed")]
    Unauthorized,
    #[error("request body does not match the endpoint schema")]
    InvalidJson,
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Quote(#[from] QuoteServiceError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
    #[error("database readiness check failed")]
    NotReady,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let error_message = self.to_string();
        let (status, code, message) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "authentication_failed",
                "a valid merchant API key is required".to_owned(),
            ),
            Self::InvalidJson => (
                StatusCode::BAD_REQUEST,
                "invalid_request",
                error_message.clone(),
            ),
            Self::Service(ServiceError::InvalidIdempotencyKey)
            | Self::Quote(QuoteServiceError::InvalidIdempotencyKey) => (
                StatusCode::BAD_REQUEST,
                "invalid_idempotency_key",
                error_message.clone(),
            ),
            Self::Service(ServiceError::NotFound)
            | Self::Quote(QuoteServiceError::PaymentIntentNotFound) => (
                StatusCode::NOT_FOUND,
                "payment_intent_not_found",
                error_message.clone(),
            ),
            Self::Service(ServiceError::Repository(RepositoryError::IdempotencyConflict))
            | Self::Quote(QuoteServiceError::Repository(RepositoryError::IdempotencyConflict))
            | Self::Repository(RepositoryError::IdempotencyConflict) => (
                StatusCode::CONFLICT,
                "idempotency_conflict",
                error_message.clone(),
            ),
            Self::Service(ServiceError::Repository(RepositoryError::DuplicateReference))
            | Self::Repository(RepositoryError::DuplicateReference) => (
                StatusCode::CONFLICT,
                "payment_intent_reference_conflict",
                error_message.clone(),
            ),
            Self::Service(ServiceError::Money(_) | ServiceError::PaymentIntent(_)) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                error_message.clone(),
            ),
            Self::Quote(QuoteServiceError::Repository(
                RepositoryError::PaymentIntentNotQuotable,
            )) => (
                StatusCode::CONFLICT,
                "payment_intent_not_quotable",
                error_message.clone(),
            ),
            Self::Quote(
                QuoteServiceError::Quote(_)
                | QuoteServiceError::Repository(
                    RepositoryError::CollectorUnavailable | RepositoryError::AmountSlotsExhausted,
                ),
            ) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "quote_unavailable",
                "a safe quote cannot be issued right now".to_owned(),
            ),
            Self::NotReady => (
                StatusCode::SERVICE_UNAVAILABLE,
                "not_ready",
                "the service cannot reach its database".to_owned(),
            ),
            Self::Service(_) | Self::Quote(_) | Self::Repository(_) => {
                tracing::error!(error = %error_message, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "the request could not be completed".to_owned(),
                )
            }
        };
        (
            status,
            Json(ErrorEnvelope {
                error: ErrorBody { code, message },
            }),
        )
            .into_response()
    }
}
