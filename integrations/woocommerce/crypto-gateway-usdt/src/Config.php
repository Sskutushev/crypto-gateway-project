<?php
/**
 * Validation of the settings an administrator types in.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * Pure validators; the gateway class turns a null into an admin error.
 */
final class Config {

	/**
	 * Normalises the gateway base URL, or returns null when it is not acceptable.
	 *
	 * HTTPS is required because the merchant key travels in every request;
	 * plain HTTP is allowed only for a gateway on the same machine.
	 *
	 * @param string $url Typed URL.
	 */
	public static function normalize_base_url( string $url ): ?string {
		$url = rtrim( trim( $url ), '/' );
		if ( '' === $url ) {
			return null;
		}
		$parts = parse_url( $url );
		if ( ! is_array( $parts ) || ! isset( $parts['scheme'], $parts['host'] ) ) {
			return null;
		}
		if ( isset( $parts['user'] ) || isset( $parts['pass'] ) || isset( $parts['query'] ) || isset( $parts['fragment'] ) ) {
			return null;
		}
		$scheme = strtolower( $parts['scheme'] );
		$host   = strtolower( trim( $parts['host'], '[]' ) );
		if ( 'https' === $scheme ) {
			return $url;
		}
		if ( 'http' === $scheme && in_array( $host, array( 'localhost', '127.0.0.1', '::1' ), true ) ) {
			return $url;
		}
		return null;
	}

	/**
	 * Whether a merchant API key has the length the gateway accepts.
	 *
	 * @param string $key Candidate key.
	 */
	public static function is_valid_api_key( string $key ): bool {
		return 1 === preg_match( '/^[\x21-\x7E]{32,256}$/', $key );
	}

	/**
	 * Whether a value is a UUID in canonical text form.
	 *
	 * @param string $value Candidate.
	 */
	public static function is_uuid( string $value ): bool {
		return 1 === preg_match( '/^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/', $value );
	}

	/**
	 * Parses "USD, eur" into ["USD", "EUR"]; entries that are not codes are dropped.
	 *
	 * @param string $list Comma or space separated codes.
	 * @return list<string>
	 */
	public static function parse_currencies( string $list ): array {
		$codes = array();
		foreach ( preg_split( '/[\s,]+/', strtoupper( $list ) ) ?: array() as $code ) {
			if ( 1 === preg_match( '/^[A-Z]{3}$/', $code ) && ! in_array( $code, $codes, true ) ) {
				$codes[] = $code;
			}
		}
		return $codes;
	}

	/**
	 * A deterministic Idempotency-Key for one write about one order.
	 *
	 * The same order, kind and counter always give the same key, so a retried
	 * request replays instead of creating a second intent or quote. The order
	 * key is random per order, so two stores sharing a merchant do not collide;
	 * only a digest of it leaves the store.
	 *
	 * @param string $order_key   WooCommerce order key.
	 * @param int    $order_id    Order id.
	 * @param string $kind        "intent", "quote" or "cancel".
	 * @param int    $counter     Attempt counter, starting at 1.
	 */
	public static function idempotency_key( string $order_key, int $order_id, string $kind, int $counter ): string {
		$digest = substr( hash( 'sha256', $order_key . '|' . $order_id ), 0, 24 );
		return 'wc-' . $order_id . '-' . $digest . '-' . $kind . '-' . $counter;
	}
}
