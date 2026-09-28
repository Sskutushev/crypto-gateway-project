//! Settled money per merchant, asset and UTC day, for the books.
//!
//! The basis is the settlement decision, the same row reconciliation compares
//! with fulfilment: a decision that allocated money (`settled`, `overpaid` or
//! `partial`) counts on the UTC day it was decided. A payment is counted, and
//! its invoiced fiat amount summed, once, on the decision that completed it;
//! a partial allocation adds raw units without adding fiat, because the fiat
//! amount belongs to the whole obligation. The allocated raw units are summed
//! a second time from the allocation rows themselves, and the export says
//! whether the two agree instead of assuming it.

use async_trait::async_trait;
use gateway_domain::RawAmount;
use time::{Date, Duration, OffsetDateTime, Time, macros::format_description};
use uuid::Uuid;

use crate::{
    OperationsError, OperatorCredential, OperatorReadRepository, OperatorReadService,
    OperatorScope, RepositoryError,
};

/// The longest period one export covers.
pub const MAX_EXPORT_DAYS: i64 = 92;
/// The most rows one export returns. A larger answer is refused, never cut.
pub const MAX_EXPORT_ROWS: usize = 20_000;

/// Inclusive UTC calendar days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountingPeriod {
    pub from: Date,
    pub to: Date,
}

impl AccountingPeriod {
    /// Parses `YYYY-MM-DD` bounds.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError::ExportRangeInvalid`] for a malformed date, an
    /// end before the start, or more than [`MAX_EXPORT_DAYS`] days.
    pub fn parse(from: &str, to: &str) -> Result<Self, OperationsError> {
        let format = format_description!("[year]-[month]-[day]");
        let from = Date::parse(from, &format).map_err(|_| OperationsError::ExportRangeInvalid)?;
        let to = Date::parse(to, &format).map_err(|_| OperationsError::ExportRangeInvalid)?;
        let days = (to - from).whole_days() + 1;
        if !(1..=MAX_EXPORT_DAYS).contains(&days) {
            return Err(OperationsError::ExportRangeInvalid);
        }
        Ok(Self { from, to })
    }

    /// The half-open instant range `[from 00:00 UTC, to + 1 day 00:00 UTC)`.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError::ExportRangeInvalid`] at the end of time.
    pub fn instants(&self) -> Result<(OffsetDateTime, OffsetDateTime), OperationsError> {
        let start = self.from.with_time(Time::MIDNIGHT).assume_utc();
        let end = self
            .to
            .checked_add(Duration::days(1))
            .ok_or(OperationsError::ExportRangeInvalid)?
            .with_time(Time::MIDNIGHT)
            .assume_utc();
        Ok((start, end))
    }
}

/// One merchant, asset and fiat currency on one UTC day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementDay {
    pub day: Date,
    pub merchant_id: Uuid,
    pub merchant_external_id: String,
    pub asset_id: Uuid,
    pub fiat_currency: String,
    /// Obligations completed (`settled` or `overpaid`).
    pub payments: u64,
    pub partial_payments: u64,
    pub overpaid_payments: u64,
    pub allocated_raw: RawAmount,
    /// Invoiced minor units of the completed obligations.
    pub fiat_minor: i128,
    /// Overpayment remainders recorded, never absorbed.
    pub remainder_raw: RawAmount,
}

/// Every day of one asset and fiat currency added up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementTotal {
    pub asset_id: Uuid,
    pub fiat_currency: String,
    pub payments: u64,
    pub partial_payments: u64,
    pub overpaid_payments: u64,
    pub allocated_raw: RawAmount,
    pub fiat_minor: i128,
    pub remainder_raw: RawAmount,
}

/// The allocation rows behind the same decisions, summed per asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationControl {
    pub asset_id: Uuid,
    pub allocation_rows: u64,
    pub allocated_raw: RawAmount,
}

