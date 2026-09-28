mod health;
mod observations;
mod operations;
mod operator_reads;
mod outbox;
mod payment_intents;
mod ports;
mod quotes;
mod reconciliation;
mod self_check;
mod settlement;
mod verification;

pub use health::{ComponentState, ComponentStatus, HealthError, HealthRepository, HealthService};
pub use observations::{
    ChainScanner, ChainSource, CollectorState, CollectorWatch, ComponentLease, CursorKind,
    CursorPosition, IntakeReport, ObservationError, ObservationRepository, ObservationService,
    RefusalReason, ResolvedObservation, ScanError, ScanPage, SourceKind, SourceState,
    parse_raw_amount, parse_tx_hash,
};
pub use operations::{
    ManualResolution, ManualResolutionResult, OperationsError, OperationsRepository,
    OperationsService, OperatorCredential, OperatorScope, PriceIngestion, PriceOutcome, RailStop,
    RecordedPrice, RiskSubmission, discard_code,
};
pub use operator_reads::{
    ConflictItem, DeadLetter, DiscrepancyAggregate, EvidenceAllocation, EvidenceAttempt,
    EvidenceFulfillment, EvidenceIntent, EvidencePaymentEvent, EvidenceQuote,
    EvidenceSettlementDecision, EvidenceTransfer, HeldPayment, MinorUnits, ObservationConflict,
    OperatorReadRepository, OperatorReadService, Overview, Page, PageRequest,
    PaymentIntentEvidence, ReconciliationDiscrepancy, ReconciliationRun, ReconciliationRunSummary,
    SettlementDecisionSummary, StateCount, UnmatchedTransfer, WebhookDeliverySummary,
};
pub use outbox::{
    DeliveryAttempt, DeliveryResult, OutboxError, OutboxEvent, OutboxReport, OutboxRepository,
    OutboxService, WebhookEndpoint, WebhookSender,
};
pub use payment_intents::{
    CreatePaymentIntent, CreatePaymentIntentResult, PaymentIntentService, ServiceError,
};
pub use ports::{
    ApiCredential, Clock, ExpiryResult, IdempotentCreate, IdempotentQuote, LeaseRepository,
    PaymentIntentRepository, QuoteContext, QuoteRepository, RepositoryError, SystemClock,
};
pub use quotes::{ExpirySweeper, IssueQuote, IssueQuoteResult, QuoteService, QuoteServiceError};
pub use reconciliation::{
    COMPONENT as RECONCILER_COMPONENT, Discrepancy, DiscrepancyKind,
    HARD_STOP_REASON as RECONCILIATION_HARD_STOP_REASON, ReconciliationError, ReconciliationKind,
    ReconciliationReport, ReconciliationRepository, ReconciliationService, ReconciliationWindow,
    RunRecord, RunStatus, ScanFindings,
};
pub use self_check::{
    ExpectedAsset, SelfCheckConfig, SelfCheckConfigError, SelfCheckReport, SelfCheckRepository,
    SelfCheckResult, SelfCheckService,
};
pub use settlement::{
    AttemptSnapshot, PendingTransfer, SettlementCommand, SettlementRecord, SettlementReport,
    SettlementRepository, SettlementService, SettlementServiceError, UnresolvedTransfer,
};
pub use verification::{
    ChainEventKey, ChainReader, ChainReaderError, VerdictOutcome, VerificationReport,
    VerificationRepository, VerificationService, VerificationServiceError,
};
