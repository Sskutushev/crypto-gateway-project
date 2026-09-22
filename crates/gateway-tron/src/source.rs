//! The TRON HTTP source.
//!
//! One type implements both windows onto the chain: the observer's
//! [`ChainScanner`] and the verifier's [`ChainReader`]. Both end in the same
//! place — a transaction's event log — because that is the only answer that
//! carries the event index a canonical fact is identified by.
//!
//! Two lanes exist and they are not interchangeable. The block lane walks
//! solidified blocks under a durable cursor and is the reading a payment may
//! rest on. The address lane asks a provider's index which transactions touched
//! a collector and then reads those transactions' logs; it detects sooner and
//! is never more than a hint about where to look.
//!
//! Nothing here retries. A provider that fails says so, and the worker above
//! decides, so a hidden retry inside a client can never turn one reading into
//! two or hide an outage from the operator.

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use gateway_application::{
    ChainEventKey, ChainReader, ChainReaderError, ChainScanner, CollectorWatch, CursorKind,
    CursorPosition, ScanError, ScanPage,
};
use gateway_domain::{AddressKey, ChainEnvironment, ObservedTransfer, TxHash};
use reqwest::{Client, header::HeaderName};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::{
    address::to_base58,
    event::{
        BlockRef, ChainContext, HeadState, TokenView, TransactionInfo, block_time, parse_transfers,
        to_observation,
    },
};

/// The fence token a scanner writes into the cursor it proposes.
///
/// The observer replaces it with the token of the lease it actually holds; a
/// scanner has no lease and must not pretend to.
const PROPOSED_FENCE_TOKEN: i64 = 1;

/// Which window onto the chain a source reads through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanLane {
    /// Walk solidified blocks under a durable cursor. The canonical reading.
    BlockRange,
    /// Ask the provider's address index which transactions to read. Faster to
    /// detect, and never authoritative on its own.
    AddressIndex,
}

impl ScanLane {
    #[must_use]
    pub const fn cursor_kind(self) -> CursorKind {
        match self {
            Self::BlockRange => CursorKind::Block,
            Self::AddressIndex => CursorKind::LogicalTime,
        }
    }
}

/// Everything a TRON source needs to know about itself.
#[derive(Debug, Clone)]
pub struct TronSourceConfig {
    /// A node or provider base URL, for example `https://api.trongrid.io` or
    /// `http://tron-node.internal:8090`.
    pub base_url: String,
    /// The provider's API key header value, when it requires one.
    pub api_key: Option<String>,
    /// The header the key travels in. Providers disagree about its name.
    pub api_key_header: String,
    pub chain: String,
    pub network: String,
    pub chain_environment: ChainEnvironment,
    pub lane: ScanLane,
    pub request_timeout: Duration,
    /// How many blocks one scan may walk. Bounded work per batch, so a long
    /// outage cannot turn into one unbounded catch-up request.
    pub max_blocks_per_scan: u32,
    /// Where a lane starts when it has no cursor yet, counted back from the
    /// solidified head. An explicit decision, logged when it is taken.
    pub bootstrap_lookback_blocks: u32,
    /// The allowlisted tokens this source knows how to name. A contract that is
    /// not here is still read and stored; it is simply described by its own
    /// address, because naming an unknown token is how a fake one gets trusted.
    pub tokens: Vec<TokenView>,
    pub user_agent: String,
}

impl TronSourceConfig {
    fn context(&self) -> ChainContext {
        ChainContext {
            chain: self.chain.clone(),
            network: self.network.clone(),
            chain_environment: self.chain_environment,
        }
    }

    fn token_for(&self, contract: &AddressKey) -> Option<&TokenView> {
        self.tokens
            .iter()
            .find(|token| &token.token_key == contract)
    }
}

/// One request against a TRON HTTP endpoint.
///
/// The trait exists so every answer this gateway can receive — including the
/// ones a hostile or broken provider sends — is reachable from a test without
/// a network.
#[async_trait]
pub trait TronTransport: Send + Sync {
    /// Posts a JSON body and returns the raw answer.
    ///
    /// # Errors
    ///
    /// Returns [`TronSourceError`] when the provider could not be reached or
    /// refused the request.
    async fn post(&self, path: &str, body: serde_json::Value) -> Result<String, TronSourceError>;

    /// Performs a GET and returns the raw answer.
    ///
    /// # Errors
    ///
    /// Returns [`TronSourceError`] when the provider could not be reached or
    /// refused the request.
    async fn get(&self, path_and_query: &str) -> Result<String, TronSourceError>;
}

