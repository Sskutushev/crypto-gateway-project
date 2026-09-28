<?php
/**
 * Order screen additions.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * A meta box with the gateway state of an order and a "Check status now" action.
 */
final class Admin {

	public const CHECK_ACTION = 'cgusdt_check_status';

	/**
	 * Hooks the meta box and the action handler.
	 */
	public static function register(): void {
		add_action( 'add_meta_boxes', array( self::class, 'add_meta_box' ) );
		add_action( 'admin_post_' . self::CHECK_ACTION, array( self::class, 'handle_check' ) );
		add_action( 'admin_notices', array( self::class, 'render_check_notice' ) );
	}

	/**
	 * Adds the box on both the HPOS and the legacy order screen.
	 */
	public static function add_meta_box(): void {
		$screen = function_exists( 'wc_get_page_screen_id' ) ? wc_get_page_screen_id( 'shop-order' ) : 'shop_order';
		add_meta_box( 'cgusdt-payment', __( 'USDT payment', 'crypto-gateway-usdt' ), array( self::class, 'render_meta_box' ), $screen, 'side', 'default' );
	}

	/**
	 * Renders the box.
	 *
	 * @param \WP_Post|\WC_Order $post_or_order Legacy screens pass a post, HPOS passes the order.
	 */
	public static function render_meta_box( $post_or_order ): void {
		$order = $post_or_order instanceof \WC_Order ? $post_or_order : wc_get_order( $post_or_order->ID ?? 0 );
		if ( ! Plugin::is_our_order( $order ) ) {
			echo '<p>' . esc_html__( 'This order was not paid with USDT.', 'crypto-gateway-usdt' ) . '</p>';
			return;
		}
		$intent_id = (string) $order->get_meta( Meta::INTENT_ID, true );
		if ( '' === $intent_id ) {
			echo '<p>' . esc_html__( 'No payment intent has been created yet.', 'crypto-gateway-usdt' ) . '</p>';
			return;
		}
		$token   = (string) $order->get_meta( Meta::CHECKOUT_TOKEN, true );
		$expires = (int) $order->get_meta( Meta::EXPIRES_AT, true );
		$late    = (int) $order->get_meta( Meta::LATE_UNTIL, true );
		$checked = (int) $order->get_meta( Meta::LAST_CHECKED, true );
		$client  = Plugin::client();
		$rows    = array(
			__( 'Intent', 'crypto-gateway-usdt' )           => $intent_id,
			__( 'Gateway status', 'crypto-gateway-usdt' )   => (string) $order->get_meta( Meta::INTENT_STATUS, true ),
			__( 'Quote attempt', 'crypto-gateway-usdt' )    => (string) $order->get_meta( Meta::QUOTE_ATTEMPT, true ),
			__( 'Exact amount', 'crypto-gateway-usdt' )     => trim( (string) $order->get_meta( Meta::AMOUNT_DISPLAY, true ) . ' ' . (string) $order->get_meta( Meta::ASSET_SYMBOL, true ) ),
			__( 'Network', 'crypto-gateway-usdt' )          => (string) $order->get_meta( Meta::ASSET_NETWORK, true ),
			__( 'Token contract', 'crypto-gateway-usdt' )   => (string) $order->get_meta( Meta::ASSET_CONTRACT, true ),
			__( 'Pay to', 'crypto-gateway-usdt' )           => (string) $order->get_meta( Meta::COLLECTOR, true ),
			__( 'Quote expires', 'crypto-gateway-usdt' )    => $expires > 0 ? gmdate( 'Y-m-d H:i', $expires ) . ' UTC' : '',
			__( 'Late payment until', 'crypto-gateway-usdt' ) => $late > 0 ? gmdate( 'Y-m-d H:i', $late ) . ' UTC' : '',
			__( 'Last checked', 'crypto-gateway-usdt' )     => $checked > 0 ? gmdate( 'Y-m-d H:i', $checked ) . ' UTC' : __( 'never', 'crypto-gateway-usdt' ),
		);
		echo '<dl class="cgusdt-meta">';
		foreach ( $rows as $label => $value ) {
			if ( '' === $value ) {
				continue;
			}
			echo '<dt><strong>' . esc_html( $label ) . '</strong></dt><dd style="margin:0 0 6px;word-break:break-all">' . esc_html( $value ) . '</dd>';
		}
		echo '</dl>';
		if ( null !== $client && 1 === preg_match( '/^[0-9a-f]{64}$/', $token ) ) {
			echo '<p><a href="' . esc_url( $client->checkout_url( $token ) ) . '" target="_blank" rel="noreferrer noopener">' . esc_html__( 'Open the payment page', 'crypto-gateway-usdt' ) . '</a></p>';
		}
		if ( current_user_can( 'edit_shop_orders' ) ) {
			$url = wp_nonce_url(
				add_query_arg(
					array(
						'action'   => self::CHECK_ACTION,
						'order_id' => $order->get_id(),
					),
					admin_url( 'admin-post.php' )
				),
				self::CHECK_ACTION . '_' . $order->get_id()
			);
			echo '<p><a class="button" href="' . esc_url( $url ) . '">' . esc_html__( 'Check status now', 'crypto-gateway-usdt' ) . '</a></p>';
		}
	}

