<?php
/**
 * Wiring and shared helpers.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * Registers hooks and holds the helpers the gateway, webhook, reconciler and
 * admin screens share.
 */
final class Plugin {

	public const GATEWAY_ID   = 'crypto_gateway_usdt';
	public const TEXT_DOMAIN  = 'crypto-gateway-usdt';
	public const LOG_SOURCE   = 'crypto-gateway-usdt';
	public const SETTINGS_KEY = 'woocommerce_crypto_gateway_usdt_settings';
	public const LOCK_PREFIX  = 'cgusdt_lock_';

	/** Seconds after which a lock left by a crashed request is ignored. */
	private const LOCK_TTL = 120;

	/**
	 * Hooks everything once WooCommerce is loaded.
	 */
	public static function boot(): void {
		if ( ! class_exists( '\WC_Payment_Gateway' ) ) {
			add_action( 'admin_notices', array( self::class, 'missing_woocommerce_notice' ) );
			return;
		}
		add_filter(
			'woocommerce_payment_gateways',
			static function ( array $gateways ): array {
				$gateways[] = Gateway::class;
				return $gateways;
			}
		);
		add_action( 'rest_api_init', array( WebhookController::class, 'register_routes' ) );
		add_action( 'woocommerce_blocks_payment_method_type_registration', array( self::class, 'register_blocks' ) );
		add_filter( 'woocommerce_cancel_unpaid_order', array( self::class, 'keep_unpaid_order_while_payable' ), 10, 2 );
		add_action( 'woocommerce_order_status_cancelled', array( self::class, 'cancel_intent_for_order' ), 10, 2 );
		add_action( 'woocommerce_order_details_after_order_table', array( self::class, 'render_customer_payment_box' ) );
		Reconciler::register();
		Admin::register();
	}

	/**
	 * Admin notice when WooCommerce is not active.
	 */
	public static function missing_woocommerce_notice(): void {
		if ( ! current_user_can( 'activate_plugins' ) ) {
			return;
		}
		echo '<div class="notice notice-error"><p>' . esc_html__( 'USDT (TRC20) payments need WooCommerce 8.0 or later to be active.', 'crypto-gateway-usdt' ) . '</p></div>';
	}

	/**
	 * Registers the Cart/Checkout Blocks integration.
	 *
	 * @param object $registry Automattic\WooCommerce\Blocks\Payments\PaymentMethodRegistry.
	 */
	public static function register_blocks( $registry ): void {
		if ( class_exists( '\Automattic\WooCommerce\Blocks\Payments\Integrations\AbstractPaymentMethodType' ) ) {
			$registry->register( new BlocksIntegration() );
		}
	}

	/**
	 * Saved gateway settings.
	 *
	 * @return array<string,mixed>
	 */
	public static function settings(): array {
		$settings = get_option( self::SETTINGS_KEY, array() );
		return is_array( $settings ) ? $settings : array();
	}

	/**
	 * A client for the configured gateway, or null when the settings are incomplete.
	 */
	public static function client(): ?ApiClient {
		$settings = self::settings();
		$base     = Config::normalize_base_url( (string) ( $settings['base_url'] ?? '' ) );
		$key      = (string) ( $settings['api_key'] ?? '' );
		if ( null === $base || ! Config::is_valid_api_key( $key ) ) {
			return null;
		}
		return new ApiClient( $base, $key );
	}

	/**
	 * Every webhook secret the store currently trusts, newest first.
	 *
	 * @return list<string>
	 */
	public static function webhook_secrets(): array {
		$settings = self::settings();
		$secrets  = array();
		foreach ( array( 'webhook_secret', 'webhook_secret_previous' ) as $field ) {
			$value = strtolower( trim( (string) ( $settings[ $field ] ?? '' ) ) );
			if ( Signature::is_valid_secret( $value ) ) {
				$secrets[] = $value;
			}
		}
		return $secrets;
	}

