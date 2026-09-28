<?php
/**
 * Maps gateway facts to order state, idempotently.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * The one place that changes an order because of the gateway.
 *
 * The webhook, the reconciliation job and the admin "check now" action all
 * end here, so a missed, repeated or out-of-order fact leads to the same
 * order state. Uses only WC_Order methods, which lets tests pass a fake.
 */
final class OrderSync {

	public const EVENT_PAID           = 'payment_intent.paid';
	public const EVENT_PARTIALLY_PAID = 'payment_intent.partially_paid';
	public const EVENT_CANCELLED      = 'payment_intent.cancelled';
	public const EVENT_OVERPAID       = 'OVERPAID';
	public const EVENT_TEST           = 'webhook.test';

	/** Remembered event ids per order; older ones are dropped. */
	public const MAX_REMEMBERED_EVENTS = 100;

	/**
	 * The action a webhook event asks for, or null for a type this plugin does not act on.
	 *
	 * @param string              $type       Event type.
	 * @param array<string,mixed> $attributes data.attributes of the envelope.
	 * @param string              $intent_id  The intent the event is about.
	 */
	public static function action_for_event( string $type, array $attributes, string $intent_id ): ?Action {
		$transfer = is_string( $attributes['transfer_id'] ?? null ) ? $attributes['transfer_id'] : '';
		switch ( $type ) {
			case self::EVENT_PAID:
				return new Action( Action::COMPLETE, 'paid', $intent_id, $transfer );
			case self::EVENT_PARTIALLY_PAID:
				return new Action( Action::HOLD_PARTIAL, 'partially_paid', '', $transfer );
			case self::EVENT_CANCELLED:
				return new Action( Action::CANCEL, 'cancelled' );
			case self::EVENT_OVERPAID:
				$remainder = $attributes['remainder_raw'] ?? null;
				if ( ! is_string( $remainder ) || 1 !== preg_match( '/^[0-9]+$/', $remainder ) ) {
					return null;
				}
				return new Action( Action::OVERPAID, null, '', $remainder );
			default:
				return null;
		}
	}

	/**
	 * The action an intent status read from the gateway asks for.
	 *
	 * @param string $status    PaymentIntent.status.
	 * @param string $intent_id Intent id, used as the transaction id when paid.
	 */
	public static function action_for_status( string $status, string $intent_id ): Action {
		switch ( $status ) {
			case 'paid':
				return new Action( Action::COMPLETE, 'paid', $intent_id );
			case 'partially_paid':
				return new Action( Action::HOLD_PARTIAL, 'partially_paid' );
			case 'cancelled':
				return new Action( Action::CANCEL, 'cancelled' );
			case 'expired':
				return new Action( Action::EXPIRED, 'expired' );
			case 'requires_quote':
			case 'awaiting_payment':
			case 'risk_hold':
				return new Action( Action::NONE, $status );
			default:
				// An unknown status is recorded nowhere: it must not become a fact about the order.
				return new Action( Action::NONE, null );
		}
	}

	/**
	 * Applies an action. Safe to call any number of times with the same action.
	 *
	 * @param object $order  WC_Order or a test double with the same methods.
	 * @param Action $action What to do.
	 * @return bool Whether the order changed beyond bookkeeping.
	 */
	public static function apply( object $order, Action $action ): bool {
		$previous = (string) $order->get_meta( Meta::INTENT_STATUS, true );
		if ( null !== $action->intent_status ) {
			// Written before any status change so hooks fired by that change see the gateway's view.
			$order->update_meta_data( Meta::INTENT_STATUS, $action->intent_status );
		}
		// A gateway status already recorded on the order was acted on before. Acting
		// again would undo what an administrator did since, such as reopening a
		// cancelled order or resolving a partial payment by hand.
		$repeated = null !== $action->intent_status && $previous === $action->intent_status;

		$changed = false;
		switch ( $action->kind ) {
			case Action::COMPLETE:
				$changed = ! $repeated && self::complete( $order, $action );
				break;
			case Action::HOLD_PARTIAL:
				if ( $repeated ) {
					break;
				}
				if ( $order->is_paid() ) {
					self::note_conflict_once( $order, 'partially_paid' );
				} elseif ( ! $order->has_status( 'on-hold' ) ) {
					$order->update_status(
						'on-hold',
						self::with_detail(
							__( 'USDT payment: the gateway operator accepted a partial payment. The remainder is still owed; do not ship. Resolve with the gateway operator.', 'crypto-gateway-usdt' ),
							$action->detail
						)
					);
					$changed = true;
				}
				break;
			case Action::CANCEL:
				if ( $repeated ) {
					break;
				}
				if ( $order->is_paid() ) {
					self::note_conflict_once( $order, 'cancelled' );
				} elseif ( ! $order->has_status( 'cancelled' ) ) {
					$order->update_status( 'cancelled', __( 'USDT payment: the payment intent was cancelled at the gateway.', 'crypto-gateway-usdt' ) );
					$changed = true;
				}
				break;
			case Action::EXPIRED:
				$attempt = (string) $order->get_meta( Meta::QUOTE_ATTEMPT, true );
				if ( ! $order->is_paid() && $order->has_status( 'pending' ) && $attempt !== (string) $order->get_meta( Meta::EXPIRED_NOTED, true ) ) {
					$order->add_order_note( __( 'USDT payment: the quote expired with no payment. The customer can pay again from the order page, which requests a new quote.', 'crypto-gateway-usdt' ) );
					$order->update_meta_data( Meta::EXPIRED_NOTED, $attempt );
					$changed = true;
				}
				break;
			case Action::OVERPAID:
				$order->add_order_note( self::overpaid_note( $order, $action->detail ) );
				$changed = true;
				break;
			case Action::NONE:
				break;
		}

		$order->save();
		return $changed;
	}

