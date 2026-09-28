<?php
/**
 * Settings validation and idempotency keys.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt\Tests\Unit;

use CryptoGatewayUsdt\Config;
use PHPUnit\Framework\TestCase;

final class ConfigTest extends TestCase {

	public function test_https_base_urls_are_accepted_and_normalised(): void {
		self::assertSame( 'https://pay.example.com', Config::normalize_base_url( ' https://pay.example.com/ ' ) );
		self::assertSame( 'https://pay.example.com/gw', Config::normalize_base_url( 'https://pay.example.com/gw/' ) );
		self::assertSame( 'https://pay.example.com:8443', Config::normalize_base_url( 'https://pay.example.com:8443' ) );
	}

	public function test_plain_http_is_allowed_only_on_localhost(): void {
		self::assertSame( 'http://localhost:8080', Config::normalize_base_url( 'http://localhost:8080' ) );
		self::assertSame( 'http://127.0.0.1:8080', Config::normalize_base_url( 'http://127.0.0.1:8080' ) );
		self::assertSame( 'http://[::1]:8080', Config::normalize_base_url( 'http://[::1]:8080' ) );
		self::assertNull( Config::normalize_base_url( 'http://pay.example.com' ) );
		self::assertNull( Config::normalize_base_url( 'http://localhost.example.com' ) );
	}

	public function test_urls_with_credentials_query_or_other_schemes_are_refused(): void {
		self::assertNull( Config::normalize_base_url( 'https://user:pass@pay.example.com' ) );
		self::assertNull( Config::normalize_base_url( 'https://pay.example.com/?a=1' ) );
		self::assertNull( Config::normalize_base_url( 'https://pay.example.com/#x' ) );
		self::assertNull( Config::normalize_base_url( 'ftp://pay.example.com' ) );
		self::assertNull( Config::normalize_base_url( 'pay.example.com' ) );
		self::assertNull( Config::normalize_base_url( '' ) );
	}

	public function test_currency_lists_are_parsed_and_deduplicated(): void {
		self::assertSame( array( 'USD', 'EUR' ), Config::parse_currencies( 'usd, EUR usd,,x, dollars' ) );
		self::assertSame( array(), Config::parse_currencies( '' ) );
	}

	public function test_api_keys_and_uuids(): void {
		self::assertTrue( Config::is_valid_api_key( 'gw_' . str_repeat( 'a', 64 ) ) );
		self::assertFalse( Config::is_valid_api_key( 'short' ) );
		self::assertFalse( Config::is_valid_api_key( str_repeat( 'a', 40 ) . ' ' ) );
		self::assertTrue( Config::is_uuid( '00000000-0000-0000-0000-000000000004' ) );
		self::assertFalse( Config::is_uuid( '00000000000000000000000000000004' ) );
	}

	public function test_idempotency_keys_are_deterministic_distinct_and_valid(): void {
		$first = Config::idempotency_key( 'wc_order_abc', 1042, 'intent', 1 );
		self::assertSame( $first, Config::idempotency_key( 'wc_order_abc', 1042, 'intent', 1 ) );
		self::assertNotSame( $first, Config::idempotency_key( 'wc_order_abc', 1042, 'intent', 2 ) );
		self::assertNotSame( $first, Config::idempotency_key( 'wc_order_abc', 1042, 'quote-1', 1 ) );
		self::assertNotSame( $first, Config::idempotency_key( 'wc_order_xyz', 1042, 'intent', 1 ), 'another store with the same order id' );
		self::assertStringNotContainsString( 'wc_order_abc', $first );
		foreach ( array( $first, Config::idempotency_key( 'k', PHP_INT_MAX, 'quote-99', 999999 ) ) as $key ) {
			self::assertMatchesRegularExpression( '/^[A-Za-z0-9_-]{16,128}$/', $key );
		}
	}
}