/// One transport serves both the scanner and the reader of a deployment, so it
/// is shared rather than duplicated.
#[async_trait]
impl<T> TronTransport for Arc<T>
where
    T: TronTransport,
{
    async fn post(&self, path: &str, body: serde_json::Value) -> Result<String, TronSourceError> {
        self.as_ref().post(path, body).await
    }

    async fn get(&self, path_and_query: &str) -> Result<String, TronSourceError> {
        self.as_ref().get(path_and_query).await
    }
}

/// The production transport: HTTPS, explicit timeouts, no redirects, no retry.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: Client,
    base_url: String,
    api_key: Option<(HeaderName, String)>,
}

impl ReqwestTransport {
    /// Builds a transport for one provider.
    ///
    /// # Errors
    ///
    /// Returns [`TronSourceError::Configuration`] when the HTTP client cannot
    /// be built or the key header name is not a header name.
    pub fn new(config: &TronSourceConfig) -> Result<Self, TronSourceError> {
        let client = Client::builder()
            .timeout(config.request_timeout)
            .connect_timeout(config.request_timeout.min(Duration::from_secs(10)))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(config.user_agent.clone())
            .build()
            .map_err(|error| TronSourceError::Configuration(error.to_string()))?;
        let api_key = match config.api_key.as_ref() {
            Some(key) => {
                let name = HeaderName::from_bytes(config.api_key_header.as_bytes())
                    .map_err(|error| TronSourceError::Configuration(error.to_string()))?;
                Some((name, key.clone()))
            }
            None => None,
        };
        Ok(Self {
            client,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            api_key,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }
}

#[async_trait]
impl TronTransport for ReqwestTransport {
    async fn post(&self, path: &str, body: serde_json::Value) -> Result<String, TronSourceError> {
        let mut request = self.client.post(self.url(path)).json(&body);
        if let Some((name, value)) = self.api_key.as_ref() {
            request = request.header(name.clone(), value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| TronSourceError::Unreachable(error.to_string()))?;
        read_body(response).await
    }

    async fn get(&self, path_and_query: &str) -> Result<String, TronSourceError> {
        let mut request = self.client.get(self.url(path_and_query));
        if let Some((name, value)) = self.api_key.as_ref() {
            request = request.header(name.clone(), value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| TronSourceError::Unreachable(error.to_string()))?;
        read_body(response).await
    }
}

async fn read_body(response: reqwest::Response) -> Result<String, TronSourceError> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| TronSourceError::Unreachable(error.to_string()))?;
    if status.is_success() {
        return Ok(body);
    }
    // A rate limit or a provider outage is an outage, not a chain state. It is
    // reported as such so the worker can retry and the operator can see it.
    Err(TronSourceError::Unreachable(format!(
        "provider answered {status}: {}",
        truncate(&body)
    )))
}

fn truncate(body: &str) -> String {
    body.chars().take(300).collect()
}

/// A source's window onto TRON.
#[derive(Debug)]
pub struct TronHttpSource<T> {
    transport: T,
    config: TronSourceConfig,
}

impl<T> TronHttpSource<T>
where
    T: TronTransport,
{
    #[must_use]
    pub const fn new(transport: T, config: TronSourceConfig) -> Self {
        Self { transport, config }
    }

    /// Reads both heads the chain reports.
    ///
    /// # Errors
    ///
    /// Returns [`TronSourceError`] when either head is unreadable. A source
    /// that cannot say how far the chain has advanced cannot say how final
    /// anything is, so nothing is read without both.
    pub async fn heads(&self) -> Result<HeadState, TronSourceError> {
        let latest = self.transport.post("wallet/getnowblock", json!({})).await?;
        let solid = self
            .transport
            .post("walletsolidity/getnowblock", json!({}))
            .await?;
        let latest: BlockAnswer = parse(&latest)?;
        let solid: BlockAnswer = parse(&solid)?;
        Ok(HeadState {
            head: latest.number().ok_or_else(missing_head)?,
            solid_head: solid.number().ok_or_else(missing_head)?,
        })
    }

    async fn block(&self, number: i64) -> Result<BlockRef, TronSourceError> {
        let body = self
            .transport
            .post("wallet/getblockbynum", json!({ "num": number }))
            .await?;
        let answer: BlockAnswer = parse(&body)?;
        answer.into_block_ref()
    }

    async fn block_transactions(
        &self,
        number: i64,
    ) -> Result<(String, Vec<TransactionInfo>), TronSourceError> {
        let body = self
            .transport
            .post(
                "wallet/gettransactioninfobyblocknum",
                json!({ "num": number }),
            )
            .await?;
        let infos: Vec<TransactionInfo> = parse(&body)?;
        Ok((body, infos))
    }

    async fn transaction(
        &self,
        tx_hash: &TxHash,
    ) -> Result<Option<(String, TransactionInfo)>, TronSourceError> {
        let body = self
            .transport
            .post(
                "wallet/gettransactioninfobyid",
                json!({ "value": tx_hash.as_str() }),
            )
            .await?;
        let info: TransactionInfo = parse(&body)?;
        if info.id.is_empty() {
            // The chain does not know this transaction. That is an answer, not
            // an error, and the verifier treats it as evidence.
            return Ok(None);
        }
        Ok(Some((body, info)))
    }

    /// Walks solidified blocks and reads every transfer to the watched address.
    ///
    /// Only solidified blocks are walked, so a cursor never advances past a
    /// block the chain may still replace. Earlier detection is the address
    /// lane's job.
    async fn scan_blocks(
        &self,
        watch: &CollectorWatch,
        cursor: Option<&CursorPosition>,
        limit: u32,
    ) -> Result<ScanPage, TronSourceError> {
        let head = self.heads().await?;
        let from = if let Some(position) = cursor {
            position
                .value
                .parse::<i64>()
                .map_err(|_| TronSourceError::Unparseable("cursor is not a block number".into()))?
                .saturating_add(1)
        } else {
            let start = head
                .solid_head
                .saturating_sub(i64::from(self.config.bootstrap_lookback_blocks));
            debug!(
                start,
                solid_head = head.solid_head,
                "no cursor yet: starting the block lane from the configured lookback"
            );
            start.max(0)
        };
        if from > head.solid_head {
            // Nothing new is irreversible yet. The cursor stays where it is.
            return Ok(ScanPage {
                transfers: Vec::new(),
                next_cursor: None,
                head: Some(head.solid_head),
            });
        }
        let last = from
            .saturating_add(i64::from(self.config.max_blocks_per_scan).saturating_sub(1))
            .min(head.solid_head);

        let mut transfers = Vec::new();
        let mut scanned_to = None;
        for number in from..=last {
            let block = self.block(number).await?;
            let (body, infos) = self.block_transactions(number).await?;
            let evidence = sha256_hex(&body);
            for info in &infos {
                transfers.extend(self.readings(info, &block, head, watch, &evidence)?);
            }
            scanned_to = Some((number, block.hash));
            if u32::try_from(transfers.len()).unwrap_or(u32::MAX) >= limit {
                // Whole blocks only: stopping inside a block would either lose
                // readings or advance the cursor past unread ones.
                break;
            }
        }

        let next_cursor = match scanned_to {
            Some((number, hash)) => Some(
                CursorPosition::new(
                    CursorKind::Block,
                    number.to_string(),
                    Some(hash),
                    PROPOSED_FENCE_TOKEN,
                )
                .map_err(|error| TronSourceError::Unparseable(error.to_string()))?,
            ),
            None => None,
        };
        Ok(ScanPage {
            transfers,
            next_cursor,
            head: Some(head.solid_head),
        })
    }

    /// Asks the provider's address index which transactions touched the
    /// collector, then reads each of those transactions' logs.
    ///
    /// The index answer is never turned into a reading directly: it carries no
    /// event index, and two transfers inside one transaction would collapse
    /// into one fact.
    async fn scan_address_index(
        &self,
        watch: &CollectorWatch,
        cursor: Option<&CursorPosition>,
        limit: u32,
    ) -> Result<ScanPage, TronSourceError> {
        let head = self.heads().await?;
        let address = to_base58(&watch.address_key)
            .map_err(|error| TronSourceError::Unparseable(error.to_string()))?;
        let min_timestamp = match cursor {
            Some(position) => position.value.parse::<i64>().map_err(|_| {
                TronSourceError::Unparseable("cursor is not a millisecond timestamp".into())
            })?,
            None => 0,
        };
        let query = format!(
            "v1/accounts/{address}/transactions/trc20?only_to=true&limit={limit}&min_timestamp={min_timestamp}&order_by=block_timestamp,asc"
        );
        let body = self.transport.get(&query).await?;
        let page: Trc20IndexPage = parse(&body)?;

        let mut blocks: HashMap<i64, BlockRef> = HashMap::new();
        let mut transfers = Vec::new();
        let mut latest_timestamp = min_timestamp;
        let mut seen = Vec::new();
        for entry in page.data {
            latest_timestamp = latest_timestamp.max(entry.block_timestamp.unwrap_or(0));
            let Some(id) = entry.transaction_id else {
                continue;
            };
            if seen.iter().any(|already| already == &id) {
                continue;
            }
            seen.push(id.clone());
            let tx_hash = TxHash::new(&id)
                .map_err(|error| TronSourceError::Unparseable(error.to_string()))?;
            let Some((raw, info)) = self.transaction(&tx_hash).await? else {
                continue;
            };
            let Some(number) = info.block_number else {
                continue;
            };
            let block = if let Some(block) = blocks.get(&number) {
                block.clone()
            } else {
                let block = self.block(number).await?;
                blocks.insert(number, block.clone());
                block
            };
            let evidence = sha256_hex(&raw);
            transfers.extend(self.readings(&info, &block, head, watch, &evidence)?);
        }

        let next_cursor = if latest_timestamp > min_timestamp {
            Some(
                CursorPosition::new(
                    CursorKind::LogicalTime,
                    latest_timestamp.to_string(),
                    None,
                    PROPOSED_FENCE_TOKEN,
                )
                .map_err(|error| TronSourceError::Unparseable(error.to_string()))?,
            )
        } else {
            None
        };
        Ok(ScanPage {
            transfers,
            next_cursor,
            head: Some(head.solid_head),
        })
    }

    /// Turns one transaction's logs into the readings that concern one watched
    /// address.
    fn readings(
        &self,
        info: &TransactionInfo,
        block: &BlockRef,
        head: HeadState,
        watch: &CollectorWatch,
        evidence_sha256: &str,
    ) -> Result<Vec<ObservedTransfer>, TronSourceError> {
        let parsed = parse_transfers(info)
            .map_err(|error| TronSourceError::Unparseable(error.to_string()))?;
        let mut readings = Vec::new();
        for transfer in parsed {
            if transfer.to != watch.address_key {
                continue;
            }
            let token = self.config.token_for(&transfer.contract);
            let reading = to_observation(
                &transfer,
                info,
                block,
                &self.config.context(),
                head,
                token,
                evidence_sha256.to_owned(),
            )
            .map_err(|error| TronSourceError::Unparseable(error.to_string()))?;
            readings.push(reading);
        }
        Ok(readings)
    }

    /// Reads one event directly, the way the verifier must.
    ///
    /// # Errors
    ///
    /// Returns [`TronSourceError`] when the provider is unreachable or its
    /// answer cannot be decoded.
    pub async fn read_event(
        &self,
        event: &ChainEventKey,
    ) -> Result<Option<ObservedTransfer>, TronSourceError> {
        if event.chain != self.config.chain
            || event.network != self.config.network
            || event.chain_environment != self.config.chain_environment
        {
            // A source reads one chain in one environment. Answering about
            // another one would let a testnet reading decide mainnet money.
            return Ok(None);
        }
        let Some((body, info)) = self.transaction(&event.tx_hash).await? else {
            return Ok(None);
        };
        let Some(number) = info.block_number else {
            return Ok(None);
        };
        let head = self.heads().await?;
        let block = self.block(number).await?;
        let evidence = sha256_hex(&body);
        let parsed = parse_transfers(&info)
            .map_err(|error| TronSourceError::Unparseable(error.to_string()))?;
        let Some(transfer) = parsed
            .into_iter()
            .find(|transfer| transfer.event_index == event.event_index)
        else {
            return Ok(None);
        };
        let token = self.config.token_for(&transfer.contract);
        let reading = to_observation(
            &transfer,
            &info,
            &block,
            &self.config.context(),
            head,
            token,
            evidence,
        )
        .map_err(|error| TronSourceError::Unparseable(error.to_string()))?;
        Ok(Some(reading))
    }
}

#[async_trait]
impl<T> ChainScanner for TronHttpSource<T>
where
    T: TronTransport,
{
    async fn scan(
        &self,
        watch: &CollectorWatch,
        cursor: Option<&CursorPosition>,
        limit: u32,
    ) -> Result<ScanPage, ScanError> {
        if watch.chain != self.config.chain
            || watch.network != self.config.network
            || watch.chain_environment != self.config.chain_environment
        {
            return Err(ScanError::Unparseable(
                "the watched collector belongs to another chain or environment".to_owned(),
            ));
        }
        let page = match self.config.lane {
            ScanLane::BlockRange => self.scan_blocks(watch, cursor, limit).await,
            ScanLane::AddressIndex => self.scan_address_index(watch, cursor, limit).await,
        };
        page.map_err(ScanError::from)
    }
}

#[async_trait]
impl<T> ChainReader for TronHttpSource<T>
where
    T: TronTransport,
{
    async fn lookup(
        &self,
        event: &ChainEventKey,
    ) -> Result<Option<ObservedTransfer>, ChainReaderError> {
        self.read_event(event).await.map_err(ChainReaderError::from)
    }
}

/// The block shape every TRON block endpoint answers with.
#[derive(Debug, Clone, Deserialize)]
struct BlockAnswer {
    #[serde(rename = "blockID", default)]
    block_id: Option<String>,
    #[serde(default)]
    block_header: Option<BlockHeader>,
}

#[derive(Debug, Clone, Deserialize)]
struct BlockHeader {
    #[serde(default)]
    raw_data: Option<BlockRawData>,
}

#[derive(Debug, Clone, Deserialize)]
struct BlockRawData {
    #[serde(default)]
    number: Option<i64>,
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(rename = "parentHash", default)]
    parent_hash: Option<String>,
}

impl BlockAnswer {
    fn number(&self) -> Option<i64> {
        self.block_header
            .as_ref()
            .and_then(|header| header.raw_data.as_ref())
            .and_then(|raw| raw.number)
    }

