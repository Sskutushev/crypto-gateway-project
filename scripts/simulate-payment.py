#!/usr/bin/env python3
"""Pays a payment intent on the chain simulator and follows it to its outcome.

Testing only. It talks to a gateway whose chain sources point at
tools/chain-simulator, and it refuses a quote that names a mainnet asset.

    MERCHANT_KEY=... python3 scripts/simulate-payment.py --create --amount-minor 4999
    MERCHANT_KEY=... python3 scripts/simulate-payment.py --intent <uuid>
    MERCHANT_KEY=... OPERATOR_KEY=... python3 scripts/simulate-payment.py --create --mode wrong-amount
    MERCHANT_KEY=... python3 scripts/simulate-payment.py --create --mode failed

Modes:
  exact         pays the quoted amount_raw; expects the intent to become paid.
  wrong-amount  pays amount_raw - 1 (or --amount-raw); expects the transfer in
                GET /v1/operator/unmatched-transfers (needs OPERATOR_KEY).
  failed        pays the exact amount in a transaction that reverts; expects
                the intent to stay unpaid after a good payment would have settled.

A quote is requested with the idempotency key ``simulate-quote-<intent id>``
unless --quote-idempotency-key names another, so running the script again for
the same intent replays the same quote instead of asking for a new one.

Standard library only. Exit status 0 means the expected outcome was observed.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid
from datetime import datetime, timezone
from typing import Any

SIMULATOR_ASSET_ID = "00000000-0000-7000-8000-000000000111"


class HttpFailure(Exception):
    def __init__(self, method: str, url: str, status: int, body: str) -> None:
        super().__init__(f"{method} {url} answered {status}: {body[:500]}")
        self.status = status


def call(method: str, url: str, body: Any = None, headers: dict[str, str] | None = None) -> Any:
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, method=method, headers={
        "Content-Type": "application/json", **(headers or {})})
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            raw = response.read()
    except urllib.error.HTTPError as error:
        raise HttpFailure(method, url, error.code, error.read().decode("utf-8", "replace")) from error
    return json.loads(raw) if raw else None


def say(message: str) -> None:
    stamp = datetime.now(timezone.utc).strftime("%H:%M:%S")
    print(f"[{stamp}] {message}", flush=True)


def bearer(key: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {key}"}


def feed_evidence(api: str, operator_key: str, asset_id: str) -> None:
    """Posts price and rail-health evidence from two groups. Simulated evidence."""
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    readings = [
        {"source_key": group, "provider_group": group, "rate_numerator": "10000",
         "rate_denominator": "1", "observed_at": now}
        for group in ("sim-price-a", "sim-price-b")
    ]
    call("POST", f"{api}/v1/operator/price-snapshots",
         {"asset_id": asset_id, "fiat_currency": "USD", "readings": readings}, bearer(operator_key))
    call("POST", f"{api}/v1/operator/rail-health",
         {"asset_id": asset_id, "health": "healthy"}, bearer(operator_key))
    say("fed simulated price evidence (2 groups, 10000 raw units per cent) and rail health")


def simulator_state(simulator: str, token_headers: dict[str, str]) -> dict[str, Any]:
    return call("GET", f"{simulator}/simulator/state", headers=token_headers)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--intent", help="an existing payment intent id")
    target.add_argument("--create", action="store_true", help="create a new intent first")
    parser.add_argument("--amount-minor", default="4999", help="fiat minor units for --create (decimal string)")
    parser.add_argument("--currency", default="USD")
    parser.add_argument("--asset", default=os.environ.get("GATEWAY_ASSET_ID", SIMULATOR_ASSET_ID))
    parser.add_argument("--mode", choices=("exact", "wrong-amount", "failed"), default="exact")
    parser.add_argument("--amount-raw", help="the raw amount to pay in wrong-amount mode")
    parser.add_argument("--quote-idempotency-key")
    parser.add_argument("--feed-evidence", action="store_true",
                        help="post simulated price and rail-health evidence first (needs OPERATOR_KEY)")
    parser.add_argument("--api", default=os.environ.get("GATEWAY_API_URL", "http://127.0.0.1:8080"))
    parser.add_argument("--simulator", default=os.environ.get("SIMULATOR_URL", "http://127.0.0.1:8090"))
    parser.add_argument("--timeout", type=float, default=300.0, help="seconds to wait for the outcome")
    args = parser.parse_args()

    merchant_key = os.environ.get("MERCHANT_KEY")
    operator_key = os.environ.get("OPERATOR_KEY")
    if not merchant_key:
        print("MERCHANT_KEY is required", file=sys.stderr)
        return 2
    if (args.feed_evidence or args.mode == "wrong-amount") and not operator_key:
        print("OPERATOR_KEY is required for --feed-evidence and --mode wrong-amount", file=sys.stderr)
        return 2
    token = os.environ.get("SIMULATOR_CONTROL_TOKEN")
    token_headers = {"X-Simulator-Token": token} if token else {}
    api = args.api.rstrip("/")
    simulator = args.simulator.rstrip("/")

    if args.feed_evidence:
        assert operator_key is not None
        feed_evidence(api, operator_key, args.asset)

    if args.create:
        reference = f"sim-{uuid.uuid4().hex[:12]}"
        intent = call("POST", f"{api}/v1/payment-intents",
                      {"amount_minor": args.amount_minor, "currency": args.currency, "reference": reference},
                      {**bearer(merchant_key), "Idempotency-Key": f"simulate-intent-{reference}"})
        intent_id = intent["id"]
        say(f"intent {intent_id} created: {args.amount_minor} {args.currency} minor units, status={intent['status']}")
    else:
        intent_id = args.intent
        intent = call("GET", f"{api}/v1/payment-intents/{intent_id}", headers=bearer(merchant_key))
        say(f"intent {intent_id} status={intent['status']}")

    quote_key = args.quote_idempotency_key or f"simulate-quote-{intent_id}"
    quote = call("POST", f"{api}/v1/payment-intents/{intent_id}/quotes", {"asset_id": args.asset},
                 {**bearer(merchant_key), "Idempotency-Key": quote_key})
    asset = quote["asset"]
    if asset.get("chain_environment") != "testnet" or asset.get("network") == "mainnet":
        print(f"refused: the quote names {asset.get('network')}/{asset.get('chain_environment')}; "
              "the simulator is for testnet rails only", file=sys.stderr)
        return 2
    amount_raw = quote["amount_raw"]
    say(f"quote {quote['id']}: pay amount_raw={amount_raw} of {asset['contract_address']} "
        f"({asset['symbol']}, {asset['network']}/{asset['chain_environment']}) to {quote['collector_address']}, "
        f"expires {quote['expires_at']}")

    pay = amount_raw
    if args.mode == "wrong-amount":
        pay = args.amount_raw or str(int(amount_raw) - 1)
        if pay == amount_raw:
            print("--amount-raw equals the quoted amount; that is not a wrong amount", file=sys.stderr)
            return 2
    transfer = {"to": quote["collector_address"], "amount_raw": pay, "contract": asset["contract_address"],
                "status": "failed" if args.mode == "failed" else "success"}
    mined = call("POST", f"{simulator}/simulator/transfers", transfer, token_headers)
    say(f"simulator mined tx {mined['tx_id']} in block {mined['block_number']} "
        f"(amount_raw={pay}, status={mined['status']}; solidified at block {mined['solidified_at_block']})")

    deadline = time.monotonic() + args.timeout
    last_status = None
    if args.mode == "exact":
        while time.monotonic() < deadline:
            status = call("GET", f"{api}/v1/payment-intents/{intent_id}", headers=bearer(merchant_key))["status"]
            if status != last_status:
                say(f"intent {intent_id} status={status}")
                last_status = status
            if status == "paid":
                return 0
            time.sleep(2)
        say(f"TIMEOUT: intent {intent_id} is stuck in status={last_status}")
        return 1

    if args.mode == "wrong-amount":
        assert operator_key is not None
        while time.monotonic() < deadline:
            page = call("GET", f"{api}/v1/operator/unmatched-transfers?limit=100", headers=bearer(operator_key))
            match = next((item for item in page["items"] if item["tx_hash"] == mined["tx_id"]), None)
            if match is not None:
                status = call("GET", f"{api}/v1/payment-intents/{intent_id}", headers=bearer(merchant_key))["status"]
                say(f"unmatched transfer {match['id']}: tx {match['tx_hash']} amount_raw={match['amount_raw']} "
                    f"finality={match['finality_state']}; intent {intent_id} status={status}")
                return 0 if status != "paid" else 1
            time.sleep(2)
        say(f"TIMEOUT: tx {mined['tx_id']} never appeared in the unmatched queue")
        return 1

    # failed: wait until a good payment in the same block would have settled
    # (solidified, then min_confirmations more), then check nothing happened.
    settle_block = mined["solidified_at_block"] + (mined["solidified_at_block"] - mined["block_number"]) + 3
    while time.monotonic() < deadline:
        if simulator_state(simulator, token_headers)["solid_head"] >= settle_block:
            time.sleep(10)
            status = call("GET", f"{api}/v1/payment-intents/{intent_id}", headers=bearer(merchant_key))["status"]
            say(f"solid head passed block {settle_block}; intent {intent_id} status={status} "
                "(a reverted transfer pays nothing)")
            return 0 if status != "paid" else 1
        time.sleep(2)
    say("TIMEOUT: the simulator did not reach the settle block in time")
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except HttpFailure as failure:
        print(f"error: {failure}", file=sys.stderr)
        sys.exit(1)