	/**
	 * Minutes an unpaid order waits before the reconciliation job polls it.
	 */
	public static function reconcile_after_minutes(): int {
		$minutes = (int) ( self::settings()['reconcile_after_minutes'] ?? 10 );
		return max( 2, min( 1440, $minutes ) );
	}

	/**
	 * Logs without context that could carry a key, a secret or a signature.
	 *
	 * @param string $level   WC_Logger level.
	 * @param string $message Message; callers pass ids and codes only.
	 */
	public static function log( string $level, string $message ): void {
		if ( function_exists( 'wc_get_logger' ) ) {
			wc_get_logger()->log( $level, $message, array( 'source' => self::LOG_SOURCE ) );
		}
	}

	/**
	 * Whether an order was placed with this gateway.
	 *
	 * @param mixed $order Candidate.
	 */
	public static function is_our_order( $order ): bool {
		return $order instanceof \WC_Order && self::GATEWAY_ID === $order->get_payment_method();
	}

	/**
	 * The order that owns an intent id, if any.
	 *
	 * @param string $intent_id Intent id.
	 */
	public static function find_order_by_intent( string $intent_id ): ?\WC_Order {
		if ( ! Config::is_uuid( $intent_id ) ) {
			return null;
		}
		$orders = wc_get_orders(
			array(
				'limit'          => 2,
				'payment_method' => self::GATEWAY_ID,
				'meta_key'       => Meta::INTENT_ID, // phpcs:ignore WordPress.DB.SlowDBQuery.slow_db_query_meta_key
				'meta_value'     => strtolower( $intent_id ), // phpcs:ignore WordPress.DB.SlowDBQuery.slow_db_query_meta_value
				'status'         => array_keys( wc_get_order_statuses() ),
			)
		);
		if ( 1 !== count( $orders ) ) {
			if ( count( $orders ) > 1 ) {
				self::log( 'critical', 'CGUSDT_INTENT_ON_TWO_ORDERS intent=' . $intent_id );
			}
			return null;
		}
		return $orders[0] instanceof \WC_Order ? $orders[0] : null;
	}

	/**
	 * Runs a callback while holding a per-order lock.
	 *
	 * The webhook, the reconciler and the admin action may reach the same
	 * order at once; the lock keeps payment_complete and status changes from
	 * interleaving. INSERT IGNORE on the unique option name is atomic, which
	 * add_option() is not.
	 *
	 * @template T
	 * @param int             $order_id Order id.
	 * @param callable(): T   $callback Work.
	 * @return T
	 * @throws LockBusy When another request holds the lock.
	 */
	public static function with_order_lock( int $order_id, callable $callback ) {
		global $wpdb;
		$name = self::LOCK_PREFIX . $order_id;
		$now  = time();
		// phpcs:ignore WordPress.DB.DirectDatabaseQuery
		$taken = $wpdb->query( $wpdb->prepare( "INSERT IGNORE INTO {$wpdb->options} (option_name, option_value, autoload) VALUES (%s, %s, 'no')", $name, (string) ( $now + self::LOCK_TTL ) ) );
		if ( 1 !== $taken ) {
			// phpcs:ignore WordPress.DB.DirectDatabaseQuery
			$expires = (int) $wpdb->get_var( $wpdb->prepare( "SELECT option_value FROM {$wpdb->options} WHERE option_name = %s", $name ) );
			if ( $expires >= $now ) {
				throw new LockBusy( 'order ' . $order_id . ' is being processed' );
			}
			// A stale lock is taken over only if nobody else took it over first.
			// phpcs:ignore WordPress.DB.DirectDatabaseQuery
			$taken = $wpdb->query( $wpdb->prepare( "UPDATE {$wpdb->options} SET option_value = %s WHERE option_name = %s AND option_value = %s", (string) ( $now + self::LOCK_TTL ), $name, (string) $expires ) );
			if ( 1 !== $taken ) {
				throw new LockBusy( 'order ' . $order_id . ' is being processed' );
			}
		}
		try {
			return $callback();
		} finally {
			// phpcs:ignore WordPress.DB.DirectDatabaseQuery
			$wpdb->delete( $wpdb->options, array( 'option_name' => $name ) );
			wp_cache_delete( $name, 'options' );
		}
	}