    fn into_block_ref(self) -> Result<BlockRef, TronSourceError> {
        let number = self.number().ok_or_else(|| {
            TronSourceError::Unparseable("the answer carries no block number".to_owned())
        })?;
        let hash = self.block_id.ok_or_else(|| {
            TronSourceError::Unparseable("the answer carries no block hash".to_owned())
        })?;
        let raw = self
            .block_header
            .as_ref()
            .and_then(|header| header.raw_data.as_ref());
        let milliseconds = raw.and_then(|raw| raw.timestamp).ok_or_else(|| {
            TronSourceError::Unparseable("the answer carries no block time".to_owned())
        })?;
        Ok(BlockRef {
            number,
            hash,
            parent_hash: raw.and_then(|raw| raw.parent_hash.clone()),
            time: block_time(milliseconds)
                .map_err(|error| TronSourceError::Unparseable(error.to_string()))?,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Trc20IndexPage {
    #[serde(default)]
    data: Vec<Trc20IndexEntry>,
}

/// One row of a provider's address index. Only two fields are read: which
/// transaction to look at, and how far the index has advanced. Everything else
/// it claims — amounts, tokens, participants — is read from the log instead.
#[derive(Debug, Clone, Deserialize)]
struct Trc20IndexEntry {
    #[serde(default)]
    transaction_id: Option<String>,
    #[serde(default)]
    block_timestamp: Option<i64>,
}

fn parse<T>(body: &str) -> Result<T, TronSourceError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_str(body)
        .map_err(|error| TronSourceError::Unparseable(format!("{error}: {}", truncate(body))))
}

fn missing_head() -> TronSourceError {
    TronSourceError::Unparseable("the answer carries no head block number".to_owned())
}

fn sha256_hex(body: &str) -> String {
    const DIGITS: [u8; 16] = *b"0123456789abcdef";
    let digest = Sha256::digest(body.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum TronSourceError {
    #[error("the TRON source is unreachable: {0}")]
    Unreachable(String),
    #[error("the TRON source answered with something unparseable: {0}")]
    Unparseable(String),
    #[error("the TRON source is misconfigured: {0}")]
    Configuration(String),
}

impl From<TronSourceError> for ScanError {
    fn from(error: TronSourceError) -> Self {
        match error {
            TronSourceError::Unreachable(detail) => Self::Unreachable(detail),
            TronSourceError::Unparseable(detail) | TronSourceError::Configuration(detail) => {
                Self::Unparseable(detail)
            }
        }
    }
}

impl From<TronSourceError> for ChainReaderError {
    fn from(error: TronSourceError) -> Self {
        match error {
            TronSourceError::Unreachable(detail) => Self::Unreachable(detail),
            TronSourceError::Unparseable(detail) | TronSourceError::Configuration(detail) => {
                Self::Unparseable(detail)
            }
        }
    }
}

#[cfg(test)]
mod tests;
