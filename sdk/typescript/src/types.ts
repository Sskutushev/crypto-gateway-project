/**
 * Wire types of the merchant API, as the gateway serialises them.
 *
 * Every amount is a decimal string of integer units. None of them is ever a
 * JavaScript number: a token amount routinely exceeds 2^53.
 */

/** An integer as a decimal string, for example `"4999000"`. */
export type IntegerString = string;

/** An RFC 3339 timestamp in UTC. */
export type Timestamp = string;

export interface FiatAmount {
  /** ISO 4217 code, upper case. */
  currency: string;
  /** Strictly positive minor units of the currency, as a decimal string. */
  minor_units: IntegerString;
}

export type PaymentIntentStatus =
  | "requires_quote"
  | "awaiting_payment"
  | "partially_paid"
  | "risk_hold"
  | "paid"
  | "expired"
  | "cancelled";

export interface PaymentIntent {
  id: string;
  merchant_id: string;
  amount: FiatAmount;
  status: PaymentIntentStatus;
  reference: string;
  description: string | null;
  metadata: Record<string, unknown>;
  created_at: Timestamp;
  updated_at: Timestamp;
}

export interface CreatePaymentIntentParams {
  /** Strictly positive minor units of the currency, as a decimal string. */
  amount_minor: IntegerString;
  /** ISO 4217 code; the gateway normalises it to upper case. */
  currency: string;
  /** The merchant's own order reference; unique per merchant. */
  reference: string;
  description?: string | null;
  /** Any JSON object. The gateway stores `{}` when it is omitted. */
  metadata?: Record<string, unknown>;
}

export interface CreateQuoteParams {
  /** The allowlisted asset id the operator gave you. */
  asset_id: string;
}

export interface CancelPaymentIntentParams {
  /** At most 500 characters. Kept in the audit trail and sent with the webhook. */
  reason?: string | null;
}

export interface QuoteAsset {
  chain: string;
  network: string;
  chain_environment: "testnet" | "mainnet";
  /** Display only; never identifies the token. */
  symbol: string;
  decimals: number;
  /** The token contract in the chain's display form. Show it to the payer. */
  contract_address: string;
}

export interface IssuedQuote {
  id: string;
  attempt_id: string;
  payment_intent_id: string;
  asset_id: string;
  collector_address_id: string;
  /** Where to pay, in the chain's display form. */
  collector_address: string;
  price_snapshot_id: string;
  quote_policy_id: string;
  rail_health_snapshot_id: string;
  fiat_amount: FiatAmount;
  /** The exact amount to send, in the token's smallest unit. Not a unit more or less. */
  amount_raw: IntegerString;
  rate_numerator: IntegerString;
  rate_denominator: IntegerString;
  /** The price readings behind the rate, as recorded. */
  price_sources: unknown;
  price_observed_at: Timestamp;
  policy_version: string;
  rail_health_observed_at: Timestamp;
  created_at: Timestamp;
  /** After this the quote is no longer offered to payers. */
  expires_at: Timestamp;
  /** Until this, a payment of exactly `amount_raw` is still recorded against the quote. */
  late_payment_until: Timestamp;
  asset: QuoteAsset;
  /** `amount_raw` in whole tokens, exact, trailing zeros dropped. For display only. */
  amount: string;
  /** Opens the hosted payment page for this attempt: `/checkout/{checkout_token}`. */
  checkout_token: string;
}

/**
 * The buyer-facing state of one attempt.
 *
 * `confirming` means a matching transfer is on the chain and not yet final;
 * `needs_review` means money arrived that a person must look at.
 */
export type CheckoutStatus =
  | "paid"
  | "needs_review"
  | "underpaid"
  | "cancelled"
  | "confirming"
  | "expired"
  | "waiting";

export interface CheckoutView {
  status: CheckoutStatus;
  merchant_name: string;
  description: string | null;
  fiat_amount: FiatAmount;
  asset: QuoteAsset;
  collector_address: string;
  amount_raw: IntegerString;
  /** `amount_raw` in whole tokens, exact. */
  amount: string;
  /** Credited to this attempt so far, in whole tokens. */
  received: string;
  expires_at: Timestamp;
  late_payment_until: Timestamp;
  transaction_hash: string | null;
  explorer_url: string | null;
}

/** A write's result: the resource, and whether the gateway replayed a stored answer. */
export interface IdempotentResult<T> {
  data: T;
  /**
   * `true` when this is the stored result of an earlier identical request.
   * `null` when the response carried no `Idempotent-Replayed` header, which
   * the gateway always sends; it is never guessed.
   */
  replayed: boolean | null;
  /** The HTTP status: 201 for a new resource, 200 for a replay or a cancellation. */
  status: number;
}
