//! Request latency by route and status, for this process.

use std::time::Instant;

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use gateway_telemetry::{Gauge, Histogram, LATENCY_BUCKETS_MICROS, Labels};

static REQUEST_DURATION: Histogram = Histogram::new(
    "gateway_http_request_duration_seconds",
    "How long the API took to answer, by matched route and status.",
    &LATENCY_BUCKETS_MICROS,
);

static IN_FLIGHT: Gauge = Gauge::new(
    "gateway_http_requests_in_flight",
    "Requests the API is answering right now.",
);

/// Records the duration and status of every request.
///
/// The route label is the matched pattern, never the raw path: a path carries
/// a payment intent id, and an id per row would grow the registry with every
/// request. A request no route matched is labelled `unmatched`.
pub async fn observe(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned());
    let started = Instant::now();
    let _in_flight = InFlight::enter();
    let response = next.run(request).await;
    REQUEST_DURATION.observe(
        &Labels::new(&[
            ("method", method.as_str()),
            ("route", route.as_deref().unwrap_or("unmatched")),
            ("status", response.status().as_str()),
        ]),
        started.elapsed(),
    );
    response
}

/// A request that is cancelled (the client went away, the timeout fired)
/// never reaches the end of `observe`; the gauge still has to come back down.
struct InFlight;

impl InFlight {
    fn enter() -> Self {
        IN_FLIGHT.add(&Labels::none(), 1);
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.add(&Labels::none(), -1);
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use gateway_storage::PgPoolOptions;
    use tower::ServiceExt;

    use crate::{AppState, router};

    const LIVE: &str = "gateway_http_request_duration_seconds_count{method=\"GET\",route=\"/health/live\",status=\"200\"} ";
    const UNMATCHED: &str = "gateway_http_request_duration_seconds_count{method=\"GET\",route=\"unmatched\",status=\"404\"} ";

    /// The value of the one sample line starting with `prefix`, or zero when
    /// the series has not been written yet.
    fn sample(text: &str, prefix: &str) -> Result<u64, Box<dyn Error>> {
        match text.lines().find_map(|line| line.strip_prefix(prefix)) {
            Some(value) => Ok(value.parse()?),
            None => Ok(0),
        }
    }

    #[tokio::test]
    async fn a_request_is_counted_under_its_matched_route_and_status() -> Result<(), Box<dyn Error>>
    {
        // The registry is shared by every test in this binary, so the test
        // reads the change it caused rather than an absolute count.
        let before = gateway_telemetry::render();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://nobody:nobody@127.0.0.1:1/nowhere")?;
        let app = router(AppState::new(pool));

        let live = app
            .clone()
            .oneshot(Request::get("/health/live").body(Body::empty())?)
            .await?;
        assert_eq!(live.status(), StatusCode::OK);
        let missing = app
            .oneshot(Request::get("/v1/nothing-here").body(Body::empty())?)
            .await?;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let after = gateway_telemetry::render();
        assert_eq!(sample(&after, LIVE)?, sample(&before, LIVE)? + 1, "{after}");
        assert_eq!(
            sample(&after, UNMATCHED)?,
            sample(&before, UNMATCHED)? + 1,
            "{after}"
        );
        assert!(
            after.contains("# TYPE gateway_http_requests_in_flight gauge\n"),
            "{after}"
        );
        Ok(())
    }
}