	/**
	 * Handles "Check status now".
	 */
	public static function handle_check(): void {
		$order_id = isset( $_GET['order_id'] ) ? absint( wp_unslash( $_GET['order_id'] ) ) : 0; // phpcs:ignore WordPress.Security.NonceVerification.Recommended -- verified below with the id in the action.
		check_admin_referer( self::CHECK_ACTION . '_' . $order_id );
		if ( ! current_user_can( 'edit_shop_orders' ) ) {
			wp_die( esc_html__( 'You are not allowed to check this order.', 'crypto-gateway-usdt' ), '', array( 'response' => 403 ) );
		}
		$order = wc_get_order( $order_id );
		if ( ! Plugin::is_our_order( $order ) || '' === (string) $order->get_meta( Meta::INTENT_ID, true ) ) {
			wp_die( esc_html__( 'This order has no USDT payment intent.', 'crypto-gateway-usdt' ), '', array( 'response' => 404 ) );
		}
		$client = Plugin::client();
		$status = null === $client ? null : Reconciler::check( $order, $client );
		$target = add_query_arg(
			array(
				'cgusdt_checked' => null === $status ? 'failed' : 'ok',
				'cgusdt_status'  => null === $status ? '' : rawurlencode( $status ),
			),
			$order->get_edit_order_url()
		);
		wp_safe_redirect( $target );
		exit;
	}

	/**
	 * Shows the result of a check on the order screen.
	 */
	public static function render_check_notice(): void {
		// phpcs:disable WordPress.Security.NonceVerification.Recommended -- display only, no state change.
		if ( ! isset( $_GET['cgusdt_checked'] ) || ! current_user_can( 'edit_shop_orders' ) ) {
			return;
		}
		$ok     = 'ok' === sanitize_key( wp_unslash( $_GET['cgusdt_checked'] ) );
		$status = isset( $_GET['cgusdt_status'] ) ? sanitize_key( wp_unslash( $_GET['cgusdt_status'] ) ) : '';
		// phpcs:enable
		if ( $ok ) {
			printf(
				'<div class="notice notice-success is-dismissible"><p>%s</p></div>',
				esc_html(
					sprintf(
						/* translators: %s: gateway intent status */
						__( 'USDT payment checked: the gateway reports "%s". The order was updated accordingly.', 'crypto-gateway-usdt' ),
						$status
					)
				)
			);
			return;
		}
		echo '<div class="notice notice-error is-dismissible"><p>' . esc_html__( 'USDT payment check failed. See WooCommerce > Status > Logs (crypto-gateway-usdt) for the reason.', 'crypto-gateway-usdt' ) . '</p></div>';
	}
}
