mod observations;
mod operations;
mod outbox;
mod oversight;
mod postgres;
mod settlement;
#[cfg(test)]
mod test_support;
mod verification;

pub use postgres::PostgresRepository;

pub use sqlx::{PgPool, postgres::PgPoolOptions};

/// Applies all embedded, versioned database migrations.
///
/// # Errors
///
/// Returns [`sqlx::migrate::MigrateError`] when `PostgreSQL` is unavailable or a
/// migration cannot be applied safely.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("../../db/migrations").run(pool).await
}
