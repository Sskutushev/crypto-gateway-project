<?php
/**
 * Creating an intent and a quote for an order.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * Turns an order into a live quote on the hosted payment page.
 *
 * Every write carries an Idempotency-Key derived from the order and a
 * counter that advances only after the gateway answered, so a retried
 * checkout replays the earlier intent or quote instead of creating another.
 */
final class PaymentFlow {

	/** A quote closer than this to its expiry is not handed to a buyer. */
	private const MIN_REMAINING_SECONDS = 60;

	/**
	 * Constructor.
	 *
	 * @param Gateway $gateway Configured gateway.
	 */
	public function __construct( private readonly Gateway $gateway ) {
	}

	/**
	 * Returns the hosted checkout URL for a live quote on this order.
	 *
	 * @param \WC_Order $order Order.
	 * @throws UserFacingError When the payment cannot be started.
	 */
	public function start( \WC_Order $order ): string {
		$currency = strtoupper( $order->get_currency() );
		$reason   = $this->gateway->unavailable_reason( $currency );
		$client   = Plugin::client();
		if ( null !== $reason || null === $client ) {
			throw new UserFacingError( $reason ?? __( 'The USDT payment gateway is not configured.', 'crypto-gateway-usdt' ) );
		}
		try {
			$minor = Money::to_minor_units( self::order_total( $order ), Money::exponent( $currency ) );
		} catch ( \InvalidArgumentException $error ) {
			Plugin::log( 'error', 'CGUSDT_TOTAL_NOT_CONVERTIBLE order=' . $order->get_id() . ' ' . $error->getMessage() );
			throw new UserFacingError( __( 'This order total cannot be paid in USDT. Please choose another payment method.', 'crypto-gateway-usdt' ) );
		}

		try {
			return Plugin::with_order_lock(
				$order->get_id(),
				fn (): string => $this->start_locked( $order, $client, $minor, $currency )
			);
		} catch ( LockBusy $busy ) {
			throw new UserFacingError( __( 'This order is already being processed. Please wait a moment and try again.', 'crypto-gateway-usdt' ) );
		} catch ( ApiException $error ) {
			Plugin::log( 'error', 'CGUSDT_START_FAILED order=' . $order->get_id() . ' ' . $error->getMessage() );
			if ( in_array( $error->error_code, array( 'quote_unavailable', 'rail_stopped' ), true ) ) {
				throw new UserFacingError( __( 'USDT payments are temporarily unavailable. Please try again in a few minutes or choose another payment method.', 'crypto-gateway-usdt' ) );
			}
			throw new UserFacingError( __( 'The payment could not be started. Please try again; you will not be charged twice.', 'crypto-gateway-usdt' ) );
		}
	}

	/**
	 * The order total as a plain decimal string, without passing through a float.
	 *
	 * @param \WC_Order $order Order.
	 * @throws \InvalidArgumentException When the stored total is not a decimal.
	 */
	public static function order_total( \WC_Order $order ): string {
		$total = $order->get_total( 'edit' );
		if ( is_int( $total ) ) {
			return (string) $total;
		}
		if ( is_string( $total ) ) {
			return trim( $total );
		}
		// A float here was set by other code; wc_format_decimal renders it at the store's precision.
		return (string) wc_format_decimal( $total, wc_get_price_decimals() );
	}

