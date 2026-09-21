use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::{
    AddressKey, ChainEnvironment, ObservationKind, ObservedTransfer, RawAmount, TxHash,
};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Clock, RepositoryError};

/// An immutable data source. Independence between sources is counted by
/// `provider_group`: two API keys of one provider are one source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainSource {
    pub id: Uuid,
    pub chain: String,
    pub network: String,
    pub chain_environment: ChainEnvironment,
    pub source_key: String,
    pub provider_group: String,
    pub kind: SourceKind,
    pub db_principal: String,
    pub state: SourceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    OwnNode,
    HostedRpc,
    IndexedApi,
}

impl SourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OwnNode => "own_node",
            Self::HostedRpc => "hosted_rpc",
            Self::IndexedApi => "indexed_api",
        }
    }

    /// Parses the stored text.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::UnknownSourceKind`] for unknown text.
    pub fn parse(value: &str) -> Result<Self, ObservationError> {
        match value {
            "own_node" => Ok(Self::OwnNode),
            "hosted_rpc" => Ok(Self::HostedRpc),
            "indexed_api" => Ok(Self::IndexedApi),
            other => Err(ObservationError::UnknownSourceKind(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    Active,
    Degraded,
    Disabled,
}

impl SourceState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Degraded => "degraded",
            Self::Disabled => "disabled",
        }
    }

    /// Parses the stored text.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::UnknownSourceState`] for unknown text.
    pub fn parse(value: &str) -> Result<Self, ObservationError> {
        match value {
            "active" => Ok(Self::Active),
            "degraded" => Ok(Self::Degraded),
            "disabled" => Ok(Self::Disabled),
            other => Err(ObservationError::UnknownSourceState(other.to_owned())),
        }
    }
}

/// One collector address an observer watches, with the asset allowlist entry
/// that makes a token acceptable on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectorWatch {
    pub collector_address_id: Uuid,
    pub address_key: AddressKey,
    pub address_text: String,
    pub asset_id: Uuid,
    pub token_key: AddressKey,
    pub decimals: i16,
    pub chain: String,
    pub network: String,
    pub chain_environment: ChainEnvironment,
    pub state: CollectorState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorState {
    Active,
    ReceivingOnly,
    Retired,
}

impl CollectorState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::ReceivingOnly => "receiving_only",
            Self::Retired => "retired",
        }
    }

    /// Parses the stored text.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::UnknownCollectorState`] for unknown text.
    pub fn parse(value: &str) -> Result<Self, ObservationError> {
        match value {
            "active" => Ok(Self::Active),
            "receiving_only" => Ok(Self::ReceivingOnly),
            "retired" => Ok(Self::Retired),
            other => Err(ObservationError::UnknownCollectorState(other.to_owned())),
        }
    }

    /// Reports whether payments are still expected on this address.
    ///
    /// A retired address must never be listened to as if it were live: money
    /// arriving there is an incident, not a payment.
    #[must_use]
    pub const fn is_watched(self) -> bool {
        matches!(self, Self::Active | Self::ReceivingOnly)
    }
}

/// A lease over a singleton component, with the fence token that makes a
/// frozen predecessor's writes refusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentLease {
    pub component: String,
    pub holder: String,
    pub fence_token: i64,
    pub lease_until: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    Block,
    LogicalTime,
    EventPosition,
}

impl CursorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::LogicalTime => "logical_time",
            Self::EventPosition => "event_position",
        }
    }

    #[must_use]
    pub const fn is_numeric(self) -> bool {
        matches!(self, Self::Block | Self::LogicalTime)
    }
}

/// Where a lane resumes reading, and under which fence token it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorPosition {
    pub kind: CursorKind,
    pub value: String,
    pub last_block_hash: Option<String>,
    pub fence_token: i64,
}

impl CursorPosition {
    /// Builds a cursor position, refusing values a chain could not produce.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::InvalidCursorValue`] for an empty, oversized
    /// or non-numeric value where the kind requires digits.
    pub fn new(
        kind: CursorKind,
        value: impl Into<String>,
        last_block_hash: Option<String>,
        fence_token: i64,
    ) -> Result<Self, ObservationError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 200
            && (!kind.is_numeric() || value.bytes().all(|byte| byte.is_ascii_digit()));
        if !valid {
            return Err(ObservationError::InvalidCursorValue);
        }
        if fence_token <= 0 {
            return Err(ObservationError::InvalidFenceToken);
        }
        Ok(Self {
            kind,
            value,
            last_block_hash,
            fence_token,
        })
    }
}

