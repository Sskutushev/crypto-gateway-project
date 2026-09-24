//! Fail-closed checks that prove a process is attached to the intended rails.

use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::{AddressKey, ChainEnvironment};
use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;

use crate::{Clock, RepositoryError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedAsset {
    pub chain: String,
    pub network: String,
    pub contract: AddressKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfCheckConfig {
    pub collectors: Vec<AddressKey>,
    pub assets: Vec<ExpectedAsset>,
    pub environment: ChainEnvironment,
    pub max_clock_skew_seconds: i64,
}

impl SelfCheckConfig {
    /// Parses deployment expectations while delegating chain-specific address
    /// decoding to the binary, which owns the chain adapter dependency.
    ///
    /// # Errors
    ///
    /// Returns [`SelfCheckConfigError`] when an allowlist is empty, malformed,
    /// duplicated, or names an unsupported environment or skew bound.
    pub fn parse<F, E>(
        collectors: &str,
        assets: &str,
        environment: &str,
        max_clock_skew_seconds: &str,
        parse_address: F,
    ) -> Result<Self, SelfCheckConfigError>
    where
        F: Fn(&str) -> Result<AddressKey, E>,
    {
        let environment = environment
            .parse()
            .map_err(|_| SelfCheckConfigError::Environment)?;
        let max_clock_skew_seconds = max_clock_skew_seconds
            .parse::<i64>()
            .map_err(|_| SelfCheckConfigError::ClockSkew)?;
        if max_clock_skew_seconds <= 0 {
            return Err(SelfCheckConfigError::ClockSkew);
        }
        let mut parsed_collectors = Vec::new();
        for value in values(collectors) {
            let key = parse_address(value)
                .map_err(|_| SelfCheckConfigError::Address(value.to_owned()))?;
            if parsed_collectors.contains(&key) {
                return Err(SelfCheckConfigError::Duplicate(value.to_owned()));
            }
            parsed_collectors.push(key);
        }
        if parsed_collectors.is_empty() {
            return Err(SelfCheckConfigError::Empty("GATEWAY_EXPECTED_COLLECTORS"));
        }
        let mut parsed_assets = Vec::new();
        for value in values(assets) {
            let mut parts = value.split(':');
            let (Some(chain), Some(network), Some(contract), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(SelfCheckConfigError::Asset(value.to_owned()));
            };
            let contract = parse_address(contract)
                .map_err(|_| SelfCheckConfigError::Address(value.to_owned()))?;
            let asset = ExpectedAsset {
                chain: chain.to_owned(),
                network: network.to_owned(),
                contract,
            };
            if parsed_assets.contains(&asset) {
                return Err(SelfCheckConfigError::Duplicate(value.to_owned()));
            }
            parsed_assets.push(asset);
        }
        if parsed_assets.is_empty() {
            return Err(SelfCheckConfigError::Empty("GATEWAY_EXPECTED_ASSETS"));
        }
        Ok(Self {
            collectors: parsed_collectors,
            assets: parsed_assets,
            environment,
            max_clock_skew_seconds,
        })
    }
}

fn values(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SelfCheckConfigError {
    #[error("{0} must name at least one value")]
    Empty(&'static str),
    #[error("invalid canonical TRON address in {0}")]
    Address(String),
    #[error("GATEWAY_EXPECTED_ASSETS entries must be chain:network:contract, not {0}")]
    Asset(String),
    #[error("duplicate self-check entry {0}")]
    Duplicate(String),
    #[error("GATEWAY_CHAIN_ENVIRONMENT must be testnet or mainnet")]
    Environment,
    #[error("GATEWAY_MAX_CLOCK_SKEW_SECONDS must be a positive whole number")]
    ClockSkew,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelfCheckResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelfCheckReport {
    #[serde(skip_serializing)]
    pub evaluated_at: OffsetDateTime,
    pub checks: Vec<SelfCheckResult>,
    pub passed: bool,
}

impl SelfCheckReport {
    #[must_use]
    pub fn new(evaluated_at: OffsetDateTime, checks: Vec<SelfCheckResult>) -> Self {
        // The summary is derived rather than supplied so a caller cannot mark
        // a report ready while retaining a failed row in its evidence.
        let passed = checks.iter().all(|check| check.passed);
        Self {
            evaluated_at,
            checks,
            passed,
        }
    }
}

#[async_trait]
pub trait SelfCheckRepository: Send + Sync {
    async fn self_check(
        &self,
        config: &SelfCheckConfig,
        process_now: OffsetDateTime,
    ) -> Result<Vec<SelfCheckResult>, RepositoryError>;
}

#[derive(Debug)]
pub struct SelfCheckService<R, C> {
    repository: Arc<R>,
    clock: C,
    config: SelfCheckConfig,
}

impl<R, C> SelfCheckService<R, C>
where
    R: SelfCheckRepository,
    C: Clock,
{
    pub const fn new(repository: Arc<R>, clock: C, config: SelfCheckConfig) -> Self {
        Self {
            repository,
            clock,
            config,
        }
    }

    /// Evaluates every invariant; a storage error is returned, never converted
    /// into a successful or stale readiness answer.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError`] when all checks could not be evaluated
    /// against one reachable database.
    pub async fn run(&self) -> Result<SelfCheckReport, RepositoryError> {
        let evaluated_at = self.clock.now();
        let checks = self
            .repository
            .self_check(&self.config, evaluated_at)
            .await?;
        Ok(SelfCheckReport::new(evaluated_at, checks))
    }
}

#[cfg(test)]
mod tests {
    use super::{SelfCheckConfig, SelfCheckConfigError, SelfCheckReport, SelfCheckResult};
    use gateway_domain::AddressKey;
    use time::OffsetDateTime;

    #[test]
    fn report_passed_is_the_conjunction_of_its_rows() {
        let row = |name: &str, passed| SelfCheckResult {
            name: name.to_owned(),
            passed,
            detail: String::new(),
        };
        assert!(SelfCheckReport::new(OffsetDateTime::UNIX_EPOCH, vec![row("a", true)]).passed);
        assert!(
            !SelfCheckReport::new(
                OffsetDateTime::UNIX_EPOCH,
                vec![row("a", true), row("b", false)],
            )
            .passed
        );
    }

    fn address(value: &str) -> Result<AddressKey, ()> {
        if value == "bad" {
            Err(())
        } else {
            AddressKey::new(value.as_bytes().to_vec()).map_err(|_| ())
        }
    }

    #[test]
    fn configuration_refuses_empty_malformed_duplicate_and_unknown_environment() {
        assert!(matches!(
            SelfCheckConfig::parse("", "tron:n:a", "testnet", "5", address),
            Err(SelfCheckConfigError::Empty(_))
        ));
        assert!(matches!(
            SelfCheckConfig::parse("bad", "tron:n:a", "testnet", "5", address),
            Err(SelfCheckConfigError::Address(_))
        ));
        assert!(matches!(
            SelfCheckConfig::parse("a,a", "tron:n:a", "testnet", "5", address),
            Err(SelfCheckConfigError::Duplicate(_))
        ));
        assert!(matches!(
            SelfCheckConfig::parse("a", "tron:n:a", "staging", "5", address),
            Err(SelfCheckConfigError::Environment)
        ));
    }
}