	/**
	 * The work under the order lock.
	 *
	 * @param \WC_Order $order    Order.
	 * @param ApiClient $client   Client.
	 * @param string    $minor    Order total in minor units.
	 * @param string    $currency Currency code.
	 * @throws UserFacingError When the order cannot be quoted.
	 * @throws ApiException    When the gateway fails.
	 */
	private function start_locked( \WC_Order $order, ApiClient $client, string $minor, string $currency ): string {
		$intent_id = (string) $order->get_meta( Meta::INTENT_ID, true );

		if ( '' !== $intent_id && ( (string) $order->get_meta( Meta::AMOUNT_MINOR, true ) !== $minor || (string) $order->get_meta( Meta::CURRENCY, true ) !== $currency ) ) {
			$this->retire_intent( $order, $client, $intent_id, 'order total changed' );
			$intent_id = '';
		}

		if ( '' !== $intent_id && '' !== (string) $order->get_meta( Meta::CHECKOUT_TOKEN, true ) ) {
			$live = $this->live_checkout_url( $order, $client );
			if ( null !== $live ) {
				return $live;
			}
			$intent = $client->get_intent( $intent_id );
			$status = is_string( $intent['status'] ?? null ) ? $intent['status'] : '';
			switch ( $status ) {
				case 'awaiting_payment':
					// The stored quote is the live one, just close to its end; the buyer may still pay it.
					return $client->checkout_url( (string) $order->get_meta( Meta::CHECKOUT_TOKEN, true ) );
				case 'expired':
				case 'requires_quote':
					break;
				case 'cancelled':
					// The order is being paid again after its intent was cancelled, e.g. an administrator reopened it.
					$this->forget_intent( $order );
					$intent_id = '';
					break;
				case 'paid':
					Plugin::sync_from_gateway( $order, $client );
					throw new UserFacingError( __( 'This order has already been paid.', 'crypto-gateway-usdt' ) );
				default:
					Plugin::sync_from_gateway( $order, $client );
					throw new UserFacingError( __( 'A payment for this order is being reviewed by the store. Please contact the store instead of paying again.', 'crypto-gateway-usdt' ) );
			}
		}

		if ( '' === $intent_id ) {
			$intent_id = $this->create_intent( $order, $client, $minor, $currency );
		}
		return $this->quote( $order, $client, $intent_id );
	}

	/**
	 * The stored checkout URL when its quote still has time left.
	 *
	 * @param \WC_Order $order  Order.
	 * @param ApiClient $client Client.
	 */
	private function live_checkout_url( \WC_Order $order, ApiClient $client ): ?string {
		$token   = (string) $order->get_meta( Meta::CHECKOUT_TOKEN, true );
		$expires = (int) $order->get_meta( Meta::EXPIRES_AT, true );
		$status  = (string) $order->get_meta( Meta::INTENT_STATUS, true );
		if ( 'awaiting_payment' === $status && $expires - time() > self::MIN_REMAINING_SECONDS ) {
			return $client->checkout_url( $token );
		}
		return null;
	}

	/**
	 * Creates the intent for the current sequence number and stores it.
	 *
	 * @param \WC_Order $order    Order.
	 * @param ApiClient $client   Client.
	 * @param string    $minor    Minor units.
	 * @param string    $currency Currency.
	 * @throws ApiException    When the gateway fails.
	 * @throws UserFacingError When the reference is already taken.
	 */
	private function create_intent( \WC_Order $order, ApiClient $client, string $minor, string $currency ): string {
		$seq       = max( 1, (int) $order->get_meta( Meta::INTENT_SEQ, true ) );
		$prefix    = (string) ( Plugin::settings()['reference_prefix'] ?? 'wc-' );
		$reference = $prefix . $order->get_order_number() . ( $seq > 1 ? '-' . $seq : '' );
		try {
			$intent = $client->create_intent(
				$minor,
				$currency,
				$reference,
				Config::idempotency_key( $order->get_order_key(), $order->get_id(), 'intent', $seq ),
				array(
					'source'   => 'woocommerce',
					'order_id' => (string) $order->get_id(),
				)
			);
		} catch ( ApiException $error ) {
			if ( in_array( $error->error_code, array( 'payment_intent_reference_conflict', 'idempotency_conflict' ), true ) ) {
				Plugin::log( 'critical', 'CGUSDT_REFERENCE_TAKEN order=' . $order->get_id() . ' reference=' . $reference . ' code=' . $error->error_code );
				throw new UserFacingError( __( 'A payment for this order already exists at the gateway. Please contact the store.', 'crypto-gateway-usdt' ) );
			}
			throw $error;
		}
		$intent_id = strtolower( (string) ( $intent['id'] ?? '' ) );
		$amount    = $intent['amount'] ?? array();
		if ( ! Config::is_uuid( $intent_id ) || ( $amount['minor_units'] ?? null ) !== $minor || strtoupper( (string) ( $amount['currency'] ?? '' ) ) !== $currency ) {
			throw new ApiException( 'intent response does not match the request', 200, 'intent_mismatch' );
		}
		$order->update_meta_data( Meta::INTENT_ID, $intent_id );
		$order->update_meta_data( Meta::INTENT_SEQ, (string) $seq );
		$order->update_meta_data( Meta::AMOUNT_MINOR, $minor );
		$order->update_meta_data( Meta::CURRENCY, $currency );
		$order->update_meta_data( Meta::INTENT_STATUS, (string) ( $intent['status'] ?? '' ) );
		$order->add_order_note(
			sprintf(
				/* translators: 1: intent id, 2: gateway reference */
				__( 'USDT payment: intent %1$s created (reference %2$s).', 'crypto-gateway-usdt' ),
				$intent_id,
				$reference
			)
		);
		$order->save();
		return $intent_id;
	}