/// What one intake batch did. Every reading is accounted for: nothing is
/// dropped silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IntakeReport {
    pub recorded: u32,
    pub duplicates: u32,
    pub refused: u32,
    pub cursor_advanced: bool,
}

/// Why a reading was not recorded. Each variant is a counter and a log line,
/// never a silent drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// The reading belongs to a different chain environment than the source.
    ForeignEnvironment,
    /// The recipient is not a collector address this gateway watches.
    ForeignRecipient,
    /// The recipient is a retired collector address.
    RetiredCollector,
    /// The chain or network does not match the source's own chain.
    ForeignChain,
}

impl RefusalReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ForeignEnvironment => "foreign_environment",
            Self::ForeignRecipient => "foreign_recipient",
            Self::RetiredCollector => "retired_collector",
            Self::ForeignChain => "foreign_chain",
        }
    }
}

/// One reading, resolved against this gateway's allowlists, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedObservation {
    pub transfer: ObservedTransfer,
    pub kind: ObservationKind,
    pub collector_address_id: Uuid,
    /// `None` when the token is not on the allowlist. The reading is still
    /// evidence; it simply can never become a canonical transfer.
    pub asset_id: Option<Uuid>,
    pub semantic_hash: [u8; 32],
    pub observer_version: String,
    pub parser_version: String,
    pub fence_token: i64,
    pub observed_at: OffsetDateTime,
}

/// One page of a chain scan: what was read, where to resume, and how far the
/// source says the chain has advanced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPage {
    pub transfers: Vec<ObservedTransfer>,
    /// Where the next scan resumes. `None` means the source could not tell,
    /// so the cursor is left where it was rather than guessed forward.
    pub next_cursor: Option<CursorPosition>,
    pub head: Option<i64>,
}

/// A source's window onto one collector address.
///
/// The scanner reports what a provider claimed. It never decides that a
/// payment happened, and it never advances a cursor by itself.
#[async_trait]
pub trait ChainScanner: Send + Sync {
    /// Reads transfers to one collector address from the given position.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider could not be reached or answered
    /// with something this gateway cannot parse.
    async fn scan(
        &self,
        watch: &CollectorWatch,
        cursor: Option<&CursorPosition>,
        limit: u32,
    ) -> Result<ScanPage, ScanError>;
}

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("the chain source is unreachable: {0}")]
    Unreachable(String),
    #[error("the chain source answered with something unparseable: {0}")]
    Unparseable(String),
}

impl ScanError {
    /// Reports whether a retry can plausibly succeed without operator action.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Unreachable(_))
    }
}

#[async_trait]
pub trait ObservationRepository: Send + Sync {
    async fn find_source(&self, source_key: &str) -> Result<Option<ChainSource>, RepositoryError>;

    async fn watched_collectors(
        &self,
        chain: &str,
        network: &str,
        environment: ChainEnvironment,
    ) -> Result<Vec<CollectorWatch>, RepositoryError>;

    async fn find_cursor(
        &self,
        source_id: Uuid,
        kind: ObservationKind,
        collector_address_id: Uuid,
    ) -> Result<Option<CursorPosition>, RepositoryError>;

    /// Records readings and advances the lane cursor in one transaction, under
    /// the caller's fence token.
    async fn record_observations(
        &self,
        source: &ChainSource,
        lease: &ComponentLease,
        observations: &[ResolvedObservation],
        cursor: Option<(Uuid, ObservationKind, CursorPosition)>,
    ) -> Result<IntakeReport, RepositoryError>;
}

/// Turns raw readings into stored evidence.
///
/// The service owns the rules that decide whether a reading is about this
/// gateway at all. It never decides that a payment happened.
#[derive(Debug)]
pub struct ObservationService<R, C> {
    repository: Arc<R>,
    clock: C,
    observer_version: String,
    parser_version: String,
}

