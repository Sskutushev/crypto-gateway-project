<?php
/**
 * Cart and Checkout Blocks support.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

use Automattic\WooCommerce\Blocks\Payments\Integrations\AbstractPaymentMethodType;

/**
 * Registers the method with the block checkout. The payment itself runs in
 * Gateway::process_payment, as for the classic checkout.
 */
final class BlocksIntegration extends AbstractPaymentMethodType {

	/**
	 * Payment method name; must equal the gateway id.
	 *
	 * @var string
	 */
	protected $name = Plugin::GATEWAY_ID;

	/**
	 * Loads settings.
	 */
	public function initialize(): void {
		$this->settings = Plugin::settings();
	}

	/**
	 * Whether the method can be shown.
	 */
	public function is_active(): bool {
		$gateways = function_exists( 'WC' ) && WC()->payment_gateways() ? WC()->payment_gateways()->payment_gateways() : array();
		$gateway  = $gateways[ Plugin::GATEWAY_ID ] ?? null;
		return $gateway instanceof Gateway && $gateway->is_available();
	}

	/**
	 * Script handles for the checkout.
	 *
	 * @return list<string>
	 */
	public function get_payment_method_script_handles(): array {
		wp_register_script(
			'cgusdt-blocks',
			plugins_url( 'assets/js/blocks.js', CGUSDT_PLUGIN_FILE ),
			array( 'wc-blocks-registry', 'wc-settings', 'wp-element', 'wp-html-entities' ),
			CGUSDT_VERSION,
			true
		);
		return array( 'cgusdt-blocks' );
	}

	/**
	 * Data exposed to the script as getSetting( 'crypto_gateway_usdt_data' ).
	 *
	 * @return array<string,mixed>
	 */
	public function get_payment_method_data(): array {
		$gateways = function_exists( 'WC' ) && WC()->payment_gateways() ? WC()->payment_gateways()->payment_gateways() : array();
		$gateway  = $gateways[ Plugin::GATEWAY_ID ] ?? null;
		return array(
			'title'       => $gateway instanceof Gateway ? $gateway->get_title() : (string) ( $this->settings['title'] ?? '' ),
			'description' => (string) ( $this->settings['description'] ?? '' ),
			'supports'    => $gateway instanceof Gateway ? array_values( array_filter( $gateway->supports, array( $gateway, 'supports' ) ) ) : array( 'products' ),
		);
	}
}