	/**
	 * Reads the intent and applies its status to the order. Caller holds the lock.
	 *
	 * @param \WC_Order $order  Order with an intent.
	 * @param ApiClient $client Client.
	 * @return string The gateway status that was applied.
	 * @throws ApiException When the gateway cannot be read.
	 */
	public static function sync_from_gateway( \WC_Order $order, ApiClient $client ): string {
		$intent_id = (string) $order->get_meta( Meta::INTENT_ID, true );
		$intent    = $client->get_intent( $intent_id );
		$status    = is_string( $intent['status'] ?? null ) ? $intent['status'] : '';
		if ( strtolower( (string) ( $intent['id'] ?? '' ) ) !== strtolower( $intent_id ) ) {
			throw new ApiException( 'intent read returned another id', 200, 'intent_mismatch' );
		}
		$action = OrderSync::action_for_status( $status, $intent_id );
		if ( Action::COMPLETE === $action->kind && ! $order->is_paid() ) {
			$action = new Action( Action::COMPLETE, 'paid', self::transaction_id( $order, $client, $intent_id ), $action->detail );
		}
		$order->update_meta_data( Meta::LAST_CHECKED, (string) time() );
		OrderSync::apply( $order, $action );
		return $status;
	}

	/**
	 * The chain transaction hash of a settled latest attempt, or the intent id.
	 *
	 * The hash is shown on the hosted page of the latest attempt only; a
	 * payment settled on an earlier attempt is recorded under the intent id,
	 * and the order note always names the intent.
	 *
	 * @param \WC_Order $order     Order.
	 * @param ApiClient $client    Client.
	 * @param string    $intent_id Intent id.
	 */
	public static function transaction_id( \WC_Order $order, ApiClient $client, string $intent_id ): string {
		$token = (string) $order->get_meta( Meta::CHECKOUT_TOKEN, true );
		if ( 1 !== preg_match( '/^[0-9a-f]{64}$/', $token ) ) {
			return $intent_id;
		}
		try {
			$view = $client->checkout_view( $token );
		} catch ( ApiException $error ) {
			self::log( 'warning', 'CGUSDT_TX_HASH_UNREAD order=' . $order->get_id() . ' ' . $error->getMessage() );
			return $intent_id;
		}
		$hash = $view['transaction_hash'] ?? null;
		if ( 'paid' === ( $view['status'] ?? null ) && is_string( $hash ) && 1 === preg_match( '/^(0x)?[0-9a-fA-F]{64}$/', $hash ) ) {
			return $hash;
		}
		return $intent_id;
	}

	/**
	 * Keeps WooCommerce's unpaid-order cleanup away from an order that can still be paid.
	 *
	 * Until the late-payment window of the last quote ends, an exact payment can
	 * still settle the intent; cancelling the order before then would turn
	 * received money into an operator case. An order that never got a quote
	 * has nothing to wait for.
	 *
	 * @param bool      $cancel Whether WooCommerce wants to cancel.
	 * @param \WC_Order $order  Order.
	 */
	public static function keep_unpaid_order_while_payable( $cancel, $order ): bool {
		if ( ! $cancel || ! self::is_our_order( $order ) || '' === (string) $order->get_meta( Meta::INTENT_ID, true ) ) {
			return (bool) $cancel;
		}
		$late_until = (int) $order->get_meta( Meta::LATE_UNTIL, true );
		return 0 === $late_until || $late_until < time();
	}