impl<R, C> ObservationService<R, C>
where
    R: ObservationRepository,
    C: Clock,
{
    pub fn new(
        repository: Arc<R>,
        clock: C,
        observer_version: impl Into<String>,
        parser_version: impl Into<String>,
    ) -> Self {
        Self {
            repository,
            clock,
            observer_version: observer_version.into(),
            parser_version: parser_version.into(),
        }
    }

    /// Resolves readings against the watched collectors and the asset
    /// allowlist, then stores what belongs to this gateway.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError`] when storage fails or the component lease
    /// was taken over while the batch was being prepared.
    pub async fn intake(
        &self,
        source: &ChainSource,
        lease: &ComponentLease,
        kind: ObservationKind,
        collectors: &[CollectorWatch],
        transfers: Vec<ObservedTransfer>,
        cursor: Option<(Uuid, CursorPosition)>,
    ) -> Result<(IntakeReport, Vec<(ObservedTransfer, RefusalReason)>), ObservationError> {
        let mut resolved = Vec::with_capacity(transfers.len());
        let mut refused = Vec::new();
        let now = self.clock.now();

        for transfer in transfers {
            match resolve(source, collectors, &transfer) {
                Ok((collector, asset_id)) => {
                    let semantic_hash = transfer.semantic_hash(source.id, kind);
                    resolved.push(ResolvedObservation {
                        transfer,
                        kind,
                        collector_address_id: collector,
                        asset_id,
                        semantic_hash,
                        observer_version: self.observer_version.clone(),
                        parser_version: self.parser_version.clone(),
                        fence_token: lease.fence_token,
                        observed_at: now,
                    });
                }
                Err(reason) => refused.push((transfer, reason)),
            }
        }

        let cursor = cursor.map(|(collector, position)| (collector, kind, position));
        let report = self
            .repository
            .record_observations(source, lease, &resolved, cursor)
            .await?;
        Ok((
            IntakeReport {
                refused: u32::try_from(refused.len()).unwrap_or(u32::MAX),
                ..report
            },
            refused,
        ))
    }
}

fn resolve(
    source: &ChainSource,
    collectors: &[CollectorWatch],
    transfer: &ObservedTransfer,
) -> Result<(Uuid, Option<Uuid>), RefusalReason> {
    if transfer.chain != source.chain || transfer.network != source.network {
        return Err(RefusalReason::ForeignChain);
    }
    if transfer.chain_environment != source.chain_environment {
        return Err(RefusalReason::ForeignEnvironment);
    }
    let watched: Vec<&CollectorWatch> = collectors
        .iter()
        .filter(|collector| collector.address_key == transfer.to_address)
        .collect();
    let Some(collector) = watched.first() else {
        return Err(RefusalReason::ForeignRecipient);
    };
    if !collector.state.is_watched() {
        return Err(RefusalReason::RetiredCollector);
    }
    // The token is identified by its canonical contract bytes. A token that
    // merely calls itself USDT resolves to no asset and can never become a
    // canonical transfer, while the reading is still kept as evidence.
    let asset_id = watched
        .iter()
        .find(|candidate| candidate.token_key == transfer.token_key)
        .map(|candidate| candidate.asset_id);
    Ok((collector.collector_address_id, asset_id))
}

/// Helper for adapters: the raw amount a chain reported as digits.
///
/// # Errors
///
/// Returns [`ObservationError::InvalidAmount`] when the text is not a positive
/// base-10 integer.
pub fn parse_raw_amount(value: &str) -> Result<RawAmount, ObservationError> {
    value
        .parse::<RawAmount>()
        .map_err(|_| ObservationError::InvalidAmount)
}

/// Helper for adapters: a normalized transaction hash.
///
/// # Errors
///
/// Returns [`ObservationError::InvalidTxHash`] for a value no chain produces.
pub fn parse_tx_hash(value: &str) -> Result<TxHash, ObservationError> {
    TxHash::new(value).map_err(|_| ObservationError::InvalidTxHash)
}

#[derive(Debug, Error)]
pub enum ObservationError {
    #[error("unknown source kind: {0}")]
    UnknownSourceKind(String),
    #[error("unknown source state: {0}")]
    UnknownSourceState(String),
    #[error("unknown collector state: {0}")]
    UnknownCollectorState(String),
    #[error("a cursor value must be a short chain position")]
    InvalidCursorValue,
    #[error("a fence token must be positive")]
    InvalidFenceToken,
    #[error("an amount must be a positive base-10 integer")]
    InvalidAmount,
    #[error("a transaction hash must be a short alphanumeric identifier")]
    InvalidTxHash,
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

impl ObservationError {
    /// Reports whether a retry can plausibly succeed without operator action.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Repository(error) => error.is_transient(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
