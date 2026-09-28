<?php
/**
 * Removes the plugin's options. Orders and their meta are kept: they are the
 * store's payment records.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

defined( 'WP_UNINSTALL_PLUGIN' ) || exit;

delete_option( 'woocommerce_crypto_gateway_usdt_settings' );
wp_clear_scheduled_hook( 'cgusdt_reconcile' );

global $wpdb;
// Per-order locks left by a request that died mid-way.
// phpcs:ignore WordPress.DB.DirectDatabaseQuery
$wpdb->query( $wpdb->prepare( "DELETE FROM {$wpdb->options} WHERE option_name LIKE %s", $wpdb->esc_like( 'cgusdt_lock_' ) . '%' ) );