	/**
	 * Requests a quote for the next attempt and stores what the buyer and admin need.
	 *
	 * @param \WC_Order $order     Order.
	 * @param ApiClient $client    Client.
	 * @param string    $intent_id Intent.
	 * @throws ApiException    When the gateway fails.
	 * @throws UserFacingError When the intent cannot be quoted.
	 */
	private function quote( \WC_Order $order, ApiClient $client, string $intent_id ): string {
		$attempt  = (int) $order->get_meta( Meta::QUOTE_ATTEMPT, true ) + 1;
		$asset_id = (string) ( Plugin::settings()['asset_id'] ?? '' );
		$key      = Config::idempotency_key( $order->get_order_key(), $order->get_id(), 'quote-' . (int) $order->get_meta( Meta::INTENT_SEQ, true ), $attempt );
		try {
			$quote = $client->create_quote( $intent_id, $asset_id, $key );
		} catch ( ApiException $error ) {
			if ( 'payment_intent_not_quotable' === $error->error_code ) {
				$status = Plugin::sync_from_gateway( $order, $client );
				Plugin::log( 'warning', 'CGUSDT_NOT_QUOTABLE order=' . $order->get_id() . ' status=' . $status );
				throw new UserFacingError(
					'paid' === $status
						? __( 'This order has already been paid.', 'crypto-gateway-usdt' )
						: __( 'This order cannot be quoted again right now. If you already sent a payment, please wait; otherwise contact the store.', 'crypto-gateway-usdt' )
				);
			}
			throw $error;
		}

		$token    = (string) ( $quote['checkout_token'] ?? '' );
		$asset    = is_array( $quote['asset'] ?? null ) ? $quote['asset'] : array();
		$raw      = (string) ( $quote['amount_raw'] ?? '' );
		$decimals = $asset['decimals'] ?? null;
		$expires  = strtotime( (string) ( $quote['expires_at'] ?? '' ) );
		$late     = strtotime( (string) ( $quote['late_payment_until'] ?? '' ) );
		if (
			strtolower( (string) ( $quote['payment_intent_id'] ?? '' ) ) !== $intent_id
			|| 1 !== preg_match( '/^[0-9a-f]{64}$/', $token )
			|| 1 !== preg_match( '/^[1-9][0-9]*$/', $raw )
			|| ! is_int( $decimals )
			|| false === $expires
			|| false === $late
		) {
			throw new ApiException( 'quote response is incomplete', 200, 'quote_mismatch' );
		}
		$display = Money::from_raw( $raw, $decimals );

		$order->update_meta_data( Meta::QUOTE_ATTEMPT, (string) $attempt );
		$order->update_meta_data( Meta::ATTEMPT_ID, (string) ( $quote['attempt_id'] ?? '' ) );
		$order->update_meta_data( Meta::CHECKOUT_TOKEN, $token );
		$order->update_meta_data( Meta::EXPIRES_AT, (string) $expires );
		$order->update_meta_data( Meta::LATE_UNTIL, (string) $late );
		$order->update_meta_data( Meta::AMOUNT_RAW, $raw );
		$order->update_meta_data( Meta::AMOUNT_DISPLAY, $display );
		$order->update_meta_data( Meta::ASSET_DECIMALS, (string) $decimals );
		$order->update_meta_data( Meta::ASSET_SYMBOL, (string) ( $asset['symbol'] ?? '' ) );
		$order->update_meta_data( Meta::ASSET_CONTRACT, (string) ( $asset['contract_address'] ?? '' ) );
		$order->update_meta_data( Meta::ASSET_NETWORK, trim( (string) ( $asset['chain'] ?? '' ) . ' ' . (string) ( $asset['network'] ?? '' ) ) );
		$order->update_meta_data( Meta::COLLECTOR, (string) ( $quote['collector_address'] ?? '' ) );
		$order->update_meta_data( Meta::INTENT_STATUS, 'awaiting_payment' );
		$order->add_order_note(
			sprintf(
				/* translators: 1: attempt number, 2: exact amount, 3: token symbol, 4: expiry time */
				__( 'USDT payment: quote %1$d issued for exactly %2$s %3$s, valid until %4$s.', 'crypto-gateway-usdt' ),
				$attempt,
				$display,
				(string) ( $asset['symbol'] ?? '' ),
				gmdate( 'Y-m-d H:i:s', $expires ) . ' UTC'
			)
		);
		if ( ! $order->has_status( 'pending' ) ) {
			$order->update_status( 'pending', __( 'Awaiting USDT payment.', 'crypto-gateway-usdt' ) );
		}
		$order->save();
		return $client->checkout_url( $token );
	}

