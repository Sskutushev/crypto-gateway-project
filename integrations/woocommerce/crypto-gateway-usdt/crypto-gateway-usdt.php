<?php
/**
 * Plugin Name:          USDT (TRC20) for WooCommerce — self-hosted gateway
 * Description:          Accept USDT on TRON through your own self-hosted, non-custodial payment gateway.
 * Version:              0.1.0
 * Requires at least:    6.4
 * Requires PHP:         8.1
 * Requires Plugins:     woocommerce
 * WC requires at least: 8.0
 * WC tested up to:      11.1
 * Author:               Crypto Gateway contributors
 * License:              Apache-2.0
 * License URI:          https://www.apache.org/licenses/LICENSE-2.0
 * Text Domain:          crypto-gateway-usdt
 * Domain Path:          /languages
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

defined( 'ABSPATH' ) || exit;

define( 'CGUSDT_VERSION', '0.1.0' );
define( 'CGUSDT_PLUGIN_FILE', __FILE__ );

spl_autoload_register(
	static function ( string $class_name ): void {
		$prefix = 'CryptoGatewayUsdt\\';
		if ( 0 !== strncmp( $class_name, $prefix, strlen( $prefix ) ) ) {
			return;
		}
		$relative = substr( $class_name, strlen( $prefix ) );
		if ( 1 !== preg_match( '/^[A-Za-z0-9]+$/', $relative ) ) {
			return;
		}
		$file = __DIR__ . '/src/' . $relative . '.php';
		if ( is_readable( $file ) ) {
			require_once $file;
		}
	}
);

add_action(
	'before_woocommerce_init',
	static function (): void {
		if ( class_exists( '\Automattic\WooCommerce\Utilities\FeaturesUtil' ) ) {
			\Automattic\WooCommerce\Utilities\FeaturesUtil::declare_compatibility( 'custom_order_tables', __FILE__, true );
			\Automattic\WooCommerce\Utilities\FeaturesUtil::declare_compatibility( 'cart_checkout_blocks', __FILE__, true );
		}
	}
);

add_action(
	'init',
	static function (): void {
		load_plugin_textdomain( 'crypto-gateway-usdt', false, dirname( plugin_basename( __FILE__ ) ) . '/languages' );
	}
);

add_action( 'plugins_loaded', array( \CryptoGatewayUsdt\Plugin::class, 'boot' ), 20 );

register_activation_hook(
	__FILE__,
	static function (): void {
		add_filter( 'cron_schedules', array( \CryptoGatewayUsdt\Reconciler::class, 'add_schedule' ) ); // phpcs:ignore WordPress.WP.CronInterval.ChangeDetected
		\CryptoGatewayUsdt\Reconciler::ensure_scheduled();
	}
);

register_deactivation_hook( __FILE__, array( \CryptoGatewayUsdt\Reconciler::class, 'unschedule' ) );
