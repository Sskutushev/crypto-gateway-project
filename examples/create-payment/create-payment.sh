#!/usr/bin/env bash
# Create a payment intent, quote it, show the payer what to send, and poll.
#
# Requires curl and jq.
#   GATEWAY_URL=https://gateway.example.com MERCHANT_KEY=... ASSET_ID=<uuid> \
#   ASSET_DECIMALS=6 ORDER_REF=order-1042 AMOUNT_MINOR=4999 CURRENCY=USD \
#   ./create-payment.sh
#
# Every write carries an Idempotency-Key derived from a stable id, so running
# the script again for the same order replays the first results instead of
# creating anything new. Retries reuse the same key and the same body.

set -euo pipefail

: "${GATEWAY_URL:?}" "${MERCHANT_KEY:?}" "${ASSET_ID:?}" "${ORDER_REF:?}" "${AMOUNT_MINOR:?}" "${CURRENCY:?}"
# The quote does not carry the token's decimals (USDT TRC20 has 6). Take it
# from the operator's asset configuration.
: "${ASSET_DECIMALS:?}"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$1" | sha256sum | cut -c1-40
  else
    printf '%s' "$1" | shasum -a 256 | cut -c1-40
  fi
}

# 16 to 128 characters of [A-Za-z0-9_-].
idempotency_key() { printf '%s-%s' "$1" "$(sha256 "$2")"; }

# Integer string of smallest units -> exact decimal string. No floating point.
format_units() {
  local raw=$1 decimals=$2
  raw=$(printf '%s' "$raw" | sed 's/^0*//'); raw=${raw:-0}
  if [ "$decimals" -eq 0 ]; then printf '%s\n' "$raw"; return; fi
  while [ "${#raw}" -le "$decimals" ]; do raw="0$raw"; done
  local whole=${raw:0:${#raw}-decimals} fraction=${raw:${#raw}-decimals}
  fraction=$(printf '%s' "$fraction" | sed 's/0*$//')
  if [ -z "$fraction" ]; then printf '%s\n' "$whole"; else printf '%s.%s\n' "$whole" "$fraction"; fi
}

# curl retries network failures, 408, 429 and 5xx. A 4xx is an answer, not an
# outage, and is shown as it is.
api() {
  local method=$1 path=$2 key=${3:-} body=${4:-}
  local args=(-sS --fail-with-body --retry 5 --retry-connrefused --max-time 20
    -X "$method" "$GATEWAY_URL$path" -H "Authorization: Bearer $MERCHANT_KEY")
  if [ -n "$key" ]; then args+=(-H "Idempotency-Key: $key"); fi
  if [ -n "$body" ]; then args+=(-H 'Content-Type: application/json' --data "$body"); fi
  local out
  if ! out=$(curl "${args[@]}"); then
    printf 'request failed: %s %s\n%s\n' "$method" "$path" "$out" >&2
    return 1
  fi
  printf '%s' "$out"
}

# 1. The obligation. A replay answers 200 with the same intent.
intent_body=$(jq -nc --arg a "$AMOUNT_MINOR" --arg c "$CURRENCY" --arg r "$ORDER_REF" \
  '{amount_minor: $a, currency: $c, reference: $r}')
intent=$(api POST /v1/payment-intents "$(idempotency_key intent "$ORDER_REF")" "$intent_body")
intent_id=$(jq -r .id <<<"$intent")
echo "intent $intent_id status=$(jq -r .status <<<"$intent")"

# 2. The quote. There is no route to read a quote back: replaying this same
#    key is how the quote is fetched again.
quote=$(api POST "/v1/payment-intents/$intent_id/quotes" "$(idempotency_key quote "$intent_id")" \
  "$(jq -nc --arg asset "$ASSET_ID" '{asset_id: $asset}')")
amount_raw=$(jq -r .amount_raw <<<"$quote")
late_until=$(jq -r .late_payment_until <<<"$quote")

# 3. What the payer sees. The amount is exact; do not round it.
echo "Send exactly $(format_units "$amount_raw" "$ASSET_DECIMALS") (amount_raw $amount_raw)"
echo "  to     $(jq -r .collector_address <<<"$quote")"
echo "  before $(jq -r .expires_at <<<"$quote")"
echo "Any other amount is not matched automatically."

# 4. Poll. The signed webhook is the primary signal; this is a fallback.
late_until_epoch=$(date -u -d "$late_until" +%s 2>/dev/null ||
  { t=${late_until%Z}; date -u -j -f '%Y-%m-%dT%H:%M:%S' "${t%%.*}" +%s; })
while :; do
  status=$(api GET "/v1/payment-intents/$intent_id" | jq -r .status)
  echo "status=$status"
  case $status in
    paid) echo "paid: fulfil the order once, keyed by the intent id"; exit 0 ;;
    cancelled) exit 0 ;;
    expired)
      # Inside late_payment_until an operator may still honour an exact late payment.
      if [ "$(date -u +%s)" -gt "$late_until_epoch" ]; then
        echo "expired: create a new intent with a new reference to try again"; exit 0
      fi ;;
  esac
  sleep 10
done
