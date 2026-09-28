<?php
/**
 * Order meta keys written by the plugin.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * Every key is read and written through the order object, so HPOS and the
 * legacy posts table behave the same.
 */
final class Meta {
	public const INTENT_ID        = '_cgusdt_intent_id';
	public const INTENT_SEQ       = '_cgusdt_intent_seq';
	public const INTENT_STATUS    = '_cgusdt_intent_status';
	public const AMOUNT_MINOR     = '_cgusdt_amount_minor';
	public const CURRENCY         = '_cgusdt_currency';
	public const QUOTE_ATTEMPT    = '_cgusdt_quote_attempt';
	public const ATTEMPT_ID       = '_cgusdt_attempt_id';
	public const CHECKOUT_TOKEN   = '_cgusdt_checkout_token';
	public const EXPIRES_AT       = '_cgusdt_expires_at';
	public const LATE_UNTIL       = '_cgusdt_late_payment_until';
	public const AMOUNT_RAW       = '_cgusdt_amount_raw';
	public const AMOUNT_DISPLAY   = '_cgusdt_amount';
	public const ASSET_SYMBOL     = '_cgusdt_asset_symbol';
	public const ASSET_DECIMALS   = '_cgusdt_asset_decimals';
	public const ASSET_CONTRACT   = '_cgusdt_asset_contract';
	public const ASSET_NETWORK    = '_cgusdt_asset_network';
	public const COLLECTOR        = '_cgusdt_collector_address';
	public const EVENTS           = '_cgusdt_events';
	public const LAST_CHECKED     = '_cgusdt_last_checked';
	public const EXPIRED_NOTED    = '_cgusdt_expired_noted';
	public const CONFLICT_NOTED   = '_cgusdt_conflict_noted';
}