	/**
	 * Cancels the intent when the order is cancelled in the store.
	 *
	 * @param int            $order_id Order id.
	 * @param \WC_Order|null $order    Order.
	 */
	public static function cancel_intent_for_order( $order_id, $order = null ): void {
		$order = $order instanceof \WC_Order ? $order : wc_get_order( $order_id );
		if ( ! self::is_our_order( $order ) ) {
			return;
		}
		$intent_id = (string) $order->get_meta( Meta::INTENT_ID, true );
		$known     = (string) $order->get_meta( Meta::INTENT_STATUS, true );
		if ( '' === $intent_id || in_array( $known, array( 'cancelled', 'paid', 'partially_paid' ), true ) ) {
			return;
		}
		$client = self::client();
		if ( null === $client ) {
			$order->add_order_note( __( 'USDT payment: the intent could not be cancelled at the gateway because the gateway is not configured.', 'crypto-gateway-usdt' ) );
			return;
		}
		$seq = max( 1, (int) $order->get_meta( Meta::INTENT_SEQ, true ) );
		try {
			$client->cancel_intent( $intent_id, 'WooCommerce order cancelled', Config::idempotency_key( $order->get_order_key(), $order->get_id(), 'cancel', $seq ) );
			$order->update_meta_data( Meta::INTENT_STATUS, 'cancelled' );
			$order->add_order_note( __( 'USDT payment: the payment intent was cancelled at the gateway.', 'crypto-gateway-usdt' ) );
		} catch ( ApiException $error ) {
			self::log( 'error', 'CGUSDT_CANCEL_FAILED order=' . $order->get_id() . ' ' . $error->getMessage() );
			$order->add_order_note(
				'payment_intent_not_cancellable' === $error->error_code
					? __( 'USDT payment: the gateway refused to cancel the intent because money or an operator decision exists for it. Review it with the gateway operator before refunding or restocking.', 'crypto-gateway-usdt' )
					: sprintf(
						/* translators: %s: gateway error code */
						__( 'USDT payment: cancelling the intent at the gateway failed (%s). A payment may still arrive; check the order status later.', 'crypto-gateway-usdt' ),
						$error->error_code
					)
			);
		}
		$order->save();
	}

	/**
	 * The payment box on the thank-you and view-order pages.
	 *
	 * @param \WC_Order $order Order.
	 */
	public static function render_customer_payment_box( $order ): void {
		if ( ! self::is_our_order( $order ) || $order->is_paid() ) {
			return;
		}
		$token   = (string) $order->get_meta( Meta::CHECKOUT_TOKEN, true );
		$expires = (int) $order->get_meta( Meta::EXPIRES_AT, true );
		$client  = self::client();
		echo '<section class="cgusdt-payment"><h2>' . esc_html__( 'USDT payment', 'crypto-gateway-usdt' ) . '</h2>';
		if ( $order->has_status( 'on-hold' ) ) {
			echo '<p>' . esc_html__( 'Your payment is being reviewed by the store. You will be contacted if anything else is needed.', 'crypto-gateway-usdt' ) . '</p>';
		} elseif ( $order->has_status( 'pending' ) && null !== $client && 1 === preg_match( '/^[0-9a-f]{64}$/', $token ) && $expires > time() ) {
			$amount = (string) $order->get_meta( Meta::AMOUNT_DISPLAY, true );
			$symbol = (string) $order->get_meta( Meta::ASSET_SYMBOL, true );
			echo '<p>' . esc_html(
				sprintf(
					/* translators: 1: exact token amount, 2: token symbol */
					__( 'Send exactly %1$s %2$s on the payment page. The amount is exact: a different amount is not matched automatically.', 'crypto-gateway-usdt' ),
					$amount,
					$symbol
				)
			) . '</p>';
			echo '<p><a class="button" rel="noreferrer noopener" target="_blank" href="' . esc_url( $client->checkout_url( $token ) ) . '">' . esc_html__( 'Open the payment page', 'crypto-gateway-usdt' ) . '</a></p>';
		} elseif ( $order->needs_payment() ) {
			echo '<p>' . esc_html__( 'The payment quote has expired. If you already paid the exact amount, the order updates on its own; otherwise request a new quote.', 'crypto-gateway-usdt' ) . '</p>';
			echo '<p><a class="button" href="' . esc_url( $order->get_checkout_payment_url() ) . '">' . esc_html__( 'Pay again', 'crypto-gateway-usdt' ) . '</a></p>';
		}
		echo '</section>';
	}
}
