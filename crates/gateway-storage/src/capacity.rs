use gateway_application::RepositoryError;
use sqlx::FromRow;
use uuid::Uuid;

use crate::PostgresRepository;

/// How many amount reservations each address that can still receive money
/// holds, and how many of them belong to a quote that is still payable.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct CollectorOccupancy {
    pub collector_id: Uuid,
    pub asset_id: Uuid,
    pub merchant_id: Option<Uuid>,
    pub state: String,
    pub open_leases: i64,
    pub live_leases: i64,
}

impl PostgresRepository {
    /// # Errors
    ///
    /// Returns [`RepositoryError::Unavailable`] when `PostgreSQL` cannot answer.
    pub async fn collector_occupancy(&self) -> Result<Vec<CollectorOccupancy>, RepositoryError> {
        sqlx::query_as::<_, CollectorOccupancy>(
            r"
            SELECT collector.id AS collector_id,
                   collector.asset_id,
                   collector.merchant_id,
                   collector.state,
                   count(lease.id) AS open_leases,
                   count(lease.id) FILTER (WHERE attempt.status = 'awaiting_payment') AS live_leases
              FROM collector_addresses AS collector
              LEFT JOIN amount_leases AS lease ON lease.collector_address_id = collector.id
              LEFT JOIN payment_attempts AS attempt ON attempt.id = lease.attempt_id
             WHERE collector.state IN ('active', 'receiving_only')
             GROUP BY collector.id
             ORDER BY collector.id
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(|error| RepositoryError::Unavailable(error.to_string()))
    }
}
