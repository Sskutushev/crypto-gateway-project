use std::{error::Error, str::FromStr};

use gateway_domain::RawAmount;
use time::macros::{date, datetime};
use uuid::Uuid;

use super::{
    AccountingPeriod, AllocationControl, SettlementDay, SettlementLedger, build_export, csv_field,
    to_csv,
};
use crate::OperationsError;

type TestResult = Result<(), Box<dyn Error>>;

const MERCHANT: Uuid = Uuid::from_u128(1);
const ASSET: Uuid = Uuid::from_u128(2);

fn raw(value: &str) -> Result<RawAmount, Box<dyn Error>> {
    Ok(RawAmount::from_str(value)?)
}

fn day(external_id: &str, allocated: &str, fiat: i128) -> Result<SettlementDay, Box<dyn Error>> {
    Ok(SettlementDay {
        day: date!(2026 - 09 - 01),
        merchant_id: MERCHANT,
        merchant_external_id: external_id.to_owned(),
        asset_id: ASSET,
        fiat_currency: "USD".to_owned(),
        payments: 2,
        partial_payments: 1,
        overpaid_payments: 1,
        allocated_raw: raw(allocated)?,
        fiat_minor: fiat,
        remainder_raw: raw("7")?,
    })
}

#[test]
fn a_period_is_whole_utc_days_and_bounded() -> TestResult {
    let period = AccountingPeriod::parse("2026-09-01", "2026-09-30")?;
    let (start, end) = period.instants()?;
    assert_eq!(start, datetime!(2026-09-01 00:00 UTC));
    assert_eq!(end, datetime!(2026-10-01 00:00 UTC));
    assert!(AccountingPeriod::parse("2026-09-01", "2026-09-01").is_ok());
    assert!(AccountingPeriod::parse("2026-01-01", "2026-04-02").is_ok());
    for (from, to) in [
        ("2026-09-02", "2026-09-01"),
        ("2026-01-01", "2026-04-03"),
        ("2026-9-1", "2026-09-30"),
        ("yesterday", "today"),
    ] {
        assert!(
            matches!(
                AccountingPeriod::parse(from, to),
                Err(OperationsError::ExportRangeInvalid)
            ),
            "{from}..{to}"
        );
    }
    Ok(())
}

#[test]
fn totals_add_up_and_the_control_sum_must_agree() -> TestResult {
    let period = AccountingPeriod::parse("2026-09-01", "2026-09-02")?;
    let mut second = day("shop", "5", 50)?;
    second.day = date!(2026 - 09 - 02);
    let ledger = SettlementLedger {
        days: vec![day("shop", "10", 100)?, second],
        allocations: vec![AllocationControl {
            asset_id: ASSET,
            allocation_rows: 6,
            allocated_raw: raw("15")?,
        }],
    };

    let export = build_export(period, None, ledger.clone())?;

    assert_eq!(export.totals.len(), 1);
    let total = &export.totals[0];
    assert_eq!(total.allocated_raw.to_string(), "15");
    assert_eq!(total.fiat_minor, 150);
    assert_eq!(total.remainder_raw.to_string(), "14");
    assert_eq!(
        (
            total.payments,
            total.partial_payments,
            total.overpaid_payments
        ),
        (4, 2, 2)
    );
    assert!(export.control.balanced);

    // One raw unit the allocation rows do not explain is a finding, shown.
    let mut drifted = ledger;
    drifted.allocations[0].allocated_raw = raw("16")?;
    assert!(!build_export(period, None, drifted)?.control.balanced);
    Ok(())
}

#[test]
fn csv_quotes_what_needs_quoting_and_defuses_formulas() {
    assert_eq!(csv_field("plain"), "plain");
    assert_eq!(csv_field("Shop, Ltd"), "\"Shop, Ltd\"");
    assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
    assert_eq!(csv_field("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
    assert_eq!(csv_field("+1"), "'+1");
    assert_eq!(csv_field("@sum"), "'@sum");
    assert_eq!(csv_field("-5"), "'-5");
}

#[test]
fn csv_has_one_header_day_total_and_control_rows_in_order() -> TestResult {
    let period = AccountingPeriod::parse("2026-09-01", "2026-09-01")?;
    let export = build_export(
        period,
        Some(MERCHANT),
        SettlementLedger {
            days: vec![day("Shop, \"One\"", "10", 100)?],
            allocations: vec![AllocationControl {
                asset_id: ASSET,
                allocation_rows: 3,
                allocated_raw: raw("10")?,
            }],
        },
    )?;

    let csv = to_csv(&export);
    let lines: Vec<&str> = csv.split("\r\n").collect();

    assert_eq!(lines.len(), 5, "{csv}");
    assert!(lines[0].starts_with("row_type,day,merchant_id"));
    assert_eq!(
        lines[1],
        format!("day,2026-09-01,{MERCHANT},\"Shop, \"\"One\"\"\",{ASSET},USD,2,1,1,10,100,7,")
    );
    assert_eq!(lines[2], format!("total,,,,{ASSET},USD,2,1,1,10,100,7,"));
    assert_eq!(lines[3], format!("control,,,,{ASSET},,3,,,10,,,true"));
    assert_eq!(lines[4], "");
    // Every row has the header's column count once quoting is accounted for.
    for line in &lines[..4] {
        let mut columns = 1;
        let mut quoted = false;
        for character in line.chars() {
            match character {
                '"' => quoted = !quoted,
                ',' if !quoted => columns += 1,
                _ => {}
            }
        }
        assert_eq!(columns, 13, "{line}");
    }
    Ok(())
}
