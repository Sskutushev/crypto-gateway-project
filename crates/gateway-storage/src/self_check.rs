//! PostgreSQL-backed start-up invariants.
//!
//! Each check answers one question about whether this process and this
//! database describe the same deployment. A failed row names what disagreed,
//! because "not ready" without a reason sends an operator to the logs of every
//! component at once.

use std::fmt::Write as _;

use async_trait::async_trait;
use gateway_application::{RepositoryError, SelfCheckConfig, SelfCheckRepository, SelfCheckResult};
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::OffsetDateTime;

use crate::postgres::{PostgresRepository, unavailable};

#[async_trait]
impl SelfCheckRepository for PostgresRepository {
    async fn self_check(
        &self,
        config: &SelfCheckConfig,
        process_now: OffsetDateTime,
    ) -> Result<Vec<SelfCheckResult>, RepositoryError> {
        Ok(vec![
            self.pinned_collectors(config).await?,
            self.pinned_assets(config).await?,
            self.environment_agreement(config).await?,
            self.finality_policy_present().await?,
            self.cursor_sanity().await?,
            self.clock_skew(config, process_now).await?,
        ])
    }
}

impl PostgresRepository {
    /// Every receiving collector re-hashes to its pin and is named by the
    /// configuration, and every configured collector is receiving.
    ///
    /// Both directions matter: a database-only address is money redirected
    /// without review, and a configured address the database no longer serves
    /// is a deployment that believes it collects where it does not.
    async fn pinned_collectors(
        &self,
        config: &SelfCheckConfig,
    ) -> Result<SelfCheckResult, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT address_key, address_text, pinned_sha256
              FROM collector_addresses
             WHERE state IN ('active', 'receiving_only')
             ORDER BY address_text
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        let mut problems = Vec::new();
        let mut keys = Vec::with_capacity(rows.len());
        for row in &rows {
            let key: Vec<u8> = row.try_get("address_key").map_err(unavailable)?;
            let text: String = row.try_get("address_text").map_err(unavailable)?;
            let pin: String = row.try_get("pinned_sha256").map_err(unavailable)?;
            if pin != sha256_hex(&key) {
                problems.push(format!("collector {text} does not re-hash to its pin"));
            }
            if !config
                .collectors
                .iter()
                .any(|expected| expected.as_bytes() == key.as_slice())
            {
                problems.push(format!("collector {text} is receiving but not configured"));
            }
            keys.push(key);
        }
        for expected in &config.collectors {
            if !keys.iter().any(|key| key.as_slice() == expected.as_bytes()) {
                problems.push(format!(
                    "configured collector {} is not receiving in the database",
                    hex(expected.as_bytes())
                ));
            }
        }
        Ok(verdict(
            "pinned_collectors",
            &problems,
            format!(
                "{} configured collector(s), {} receiving collector row(s)",
                config.collectors.len(),
                rows.len()
            ),
        ))
    }

    /// The same two-way agreement for token contracts.
    async fn pinned_assets(
        &self,
        config: &SelfCheckConfig,
    ) -> Result<SelfCheckResult, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT chain, network, contract_address_key, display_symbol, pinned_sha256
              FROM chain_assets
             WHERE status = 'active'
             ORDER BY chain, network, display_symbol
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        let mut problems = Vec::new();
        let mut seen: Vec<(String, String, Vec<u8>)> = Vec::with_capacity(rows.len());
        for row in &rows {
            let chain: String = row.try_get("chain").map_err(unavailable)?;
            let network: String = row.try_get("network").map_err(unavailable)?;
            let key: Vec<u8> = row.try_get("contract_address_key").map_err(unavailable)?;
            let symbol: String = row.try_get("display_symbol").map_err(unavailable)?;
            let pin: String = row.try_get("pinned_sha256").map_err(unavailable)?;
            let name = format!("{chain}:{network}:{symbol}");
            if pin != sha256_hex(&key) {
                problems.push(format!("asset {name} does not re-hash to its pin"));
            }
            if !config.assets.iter().any(|expected| {
                expected.chain == chain
                    && expected.network == network
                    && expected.contract.as_bytes() == key.as_slice()
            }) {
                problems.push(format!("asset {name} is active but not configured"));
            }
            seen.push((chain, network, key));
        }
        for expected in &config.assets {
            if !seen.iter().any(|(chain, network, key)| {
                *chain == expected.chain
                    && *network == expected.network
                    && key.as_slice() == expected.contract.as_bytes()
            }) {
                problems.push(format!(
                    "configured asset {}:{}:{} is not active in the database",
                    expected.chain,
                    expected.network,
                    hex(expected.contract.as_bytes())
                ));
            }
        }
        Ok(verdict(
            "pinned_assets",
            &problems,
            format!(
                "{} configured asset(s), {} active asset row(s)",
                config.assets.len(),
                rows.len()
            ),
        ))
    }

    /// No active source, asset or receiving collector belongs to another
    /// environment. A collector has no environment column of its own; it
    /// inherits its asset's.
    async fn environment_agreement(
        &self,
        config: &SelfCheckConfig,
    ) -> Result<SelfCheckResult, RepositoryError> {
        let expected = config.environment.as_str();
        let rows = sqlx::query(
            r"
            SELECT 'source ' || source_key AS name, chain_environment
              FROM chain_sources
             WHERE state = 'active' AND chain_environment <> $1
            UNION ALL
            SELECT 'asset ' || chain || ':' || network || ':' || display_symbol, chain_environment
              FROM chain_assets
             WHERE status = 'active' AND chain_environment <> $1
            UNION ALL
            SELECT 'collector ' || collector.address_text, asset.chain_environment
              FROM collector_addresses AS collector
              JOIN chain_assets AS asset ON asset.id = collector.asset_id
             WHERE collector.state IN ('active', 'receiving_only')
               AND asset.chain_environment <> $1
             ORDER BY 1
            ",
        )
        .bind(expected)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        let mut problems = Vec::with_capacity(rows.len());
        for row in &rows {
            let name: String = row.try_get("name").map_err(unavailable)?;
            let environment: String = row.try_get("chain_environment").map_err(unavailable)?;
            problems.push(format!("{name} is {environment}"));
        }
        Ok(verdict(
            "environment_agreement",
            &problems,
            format!("every active row is {expected}"),
        ))
    }

    /// Every active asset's chain and network has an active finality policy;
    /// without one the verifier could never call a transfer final.
    async fn finality_policy_present(&self) -> Result<SelfCheckResult, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT asset.chain || ':' || asset.network || ':' || asset.display_symbol AS name
              FROM chain_assets AS asset
             WHERE asset.status = 'active'
               AND NOT EXISTS (
                   SELECT 1
                     FROM chain_finality_policies AS policy
                    WHERE policy.chain = asset.chain
                      AND policy.network = asset.network
                      AND policy.chain_environment = asset.chain_environment
                      AND policy.status = 'active'
               )
             ORDER BY 1
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        let mut problems = Vec::with_capacity(rows.len());
        for row in &rows {
            let name: String = row.try_get("name").map_err(unavailable)?;
            problems.push(format!("asset {name} has no active finality policy"));
        }
        Ok(verdict(
            "finality_policy_present",
            &problems,
            "every active asset has an active finality policy".to_owned(),
        ))
    }

    /// No block cursor of an active source stands ahead of a head that source
    /// reported after the cursor last moved. The schema records no chain head
    /// of its own, so a source is measured against its own claim, and only a
    /// claim made after the cursor moved can contradict it: a scan that finds
    /// nothing advances the cursor past the head of an older reading, and
    /// that is a cursor doing its job, not a cursor written by something
    /// other than a scan of real blocks.
    async fn cursor_sanity(&self) -> Result<SelfCheckResult, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT source.source_key, cursor_row.cursor_value, head.source_head
              FROM chain_cursors AS cursor_row
              JOIN chain_sources AS source
                ON source.id = cursor_row.source_id AND source.state = 'active'
              JOIN LATERAL (
                    SELECT max(observation.source_head) AS source_head
                      FROM chain_observations AS observation
                     WHERE observation.source_id = cursor_row.source_id
                       AND observation.observed_at >= cursor_row.updated_at
              ) AS head ON true
             WHERE cursor_row.cursor_kind = 'block'
               AND cursor_row.cursor_value ~ '^[0-9]+$'
               AND head.source_head IS NOT NULL
               AND cursor_row.cursor_value::NUMERIC > head.source_head
             ORDER BY source.source_key
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        let mut problems = Vec::with_capacity(rows.len());
        for row in &rows {
            let source_key: String = row.try_get("source_key").map_err(unavailable)?;
            let cursor_value: String = row.try_get("cursor_value").map_err(unavailable)?;
            let source_head: i64 = row.try_get("source_head").map_err(unavailable)?;
            problems.push(format!(
                "source {source_key} cursor {cursor_value} is ahead of the head {source_head} it reported afterwards"
            ));
        }
        Ok(verdict(
            "cursor_sanity",
            &problems,
            "no block cursor is ahead of a head its source reported after the cursor moved"
                .to_owned(),
        ))
    }

    /// The database clock and the process clock agree within the configured
    /// bound. Leases, quote deadlines and evidence ages compare the two.
    async fn clock_skew(
        &self,
        config: &SelfCheckConfig,
        process_now: OffsetDateTime,
    ) -> Result<SelfCheckResult, RepositoryError> {
        let database_now: OffsetDateTime = sqlx::query_scalar("SELECT now()")
            .fetch_one(self.pool())
            .await
            .map_err(unavailable)?;
        let skew = (database_now - process_now).whole_seconds().abs();
        let limit = config.max_clock_skew_seconds;
        let problems = if skew < limit {
            Vec::new()
        } else {
            vec![format!(
                "PostgreSQL and the process disagree by {skew}s; the limit is below {limit}s"
            )]
        };
        Ok(verdict(
            "clock_skew",
            &problems,
            format!("clock skew is {skew}s, limit below {limit}s"),
        ))
    }
}

/// A passed check keeps its summary; a failed one lists what disagreed.
fn verdict(name: &str, problems: &[String], summary: String) -> SelfCheckResult {
    SelfCheckResult {
        name: name.to_owned(),
        passed: problems.is_empty(),
        detail: if problems.is_empty() {
            summary
        } else {
            problems.join("; ")
        },
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            // Writing into a String cannot fail; the Result is the trait's shape.
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests;