	/**
	 * Whether this order already processed a webhook event.
	 *
	 * @param object $order    Order.
	 * @param string $event_id Event id.
	 */
	public static function seen_event( object $order, string $event_id ): bool {
		$seen = $order->get_meta( Meta::EVENTS, true );
		return is_array( $seen ) && in_array( $event_id, $seen, true );
	}

	/**
	 * Records an event id on the order, keeping the newest MAX_REMEMBERED_EVENTS.
	 *
	 * @param object $order    Order.
	 * @param string $event_id Event id.
	 */
	public static function remember_event( object $order, string $event_id ): void {
		$seen = $order->get_meta( Meta::EVENTS, true );
		$seen = is_array( $seen ) ? array_values( $seen ) : array();
		if ( in_array( $event_id, $seen, true ) ) {
			return;
		}
		$seen[] = $event_id;
		if ( count( $seen ) > self::MAX_REMEMBERED_EVENTS ) {
			$seen = array_slice( $seen, -self::MAX_REMEMBERED_EVENTS );
		}
		$order->update_meta_data( Meta::EVENTS, $seen );
	}

	/**
	 * Completes the order once.
	 *
	 * @param object $order  Order.
	 * @param Action $action COMPLETE action.
	 */
	private static function complete( object $order, Action $action ): bool {
		if ( $order->is_paid() ) {
			return false;
		}
		if ( ! $order->has_status( array( 'pending', 'on-hold', 'failed', 'cancelled' ) ) ) {
			self::note_conflict_once( $order, 'paid' );
			return false;
		}
		$order->add_order_note(
			self::with_detail( __( 'USDT payment settled by the gateway.', 'crypto-gateway-usdt' ), $action->detail )
		);
		$order->payment_complete( $action->transaction_id );
		return true;
	}

	/**
	 * Adds a note once per conflicting gateway status; never changes the order status.
	 *
	 * @param object $order  Order.
	 * @param string $status Gateway status that disagrees with the order.
	 */
	private static function note_conflict_once( object $order, string $status ): void {
		if ( $status === (string) $order->get_meta( Meta::CONFLICT_NOTED, true ) ) {
			return;
		}
		$order->add_order_note(
			sprintf(
				/* translators: 1: gateway intent status, 2: WooCommerce order status */
				__( 'USDT payment: the gateway reports "%1$s" but the order is "%2$s". The order was not changed; review it with the gateway operator.', 'crypto-gateway-usdt' ),
				$status,
				$order->get_status()
			)
		);
		$order->update_meta_data( Meta::CONFLICT_NOTED, $status );
	}

	/**
	 * Note text for an overpayment remainder, in whole tokens when the decimals are known.
	 *
	 * @param object $order         Order.
	 * @param string $remainder_raw Remainder in the token's smallest unit.
	 */
	private static function overpaid_note( object $order, string $remainder_raw ): string {
		$decimals = $order->get_meta( Meta::ASSET_DECIMALS, true );
		$symbol   = (string) $order->get_meta( Meta::ASSET_SYMBOL, true );
		$amount   = sprintf(
			/* translators: %s: amount in the token's smallest unit */
			__( '%s raw units', 'crypto-gateway-usdt' ),
			$remainder_raw
		);
		if ( is_numeric( $decimals ) && (string) (int) $decimals === (string) $decimals ) {
			$amount = Money::from_raw( $remainder_raw, (int) $decimals ) . ( '' !== $symbol ? ' ' . $symbol : '' ) . ' (' . $amount . ')';
		}
		return sprintf(
			/* translators: %s: overpaid remainder */
			__( 'USDT payment: the buyer overpaid by %s. The gateway never refunds; the operator records what happens to the remainder.', 'crypto-gateway-usdt' ),
			$amount
		);
	}

	/**
	 * Appends a transfer id or other detail to a note.
	 *
	 * @param string $note   Note.
	 * @param string $detail Detail, may be empty.
	 */
	private static function with_detail( string $note, string $detail ): string {
		if ( '' === $detail ) {
			return $note;
		}
		return $note . ' ' . sprintf(
			/* translators: %s: gateway transfer id */
			__( 'Transfer: %s.', 'crypto-gateway-usdt' ),
			$detail
		);
	}
}
