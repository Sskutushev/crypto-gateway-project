//! Worker configuration.
//!
//! Every value is read once, at startup, and an unreadable one stops the
//! process. A worker that quietly falls back to a default runs a different
//! schedule than the operator configured, and nobody finds out until the
//! numbers disagree.

use std::{env, time::Duration};

use anyhow::{Context, Result, bail};
use gateway_domain::ChainEnvironment;
use gateway_scheduler::{BatchConfig, RetryPolicy};
use gateway_tron::ScanLane;

/// Which loops this process runs. One image, several deployments: an observer
/// with only RPC access and an observer role, a verifier with its own role, a
/// payment worker with the money role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Expiry,
    Observer,
    Verifier,
    Settlement,
    Outbox,
    Reconciler,
}

impl Role {
    pub const ALL: [Self; 6] = [
        Self::Expiry,
        Self::Observer,
        Self::Verifier,
        Self::Settlement,
        Self::Outbox,
        Self::Reconciler,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expiry => "expiry",
            Self::Observer => "observer",
            Self::Verifier => "verifier",
            Self::Settlement => "settlement",
            Self::Outbox => "outbox",
            Self::Reconciler => "reconciler",
        }
    }

    /// The prefix its settings are read from.
    const fn prefix(self) -> &'static str {
        match self {
            Self::Expiry => "GATEWAY_EXPIRY",
            Self::Observer => "GATEWAY_OBSERVER",
            Self::Verifier => "GATEWAY_VERIFIER",
            Self::Settlement => "GATEWAY_SETTLEMENT",
            Self::Outbox => "GATEWAY_OUTBOX",
            Self::Reconciler => "GATEWAY_RECONCILER",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == value)
            .with_context(|| {
                let names = Self::ALL.map(Self::as_str).join(", ");
                format!("unknown worker role \"{value}\"; known roles are {names}")
            })
    }
}

/// Everything this process needs to start.
#[derive(Debug, Clone)]
pub struct WorkerSettings {
    pub database_url: String,
    pub instance: String,
    pub roles: Vec<Role>,
    pub chain: String,
    pub network: String,
    pub chain_environment: ChainEnvironment,
    pub observer: Option<ObserverSettings>,
    pub verifier: Option<VerifierSettings>,
    pub outbox: Option<OutboxSettings>,
    pub batches: Vec<(Role, BatchConfig)>,
}

/// One provider endpoint. The verifier reads through its own, so its re-read
/// is independent of the observer whose claim it is checking.
#[derive(Debug, Clone)]
pub struct TronEndpoint {
    pub base_url: String,
    pub api_key: Option<String>,
    pub api_key_header: String,
    pub request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ObserverSettings {
    /// The `chain_sources` row this process speaks as. It must already exist:
    /// a source that is not registered has no provider group, so no reading it
    /// makes could ever count as independent evidence.
    pub source_key: String,
    pub endpoint: TronEndpoint,
    pub lane: ScanLane,
    pub max_blocks_per_scan: u32,
    pub bootstrap_lookback_blocks: u32,
}

#[derive(Debug, Clone)]
pub struct VerifierSettings {
    pub source_key: String,
    pub endpoint: TronEndpoint,
}

#[derive(Debug, Clone)]
pub struct OutboxSettings {
    /// The deployment master key, hex encoded. Endpoint secrets are derived
    /// from it and never stored, so the database alone cannot forge an event.
    pub master_key: Vec<u8>,
    pub request_timeout: Duration,
    pub max_attempts: i32,
}

impl WorkerSettings {
    /// Reads the process configuration from the environment.
    ///
    /// # Errors
    ///
    /// Returns an error when a required value is missing or a present value is
    /// not the kind of value it must be.
    pub fn from_env() -> Result<Self> {
        let database_url = required("GATEWAY_DATABASE_URL")?;
        let roles = roles()?;
        let chain = read("GATEWAY_CHAIN").unwrap_or_else(|| "tron".to_owned());
        let network = read("GATEWAY_NETWORK").unwrap_or_else(|| "mainnet".to_owned());
        let chain_environment = chain_environment()?;
        let instance = instance_identity();

        let observer = if roles.contains(&Role::Observer) {
            Some(observer_settings()?)
        } else {
            None
        };
        let verifier = if roles.contains(&Role::Verifier) {
            Some(VerifierSettings {
                source_key: required("GATEWAY_VERIFIER_SOURCE_KEY")?,
                endpoint: endpoint("GATEWAY_VERIFIER_TRON")?,
            })
        } else {
            None
        };
        let outbox = if roles.contains(&Role::Outbox) {
            Some(outbox_settings()?)
        } else {
            None
        };

        let mut batches = Vec::with_capacity(roles.len());
        for role in &roles {
            batches.push((*role, batch_config(role.prefix())?));
        }

        Ok(Self {
            database_url,
            instance,
            roles,
            chain,
            network,
            chain_environment,
            observer,
            verifier,
            outbox,
            batches,
        })
    }