	/**
	 * Cancels an intent that no longer matches the order and moves to the next sequence number.
	 *
	 * @param \WC_Order $order     Order.
	 * @param ApiClient $client    Client.
	 * @param string    $intent_id Intent.
	 * @param string    $reason    Audit reason.
	 * @throws UserFacingError When the gateway refuses because money or a decision exists.
	 * @throws ApiException    When the gateway fails.
	 */
	private function retire_intent( \WC_Order $order, ApiClient $client, string $intent_id, string $reason ): void {
		$seq = max( 1, (int) $order->get_meta( Meta::INTENT_SEQ, true ) );
		try {
			$client->cancel_intent( $intent_id, $reason, Config::idempotency_key( $order->get_order_key(), $order->get_id(), 'cancel', $seq ) );
		} catch ( ApiException $error ) {
			if ( 'payment_intent_not_cancellable' === $error->error_code ) {
				$order->add_order_note( __( 'USDT payment: the order total changed, but the earlier intent has money or a decision on it and was not replaced. Review it with the gateway operator.', 'crypto-gateway-usdt' ) );
				$order->save();
				throw new UserFacingError( __( 'A payment for this order is being reviewed by the store. Please contact the store instead of paying again.', 'crypto-gateway-usdt' ) );
			}
			throw $error;
		}
		$order->add_order_note(
			sprintf(
				/* translators: %s: intent id */
				__( 'USDT payment: intent %s cancelled because the order total changed.', 'crypto-gateway-usdt' ),
				$intent_id
			)
		);
		$this->forget_intent( $order );
	}

	/**
	 * Clears the intent and quote meta and advances the sequence, so the next intent gets a new reference.
	 *
	 * @param \WC_Order $order Order.
	 */
	private function forget_intent( \WC_Order $order ): void {
		$order->update_meta_data( Meta::INTENT_SEQ, (string) ( max( 1, (int) $order->get_meta( Meta::INTENT_SEQ, true ) ) + 1 ) );
		foreach ( array( Meta::INTENT_ID, Meta::INTENT_STATUS, Meta::AMOUNT_MINOR, Meta::CURRENCY, Meta::QUOTE_ATTEMPT, Meta::ATTEMPT_ID, Meta::CHECKOUT_TOKEN, Meta::EXPIRES_AT, Meta::LATE_UNTIL, Meta::AMOUNT_RAW, Meta::AMOUNT_DISPLAY, Meta::EXPIRED_NOTED, Meta::CONFLICT_NOTED ) as $key ) {
			$order->delete_meta_data( $key );
		}
		$order->save();
	}
}
