<?php
/**
 * The webhook endpoint.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * POST /wp-json/crypto-gateway-usdt/v1/webhook.
 *
 * Authentication is the signature over the raw body, so the route is public
 * to WordPress and refuses everything that does not verify. A 2xx is sent
 * only after the order change is saved; any failure answers 5xx and the
 * gateway retries the same event id later.
 */
final class WebhookController {

	public const ROUTE_NAMESPACE = 'crypto-gateway-usdt/v1';
	public const ROUTE           = '/webhook';

	/**
	 * Registers the route.
	 */
	public static function register_routes(): void {
		register_rest_route(
			self::ROUTE_NAMESPACE,
			self::ROUTE,
			array(
				'methods'             => 'POST',
				'callback'            => array( self::class, 'handle' ),
				// The signature check in handle() is the authentication; WordPress users play no part.
				'permission_callback' => '__return_true',
			)
		);
	}

	/**
	 * Handles one delivery.
	 *
	 * @param \WP_REST_Request $request Request.
	 */
	public static function handle( \WP_REST_Request $request ): \WP_REST_Response {
		$raw     = $request->get_body();
		$secrets = Plugin::webhook_secrets();
		if ( array() === $secrets ) {
			Plugin::log( 'error', 'CGUSDT_WEBHOOK_NO_SECRET a delivery arrived but no signing secret is configured' );
			return self::reply( 503, 'not_configured' );
		}
		$result = Signature::verify( $request->get_header( 'gateway_signature' ), $raw, $secrets, time() );
		if ( Signature::OK !== $result ) {
			Plugin::log( 'warning', 'CGUSDT_WEBHOOK_REFUSED reason=' . $result );
			return self::reply( 401, 'invalid_signature' );
		}

		$event = json_decode( $raw, true, 32, JSON_BIGINT_AS_STRING );
		if ( ! is_array( $event ) || ! is_string( $event['id'] ?? null ) || ! Config::is_uuid( $event['id'] ) || ! is_string( $event['type'] ?? null ) ) {
			Plugin::log( 'error', 'CGUSDT_WEBHOOK_UNREADABLE a signed body is not an event envelope' );
			return self::reply( 400, 'invalid_event' );
		}
		$event_id = $event['id'];
		$type     = $event['type'];
		$log_type = (string) preg_replace( '/[^A-Za-z0-9._-]/', '', substr( $type, 0, 64 ) );
		$data     = is_array( $event['data'] ?? null ) ? $event['data'] : array();
		$attrs    = is_array( $data['attributes'] ?? null ) ? $data['attributes'] : array();

		if ( OrderSync::EVENT_TEST === $type ) {
			Plugin::log( 'info', 'CGUSDT_WEBHOOK_TEST event=' . $event_id );
			return self::reply( 200, 'test_received' );
		}

		$intent_id = is_string( $attrs['payment_intent_id'] ?? null )
			? $attrs['payment_intent_id']
			: ( 'payment_intent' === ( $data['object'] ?? null ) && is_string( $data['id'] ?? null ) ? $data['id'] : '' );
		$action    = OrderSync::action_for_event( $type, $attrs, strtolower( $intent_id ) );
		if ( null === $action ) {
			// Acknowledged so an event type added to the gateway later is not retried forever; nothing changes.
			Plugin::log( 'notice', 'CGUSDT_WEBHOOK_IGNORED type=' . $log_type . ' event=' . $event_id );
			return self::reply( 200, 'ignored' );
		}

		$order = Plugin::find_order_by_intent( $intent_id );
		if ( null === $order ) {
			Plugin::log( 'warning', 'CGUSDT_WEBHOOK_NO_ORDER type=' . $log_type . ' intent=' . sanitize_key( $intent_id ) . ' event=' . $event_id );
			return self::reply( 200, 'no_matching_order' );
		}

		try {
			$outcome = Plugin::with_order_lock(
				$order->get_id(),
				static function () use ( $order, $action, $event_id ): string {
					// Re-read under the lock: another request may have changed the order since the lookup.
					$fresh = wc_get_order( $order->get_id() );
					if ( ! $fresh instanceof \WC_Order ) {
						throw new \RuntimeException( 'order disappeared' );
					}
					if ( OrderSync::seen_event( $fresh, $event_id ) ) {
						return 'duplicate';
					}
					if ( Action::COMPLETE === $action->kind ) {
						$action = self::confirmed_completion( $fresh, $action );
					}
					OrderSync::remember_event( $fresh, $event_id );
					OrderSync::apply( $fresh, $action );
					return 'processed';
				}
			);
		} catch ( LockBusy $busy ) {
			return self::reply( 503, 'busy' );
		} catch ( \Throwable $error ) {
			Plugin::log( 'error', 'CGUSDT_WEBHOOK_FAILED event=' . $event_id . ' order=' . $order->get_id() . ' ' . $error->getMessage() );
			return self::reply( 500, 'processing_failed' );
		}
		Plugin::log( 'info', 'CGUSDT_WEBHOOK_' . strtoupper( $outcome ) . ' type=' . $log_type . ' event=' . $event_id . ' order=' . $order->get_id() );
		return self::reply( 200, $outcome );
	}

	/**
	 * Confirms a paid event against the intent before the order is completed.
	 *
	 * The signature proves the gateway sent the event; the read proves the
	 * intent is still paid now, so a stale or misrouted event cannot complete
	 * an order. A failed read throws and the delivery is retried.
	 *
	 * @param \WC_Order $order  Order.
	 * @param Action    $action COMPLETE action from the event.
	 * @throws \RuntimeException When the gateway cannot be read or disagrees.
	 */
	private static function confirmed_completion( \WC_Order $order, Action $action ): Action {
		if ( $order->is_paid() ) {
			return $action;
		}
		$client = Plugin::client();
		if ( null === $client ) {
			throw new \RuntimeException( 'gateway not configured; cannot confirm a paid event' );
		}
		$intent = $client->get_intent( $action->transaction_id );
		if ( 'paid' !== ( $intent['status'] ?? null ) ) {
			throw new \RuntimeException( 'paid event but the intent reads as ' . sanitize_key( (string) ( $intent['status'] ?? 'unknown' ) ) );
		}
		return new Action(
			Action::COMPLETE,
			'paid',
			Plugin::transaction_id( $order, $client, $action->transaction_id ),
			$action->detail
		);
	}

	/**
	 * A small JSON answer; never echoes the request.
	 *
	 * @param int    $status HTTP status.
	 * @param string $result Machine-readable outcome.
	 */
	private static function reply( int $status, string $result ): \WP_REST_Response {
		return new \WP_REST_Response( array( 'result' => $result ), $status );
	}
}