    #[must_use]
    pub fn batch(&self, role: Role) -> Option<&BatchConfig> {
        self.batches
            .iter()
            .find(|(candidate, _)| *candidate == role)
            .map(|(_, config)| config)
    }
}

fn roles() -> Result<Vec<Role>> {
    let raw = required("GATEWAY_WORKER_ROLES")?;
    let mut roles = Vec::new();
    for name in raw.split(',') {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let role = Role::parse(name)?;
        if !roles.contains(&role) {
            roles.push(role);
        }
    }
    if roles.is_empty() {
        bail!("GATEWAY_WORKER_ROLES names no role to run");
    }
    Ok(roles)
}

fn observer_settings() -> Result<ObserverSettings> {
    Ok(ObserverSettings {
        source_key: required("GATEWAY_OBSERVER_SOURCE_KEY")?,
        endpoint: endpoint("GATEWAY_TRON")?,
        lane: lane()?,
        max_blocks_per_scan: count("GATEWAY_TRON_MAX_BLOCKS_PER_SCAN", 50)?,
        bootstrap_lookback_blocks: count("GATEWAY_TRON_BOOTSTRAP_LOOKBACK_BLOCKS", 1_200)?,
    })
}

fn endpoint(prefix: &str) -> Result<TronEndpoint> {
    Ok(TronEndpoint {
        base_url: required(&format!("{prefix}_BASE_URL"))?,
        api_key: read(&format!("{prefix}_API_KEY")),
        api_key_header: read(&format!("{prefix}_API_KEY_HEADER"))
            .unwrap_or_else(|| "TRON-PRO-API-KEY".to_owned()),
        request_timeout: seconds(&format!("{prefix}_REQUEST_TIMEOUT_SECONDS"), 15)?,
    })
}

fn outbox_settings() -> Result<OutboxSettings> {
    let raw = required("GATEWAY_WEBHOOK_MASTER_KEY")?;
    let master_key = decode_hex(&raw).context(
        "GATEWAY_WEBHOOK_MASTER_KEY must be hex encoded, for example from `openssl rand -hex 32`",
    )?;
    if master_key.len() < 32 {
        bail!("GATEWAY_WEBHOOK_MASTER_KEY must carry at least 32 bytes");
    }
    Ok(OutboxSettings {
        master_key,
        request_timeout: seconds("GATEWAY_WEBHOOK_REQUEST_TIMEOUT_SECONDS", 10)?,
        max_attempts: i32::try_from(count("GATEWAY_WEBHOOK_MAX_ATTEMPTS", 12)?)
            .context("GATEWAY_WEBHOOK_MAX_ATTEMPTS is too large")?,
    })
}

fn batch_config(prefix: &str) -> Result<BatchConfig> {
    let defaults = BatchConfig::default();
    let config = BatchConfig {
        interval: seconds(
            &format!("{prefix}_INTERVAL_SECONDS"),
            defaults.interval.as_secs(),
        )?,
        batch_limit: count(&format!("{prefix}_BATCH_LIMIT"), defaults.batch_limit)?,
        max_batches_per_tick: count(
            &format!("{prefix}_MAX_BATCHES_PER_TICK"),
            defaults.max_batches_per_tick,
        )?,
        lease_seconds: i64::from(count(
            &format!("{prefix}_LEASE_SECONDS"),
            u32::try_from(defaults.lease_seconds).unwrap_or(120),
        )?),
        retry: RetryPolicy {
            max_attempts: count(
                &format!("{prefix}_RETRY_ATTEMPTS"),
                defaults.retry.max_attempts,
            )?,
            initial_backoff: seconds(
                &format!("{prefix}_RETRY_INITIAL_BACKOFF_SECONDS"),
                defaults.retry.initial_backoff.as_secs(),
            )?,
            max_backoff: seconds(
                &format!("{prefix}_RETRY_MAX_BACKOFF_SECONDS"),
                defaults.retry.max_backoff.as_secs(),
            )?,
        },
    };
    config
        .validated()
        .with_context(|| format!("{prefix}_* settings describe an unsafe schedule"))
}

fn lane() -> Result<ScanLane> {
    match read("GATEWAY_TRON_LANE").as_deref() {
        None | Some("block_range") => Ok(ScanLane::BlockRange),
        Some("address_index") => Ok(ScanLane::AddressIndex),
        Some(other) => bail!("GATEWAY_TRON_LANE must be block_range or address_index, not {other}"),
    }
}

fn chain_environment() -> Result<ChainEnvironment> {
    match read("GATEWAY_CHAIN_ENVIRONMENT").as_deref() {
        // Mainnet is never the default. A process that was not told which world
        // it lives in must not guess the one where the money is real.
        Some("mainnet") => Ok(ChainEnvironment::Mainnet),
        Some("testnet") => Ok(ChainEnvironment::Testnet),
        Some(other) => bail!("GATEWAY_CHAIN_ENVIRONMENT must be mainnet or testnet, not {other}"),
        None => bail!("GATEWAY_CHAIN_ENVIRONMENT is required and must be mainnet or testnet"),
    }
}

/// The identity a lease is held under.
///
/// The boot component matters: a restarted process with the same host name
/// must not inherit the lease its predecessor was still writing under.
fn instance_identity() -> String {
    let host = read("GATEWAY_INSTANCE_NAME")
        .or_else(|| read("HOSTNAME"))
        .unwrap_or_else(|| "gateway-worker".to_owned());
    format!("{host}:{}", uuid::Uuid::now_v7())
}

fn read(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn required(name: &str) -> Result<String> {
    read(name).with_context(|| format!("{name} is required"))
}

fn seconds(name: &str, default: u64) -> Result<Duration> {
    Ok(Duration::from_secs(number(name, default)?))
}

fn count(name: &str, default: u32) -> Result<u32> {
    let value = number(name, u64::from(default))?;
    u32::try_from(value).with_context(|| format!("{name} is too large"))
}

fn number(name: &str, default: u64) -> Result<u64> {
    match read(name) {
        None => Ok(default),
        Some(value) => value
            .parse::<u64>()
            .with_context(|| format!("{name} must be a whole number")),
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    let trimmed = value.trim();
    if !trimmed.len().is_multiple_of(2) {
        bail!("a hex value has an even number of characters");
    }
    let mut bytes = Vec::with_capacity(trimmed.len() / 2);
    for pair in trimmed.as_bytes().chunks_exact(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("a hex value contains only 0-9 and a-f"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Role, decode_hex};

    #[test]
    fn roles_are_named_exactly_or_refused() -> anyhow::Result<()> {
        assert_eq!(Role::parse("observer")?, Role::Observer);
        assert_eq!(Role::parse("outbox")?, Role::Outbox);
        assert!(Role::parse("Observer").is_err());
        assert!(Role::parse("everything").is_err());
        Ok(())
    }

    #[test]
    fn a_master_key_that_is_not_hex_stops_startup() -> anyhow::Result<()> {
        assert!(decode_hex("zz").is_err());
        assert!(decode_hex("abc").is_err());
        assert_eq!(decode_hex("0a0b")?.len(), 2);
        Ok(())
    }
}
