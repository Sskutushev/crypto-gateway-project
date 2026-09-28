//! `gateway-worker admin <command> --flag value ...`
//!
//! Onboarding without hand-written SQL. Every command names the person who
//! runs it (`--actor`), is written to the audit trail with its change, and
//! prints one JSON object on stdout. Secrets are printed exactly once, at
//! creation, and never logged; store them before closing the terminal.
//!
//! It runs with `GATEWAY_DATABASE_URL` set to the provisioner role (see
//! `db/roles`). Webhook commands also need `GATEWAY_WEBHOOK_MASTER_KEY`,
//! because a webhook signing secret is derived from it and only its
//! fingerprint is stored.

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use gateway_application::{
    CollectorPolicy, ListRequest, NewCollector, ProvisioningError, ProvisioningService,
    RandomBytes, SystemClock, WebhookRedelivery,
};
use gateway_storage::{PgPoolOptions, PostgresRepository};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

const USAGE: &str = "usage: gateway-worker admin <command> --actor <name> [flags]

commands:
  merchant-create     --external-id <id> --name <display name> [--collector-policy own|shared]
  api-key-issue       --merchant <uuid> --label <text>
  api-key-revoke      --key <uuid> --reason <text>
  webhook-add         --merchant <uuid> --url <https url> [--description <text>]
  webhook-rotate      --endpoint <uuid> --reason <text> [--transition-hours <1..720>]
  webhook-disable     --endpoint <uuid> --reason <text>
  webhook-test        --endpoint <uuid>
  collector-statement --merchant <uuid> --address <T...> [--issued <RFC 3339>]
  collector-register  --asset <uuid> --address <T...>
                      (--merchant <uuid> --issued <RFC 3339> --signature <hex>
                       | [--merchant <uuid>] --manual-evidence <who checked and how>)
  collector-stop-quoting --collector <uuid> --reason <text>
  collector-retire    --collector <uuid> --reason <text> [--compromised yes]
                      (refused while a reservation remains, unless compromised)
  webhook-redeliver   --event <uuid> --reason <text> --idempotency-key <16-128 chars>
                      [--endpoint <uuid>]
                      (the same event id and payload again; one endpoint or all)

read-only commands (no --actor; never print a secret or a hash):
  merchant-list       [--limit <1..200>] [--cursor <uuid>]
  api-key-list        --merchant <uuid> [--limit <1..200>] [--cursor <uuid>]
  webhook-list        --merchant <uuid> [--limit <1..200>] [--cursor <uuid>]
  collector-list      [--merchant <uuid>] [--limit <1..200>] [--cursor <uuid>]";

const READ_COMMANDS: [&str; 4] = [
    "merchant-list",
    "api-key-list",
    "webhook-list",
    "collector-list",
];

struct OsRandom;

impl RandomBytes for OsRandom {
    fn fill(&self, buffer: &mut [u8]) -> Result<(), ProvisioningError> {
        getrandom::fill(buffer).map_err(|_| ProvisioningError::Randomness)
    }
}