/// What the repository reads: the day rows (at most `max_rows + 1`) and the
/// independent allocation sums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementLedger {
    pub days: Vec<SettlementDay>,
    pub allocations: Vec<AllocationControl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlSum {
    pub allocations: Vec<AllocationControl>,
    /// Whether, for every asset, the decisions and the allocation rows name the
    /// same number of raw units. `false` is a finding for reconciliation.
    pub balanced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountingExport {
    pub period: AccountingPeriod,
    pub merchant_id: Option<Uuid>,
    pub days: Vec<SettlementDay>,
    pub totals: Vec<SettlementTotal>,
    pub control: ControlSum,
}

#[async_trait]
pub trait AccountingRepository: Send + Sync {
    /// Money-moving settlement decisions in `[start, end)`, grouped by UTC day,
    /// merchant, asset and fiat currency in that order, plus the allocation
    /// rows behind the same decisions summed per asset.
    async fn settlement_ledger(
        &self,
        start: OffsetDateTime,
        end: OffsetDateTime,
        merchant_id: Option<Uuid>,
        max_rows: usize,
    ) -> Result<SettlementLedger, RepositoryError>;
}

impl<R> OperatorReadService<R>
where
    R: OperatorReadRepository + AccountingRepository,
{
    /// Builds the settlement export for inclusive UTC days `from..=to`.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError`] when the key lacks `read`, the range is
    /// invalid, the answer would exceed [`MAX_EXPORT_ROWS`], or storage fails.
    pub async fn settlement_export(
        &self,
        credential: &OperatorCredential,
        from: &str,
        to: &str,
        merchant_id: Option<Uuid>,
    ) -> Result<AccountingExport, OperationsError> {
        if !credential.allows(OperatorScope::Read) {
            return Err(OperationsError::MissingScope(OperatorScope::Read));
        }
        let period = AccountingPeriod::parse(from, to)?;
        let (start, end) = period.instants()?;
        let ledger = self
            .repository()
            .settlement_ledger(start, end, merchant_id, MAX_EXPORT_ROWS)
            .await?;
        if ledger.days.len() > MAX_EXPORT_ROWS {
            return Err(OperationsError::ExportTooLarge);
        }
        build_export(period, merchant_id, ledger)
    }
}

/// Adds the days up per asset and currency and checks them against the
/// allocation rows.
///
/// # Errors
///
/// Returns [`OperationsError::Repository`] when a sum overflows, which only
/// corrupt stored amounts can cause.
pub fn build_export(
    period: AccountingPeriod,
    merchant_id: Option<Uuid>,
    ledger: SettlementLedger,
) -> Result<AccountingExport, OperationsError> {
    let overflow = || {
        OperationsError::Repository(RepositoryError::CorruptData("settled sums overflow".into()))
    };
    let mut totals: Vec<SettlementTotal> = Vec::new();
    for day in &ledger.days {
        let existing = totals
            .iter()
            .position(|t| t.asset_id == day.asset_id && t.fiat_currency == day.fiat_currency);
        let index = if let Some(index) = existing {
            index
        } else {
            totals.push(SettlementTotal {
                asset_id: day.asset_id,
                fiat_currency: day.fiat_currency.clone(),
                payments: 0,
                partial_payments: 0,
                overpaid_payments: 0,
                allocated_raw: RawAmount::ZERO,
                fiat_minor: 0,
                remainder_raw: RawAmount::ZERO,
            });
            totals.len() - 1
        };
        let total = &mut totals[index];
        total.payments = total
            .payments
            .checked_add(day.payments)
            .ok_or_else(overflow)?;
        total.partial_payments = total
            .partial_payments
            .checked_add(day.partial_payments)
            .ok_or_else(overflow)?;
        total.overpaid_payments = total
            .overpaid_payments
            .checked_add(day.overpaid_payments)
            .ok_or_else(overflow)?;
        total.allocated_raw = total
            .allocated_raw
            .checked_add(day.allocated_raw)
            .map_err(|_| overflow())?;
        total.fiat_minor = total
            .fiat_minor
            .checked_add(day.fiat_minor)
            .ok_or_else(overflow)?;
        total.remainder_raw = total
            .remainder_raw
            .checked_add(day.remainder_raw)
            .map_err(|_| overflow())?;
    }
    totals.sort_by(|a, b| {
        (a.asset_id, a.fiat_currency.as_str()).cmp(&(b.asset_id, b.fiat_currency.as_str()))
    });

    let mut per_asset: Vec<(Uuid, RawAmount)> = Vec::new();
    for total in &totals {
        match per_asset
            .iter_mut()
            .find(|(asset, _)| *asset == total.asset_id)
        {
            Some((_, sum)) => {
                *sum = sum
                    .checked_add(total.allocated_raw)
                    .map_err(|_| overflow())?;
            }
            None => per_asset.push((total.asset_id, total.allocated_raw)),
        }
    }
    let balanced = per_asset.len() == ledger.allocations.len()
        && per_asset.iter().all(|(asset, sum)| {
            ledger
                .allocations
                .iter()
                .any(|control| control.asset_id == *asset && control.allocated_raw == *sum)
        });
    Ok(AccountingExport {
        period,
        merchant_id,
        days: ledger.days,
        totals,
        control: ControlSum {
            allocations: ledger.allocations,
            balanced,
        },
    })
}

pub const CSV_HEADER: &str = "row_type,day,merchant_id,merchant_external_id,asset_id,fiat_currency,payments,partial_payments,overpaid_payments,allocated_raw,fiat_minor,remainder_raw,control_balanced";

/// Renders the export as RFC 4180 CSV: a header, one `day` row per group, a
/// `total` row per asset and currency, and a `control` row per asset carrying
/// the allocation-row sum and whether it matches.
#[must_use]
pub fn to_csv(export: &AccountingExport) -> String {
    let mut out = String::with_capacity(128 * (export.days.len() + export.totals.len() + 2));
    out.push_str(CSV_HEADER);
    out.push_str("\r\n");
    for day in &export.days {
        push_row(
            &mut out,
            &[
                "day",
                &day.day.to_string(),
                &day.merchant_id.to_string(),
                &day.merchant_external_id,
                &day.asset_id.to_string(),
                &day.fiat_currency,
                &day.payments.to_string(),
                &day.partial_payments.to_string(),
                &day.overpaid_payments.to_string(),
                &day.allocated_raw.to_string(),
                &day.fiat_minor.to_string(),
                &day.remainder_raw.to_string(),
                "",
            ],
        );
    }
    for total in &export.totals {
        push_row(
            &mut out,
            &[
                "total",
                "",
                "",
                "",
                &total.asset_id.to_string(),
                &total.fiat_currency,
                &total.payments.to_string(),
                &total.partial_payments.to_string(),
                &total.overpaid_payments.to_string(),
                &total.allocated_raw.to_string(),
                &total.fiat_minor.to_string(),
                &total.remainder_raw.to_string(),
                "",
            ],
        );
    }
    for control in &export.control.allocations {
        push_row(
            &mut out,
            &[
                "control",
                "",
                "",
                "",
                &control.asset_id.to_string(),
                "",
                &control.allocation_rows.to_string(),
                "",
                "",
                &control.allocated_raw.to_string(),
                "",
                "",
                if export.control.balanced {
                    "true"
                } else {
                    "false"
                },
            ],
        );
    }
    out
}

fn push_row(out: &mut String, fields: &[&str]) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&csv_field(field));
    }
    out.push_str("\r\n");
}

/// Quotes a field when CSV needs it, and defuses a leading character a
/// spreadsheet would read as a formula: an export opened in one must show a
/// merchant's text, never run it.
#[must_use]
pub fn csv_field(value: &str) -> String {
    let defused = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        let mut guarded = String::with_capacity(value.len() + 1);
        guarded.push('\'');
        guarded.push_str(value);
        guarded
    } else {
        value.to_owned()
    };
    if defused.contains([',', '"', '\n', '\r']) {
        let mut quoted = String::with_capacity(defused.len() + 2);
        quoted.push('"');
        for character in defused.chars() {
            if character == '"' {
                quoted.push('"');
            }
            quoted.push(character);
        }
        quoted.push('"');
        quoted
    } else {
        defused
    }
}

/// The file name a download is offered under.
#[must_use]
pub fn csv_file_name(period: &AccountingPeriod) -> String {
    format!("settlements-{}-to-{}.csv", period.from, period.to)
}

#[cfg(test)]
mod tests;
