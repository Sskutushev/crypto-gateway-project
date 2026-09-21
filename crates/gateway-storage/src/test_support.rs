use std::{env, error::Error};

use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::sync::Mutex;

use crate::migrate;

/// Every database scenario truncates and reseeds the same schema, so they must
/// not interleave even when the harness runs them in parallel.
pub(crate) static DATABASE: Mutex<()> = Mutex::const_new(());

/// Connects to the disposable test database named by the environment and
/// applies all migrations.
pub(crate) async fn connect() -> Result<PgPool, Box<dyn Error>> {
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// Returns the test database URL with different credentials, so a scenario can
/// connect as a restricted role instead of the owner.
pub(crate) fn database_url_as(user: &str, password: &str) -> Result<String, Box<dyn Error>> {
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let (scheme, rest) = database_url
        .split_once("://")
        .ok_or("GATEWAY_TEST_DATABASE_URL is not a URL")?;
    let host_and_path = rest.rsplit_once('@').map_or(rest, |(_, tail)| tail);
    Ok(format!("{scheme}://{user}:{password}@{host_and_path}"))
}
