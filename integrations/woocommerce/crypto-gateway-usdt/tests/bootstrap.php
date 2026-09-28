<?php
/**
 * Test bootstrap: the pure classes need only the translation functions.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

require __DIR__ . '/../vendor/autoload.php';

if ( ! function_exists( '__' ) ) {
	/**
	 * Identity translation for tests.
	 *
	 * @param string $text   Text.
	 * @param string $domain Text domain.
	 */
	function __( string $text, string $domain = 'default' ): string { // phpcs:ignore
		return $text;
	}
}
