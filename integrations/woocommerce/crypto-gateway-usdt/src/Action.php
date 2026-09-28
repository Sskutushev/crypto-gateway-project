<?php
/**
 * What a gateway fact means for a WooCommerce order.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * An order change derived from a webhook event or an intent status.
 */
final class Action {

	public const COMPLETE     = 'complete';
	public const HOLD_PARTIAL = 'hold_partial';
	public const CANCEL       = 'cancel';
	public const EXPIRED      = 'expired';
	public const OVERPAID     = 'overpaid';
	public const NONE         = 'none';

	/**
	 * Constructor.
	 *
	 * @param string      $kind           One of the constants.
	 * @param string|null $intent_status  The intent status this fact implies, when it implies one.
	 * @param string      $transaction_id Recorded by payment_complete for COMPLETE.
	 * @param string      $detail         Extra fact for the order note (transfer id, remainder).
	 */
	public function __construct(
		public readonly string $kind,
		public readonly ?string $intent_status,
		public readonly string $transaction_id = '',
		public readonly string $detail = ''
	) {
	}
}