/// Runs one admin command. `args` excludes the program name and `admin`.
pub async fn run(args: &[String]) -> Result<()> {
    let (command, flags) = parse(args)?;
    if command == "collector-statement" {
        // Builds the text to sign; touches no database.
        return print(&statement(&flags)?);
    }
    let read_only = READ_COMMANDS.contains(&command);
    let actor = if read_only {
        ""
    } else {
        flag(&flags, "actor")?
    };
    let database_url =
        std::env::var("GATEWAY_DATABASE_URL").context("GATEWAY_DATABASE_URL is required")?;
    let master_key = match std::env::var("GATEWAY_WEBHOOK_MASTER_KEY") {
        Ok(raw) => Some(
            gateway_tron::address::decode_hex(raw.trim())
                .map_err(|_| anyhow!("GATEWAY_WEBHOOK_MASTER_KEY must be hex encoded"))?,
        ),
        Err(_) => None,
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .context("connect to PostgreSQL")?;
    let service = ProvisioningService::new(
        Arc::new(PostgresRepository::new(pool)),
        SystemClock,
        OsRandom,
        master_key,
    );
    let output = if read_only {
        list(&service, command, &flags).await?
    } else {
        execute(&service, command, &flags, actor).await?
    };
    print(&output)
}

async fn execute(
    service: &ProvisioningService<PostgresRepository, SystemClock, OsRandom>,
    command: &str,
    flags: &HashMap<String, String>,
    actor: &str,
) -> Result<Value> {
    Ok(match command {
        "merchant-create" => {
            let policy: CollectorPolicy = flags
                .get("collector-policy")
                .map_or("own", String::as_str)
                .parse()?;
            let merchant = service
                .create_merchant(
                    actor,
                    flag(flags, "external-id")?,
                    flag(flags, "name")?,
                    policy,
                )
                .await?;
            json!({
                "merchant_id": merchant.id,
                "external_id": merchant.external_id,
                "collector_policy": merchant.collector_policy.as_str(),
                "created": merchant.created,
            })
        }
        "api-key-issue" => {
            let key = service
                .issue_api_key(actor, uuid(flags, "merchant")?, flag(flags, "label")?)
                .await?;
            json!({
                "key_id": key.key_id,
                "merchant_id": key.merchant_id,
                "prefix": key.prefix,
                "secret": key.secret,
                "note": "shown once; the gateway keeps only its hash",
            })
        }
        "api-key-revoke" => {
            let key = uuid(flags, "key")?;
            service
                .revoke_api_key(actor, key, flag(flags, "reason")?)
                .await?;
            json!({ "key_id": key, "revoked": true })
        }
        "webhook-add" => add_webhook(service, flags, actor).await?,
        "webhook-rotate" => rotate_webhook(service, flags, actor).await?,
        "webhook-disable" => {
            let endpoint = uuid(flags, "endpoint")?;
            service
                .disable_webhook_endpoint(actor, endpoint, flag(flags, "reason")?)
                .await?;
            json!({ "endpoint_id": endpoint, "disabled": true })
        }
        "webhook-test" => {
            let endpoint = uuid(flags, "endpoint")?;
            let event = service.send_test_event(actor, endpoint).await?;
            json!({
                "endpoint_id": endpoint,
                "event_id": event,
                "note": "queued; the outbox worker delivers it like any other event",
            })
        }
        "collector-register" => register_collector(service, flags, actor).await?,
        "webhook-redeliver" => redeliver(service, flags, actor).await?,
        "collector-stop-quoting" => {
            let collector = uuid(flags, "collector")?;
            service
                .stop_quoting_collector(actor, collector, flag(flags, "reason")?)
                .await?;
            json!({
                "collector_id": collector,
                "state": "receiving_only",
                "note": "no new quotes; issued quotes are still watched and paid; retire once no reservation remains",
            })
        }
        "collector-retire" => {
            let collector = uuid(flags, "collector")?;
            let compromised = match flags.get("compromised").map(String::as_str) {
                None => false,
                Some("yes") => true,
                Some(_) => bail!("--compromised takes the value yes"),
            };
            service
                .retire_collector(actor, collector, flag(flags, "reason")?, compromised)
                .await?;
            json!({ "collector_id": collector, "retired": true, "compromised": compromised })
        }
        other => bail!("unknown admin command {other}\n\n{USAGE}"),
    })
}

async fn redeliver(
    service: &ProvisioningService<PostgresRepository, SystemClock, OsRandom>,
    flags: &HashMap<String, String>,
    actor: &str,
) -> Result<Value> {
    let request = WebhookRedelivery {
        event_id: uuid(flags, "event")?,
        endpoint_id: flags
            .get("endpoint")
            .map(|value| parse_uuid("endpoint", value))
            .transpose()?,
        reason: flag(flags, "reason")?.to_owned(),
    };
    let result = service
        .redeliver_webhook(actor, flag(flags, "idempotency-key")?, &request)
        .await?;
    Ok(json!({
        "redelivery_id": result.id,
        "event_id": result.event_id,
        "merchant_id": result.merchant_id,
        "endpoint_id": result.endpoint_id,
        "previous_state": result.previous_state,
        "previous_attempts": result.previous_attempts,
        "requested_at": rfc3339(result.requested_at)?,
        "replayed": result.replayed,
        "note": "queued again with the same event id; merchants deduplicate by it",
    }))
}

// One flat table of read-only commands; splitting it would scatter it.
#[allow(clippy::too_many_lines)]
async fn list(
    service: &ProvisioningService<PostgresRepository, SystemClock, OsRandom>,
    command: &str,
    flags: &HashMap<String, String>,
) -> Result<Value> {
    let limit = flags
        .get("limit")
        .map(|value| value.parse::<u32>())
        .transpose()
        .context("--limit must be a whole number")?;
    let cursor = flags
        .get("cursor")
        .map(|value| parse_uuid("cursor", value))
        .transpose()?;
    let page = ListRequest::new(limit, cursor)?;
    let (items, next_cursor) = match command {
        "merchant-list" => {
            let listing = service.list_merchants(page).await?;
            let items = listing
                .items
                .into_iter()
                .map(|merchant| {
                    Ok(json!({
                        "merchant_id": merchant.id,
                        "external_id": merchant.external_id,
                        "display_name": merchant.display_name,
                        "status": merchant.status,
                        "collector_policy": merchant.collector_policy,
                        "created_at": rfc3339(merchant.created_at)?,
                    }))
                })
                .collect::<Result<Vec<_>>>()?;
            (items, listing.next_cursor)
        }
        "api-key-list" => {
            let listing = service
                .list_api_keys(uuid(flags, "merchant")?, page)
                .await?;
            let items = listing
                .items
                .into_iter()
                .map(|key| {
                    Ok(json!({
                        "key_id": key.id,
                        "merchant_id": key.merchant_id,
                        "prefix": key.prefix,
                        "label": key.label,
                        "created_at": rfc3339(key.created_at)?,
                        "last_used_at": key.last_used_at.map(rfc3339).transpose()?,
                        "revoked_at": key.revoked_at.map(rfc3339).transpose()?,
                    }))
                })
                .collect::<Result<Vec<_>>>()?;
            (items, listing.next_cursor)
        }
        "webhook-list" => {
            let listing = service
                .list_webhook_endpoints(uuid(flags, "merchant")?, page)
                .await?;
            let items = listing
                .items
                .into_iter()
                .map(|endpoint| {
                    Ok(json!({
                        "endpoint_id": endpoint.id,
                        "merchant_id": endpoint.merchant_id,
                        "url": endpoint.url,
                        "description": endpoint.description,
                        "status": endpoint.status,
                        "secret_version": endpoint.secret_version,
                        "previous_secret_signs_until":
                            endpoint.previous_secret_valid_until.map(rfc3339).transpose()?,
                        "created_at": rfc3339(endpoint.created_at)?,
                        "disabled_at": endpoint.disabled_at.map(rfc3339).transpose()?,
                    }))
                })
                .collect::<Result<Vec<_>>>()?;
            (items, listing.next_cursor)
        }
        "collector-list" => {
            let merchant = flags
                .get("merchant")
                .map(|value| parse_uuid("merchant", value))
                .transpose()?;
            let listing = service.list_collectors(merchant, page).await?;
            let items = listing
                .items
                .into_iter()
                .map(|collector| {
                    Ok(json!({
                        "collector_id": collector.id,
                        "asset_id": collector.asset_id,
                        "merchant_id": collector.merchant_id,
                        "address": collector.address,
                        "state": collector.state,
                        "open_reservations": collector.open_reservations,
                        "valid_from": rfc3339(collector.valid_from)?,
                        "retired_at": collector.retired_at.map(rfc3339).transpose()?,
                    }))
                })
                .collect::<Result<Vec<_>>>()?;
            (items, listing.next_cursor)
        }
        other => bail!("unknown admin command {other}\n\n{USAGE}"),
    };
    Ok(json!({ "items": items, "next_cursor": next_cursor }))
}

async fn add_webhook(
    service: &ProvisioningService<PostgresRepository, SystemClock, OsRandom>,
    flags: &HashMap<String, String>,
    actor: &str,
) -> Result<Value> {
    let url = flag(flags, "url")?;
    gateway_webhook::validate_endpoint_url(url)
        .map_err(|error| anyhow!("the webhook URL is refused: {error}"))?;
    let registration = service
        .add_webhook_endpoint(
            actor,
            uuid(flags, "merchant")?,
            url,
            flags.get("description").map(String::as_str),
        )
        .await?;
    Ok(json!({
        "endpoint_id": registration.endpoint_id,
        "merchant_id": registration.merchant_id,
        "url": registration.url,
        "secret_version": registration.secret_version,
        "signing_secret": registration.signing_secret,
        "note": "shown once; verify deliveries with HMAC-SHA256 keyed by the hex-decoded secret",
    }))
}

async fn rotate_webhook(
    service: &ProvisioningService<PostgresRepository, SystemClock, OsRandom>,
    flags: &HashMap<String, String>,
    actor: &str,
) -> Result<Value> {
    let hours: i64 = flags
        .get("transition-hours")
        .map_or(Ok(72), |value| value.parse())
        .context("--transition-hours must be a whole number of hours")?;
    let rotated = service
        .rotate_webhook_secret(
            actor,
            uuid(flags, "endpoint")?,
            Duration::hours(hours),
            flag(flags, "reason")?,
        )
        .await?;
    Ok(json!({
        "endpoint_id": rotated.endpoint_id,
        "secret_version": rotated.secret_version,
        "signing_secret": rotated.signing_secret,
        "previous_secret_signs_until": rotated.previous_valid_until.map(rfc3339).transpose()?,
        "note": "shown once; both secrets sign every delivery until the previous one expires",
    }))
}

async fn register_collector(
    service: &ProvisioningService<PostgresRepository, SystemClock, OsRandom>,
    flags: &HashMap<String, String>,
    actor: &str,
) -> Result<Value> {
    let address_text = flag(flags, "address")?;
    let address = gateway_tron::from_base58(address_text)
        .map_err(|error| anyhow!("--address is not a TRON address: {error}"))?;
    let merchant = flags
        .get("merchant")
        .map(|value| parse_uuid("merchant", value))
        .transpose()?;
    let now = OffsetDateTime::now_utc();
    // A merchant's address is proven by the merchant's own signature when
    // possible; a manual check is accepted only with a named, recorded reason.
    let evidence = match (
        merchant,
        flags.get("signature"),
        flags.get("manual-evidence"),
    ) {
        (Some(merchant_id), Some(signature), None) => {
            let issued = OffsetDateTime::parse(flag(flags, "issued")?, &Rfc3339)
                .context("--issued must be RFC 3339")?;
            gateway_tron::verify_ownership(merchant_id, &address, issued, signature, now)
                .map_err(|error| anyhow!("the ownership proof does not hold: {error}"))?;
            format!(
                "TIP-191 signature over the ownership statement issued {} verified at {}",
                rfc3339(issued)?,
                rfc3339(now)?
            )
        }
        (_, None, Some(manual)) => format!("manual check by {actor}: {}", manual.trim()),
        (_, Some(_), Some(_)) => bail!("give either --signature or --manual-evidence, not both"),
        (None, Some(_), None) => bail!("--signature proves a merchant's address; add --merchant"),
        (_, None, None) => {
            bail!("ownership must be proven: --signature with --issued, or --manual-evidence")
        }
    };
    let collector = service
        .register_collector(
            actor,
            NewCollector {
                id: Uuid::now_v7(),
                asset_id: uuid(flags, "asset")?,
                merchant_id: merchant,
                address,
                address_text: address_text.trim().to_owned(),
                ownership_evidence: evidence.clone(),
            },
        )
        .await?;
    Ok(json!({
        "collector_id": collector,
        "merchant_id": merchant,
        "address": address_text.trim(),
        "ownership_evidence": evidence,
    }))
}

fn statement(flags: &HashMap<String, String>) -> Result<Value> {
    let merchant = uuid(flags, "merchant")?;
    let address = gateway_tron::from_base58(flag(flags, "address")?)
        .map_err(|error| anyhow!("--address is not a TRON address: {error}"))?;
    let issued = match flags.get("issued") {
        Some(value) => {
            OffsetDateTime::parse(value, &Rfc3339).context("--issued must be RFC 3339")?
        }
        None => OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .context("truncate the issue time")?,
    };
    let text = gateway_tron::ownership_statement(merchant, &address, issued)
        .map_err(|error| anyhow!("{error}"))?;
    Ok(json!({
        "statement": text,
        "issued": rfc3339(issued)?,
        "valid_for_hours": gateway_tron::OWNERSHIP_PROOF_TTL.whole_hours(),
        "next": "sign the statement exactly, with the wallet that holds the address (TronLink signMessageV2), then run collector-register with --issued and --signature",
    }))
}

fn parse(args: &[String]) -> Result<(&str, HashMap<String, String>)> {
    let Some((command, rest)) = args.split_first() else {
        bail!("{USAGE}");
    };
    let mut flags = HashMap::new();
    let mut iter = rest.iter();
    while let Some(name) = iter.next() {
        let key = name
            .strip_prefix("--")
            .ok_or_else(|| anyhow!("expected a --flag, got {name}\n\n{USAGE}"))?;
        let value = iter
            .next()
            .ok_or_else(|| anyhow!("--{key} needs a value"))?;
        if flags.insert(key.to_owned(), value.clone()).is_some() {
            bail!("--{key} was given twice");
        }
    }
    Ok((command.as_str(), flags))
}

fn flag<'a>(flags: &'a HashMap<String, String>, name: &str) -> Result<&'a str> {
    flags
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("--{name} is required\n\n{USAGE}"))
}

fn uuid(flags: &HashMap<String, String>, name: &str) -> Result<Uuid> {
    parse_uuid(name, flag(flags, name)?)
}

fn parse_uuid(name: &str, value: &str) -> Result<Uuid> {
    value
        .trim()
        .parse()
        .map_err(|_| anyhow!("--{name} must be a UUID"))
}

fn rfc3339(value: OffsetDateTime) -> Result<String> {
    value.format(&Rfc3339).context("format a timestamp")
}

#[allow(clippy::print_stdout)]
fn print(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn flags_are_paired_and_never_repeated() -> anyhow::Result<()> {
        let parsed = args(&["api-key-issue", "--merchant", "m", "--label", "server"]);
        let (command, flags) = parse(&parsed)?;
        assert_eq!(command, "api-key-issue");
        assert_eq!(flags.get("label").map(String::as_str), Some("server"));

        assert!(parse(&args(&["api-key-issue", "--merchant"])).is_err());
        assert!(parse(&args(&["api-key-issue", "merchant", "m"])).is_err());
        assert!(parse(&args(&["api-key-issue", "--label", "a", "--label", "b"])).is_err());
        assert!(parse(&args(&[])).is_err());
        Ok(())
    }
}
