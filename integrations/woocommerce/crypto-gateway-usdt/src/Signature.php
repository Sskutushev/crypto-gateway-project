<?php
/**
 * Webhook signature verification.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * Verifies `Gateway-Signature: t=<unix>,v1=<hex>[,v1=<hex>...]` over the raw body.
 *
 * The HMAC key is the hex-decoded secret (32 bytes), not the 64-character text.
 * Pure: no WordPress calls, so it is unit-tested directly.
 */
final class Signature {

	public const DEFAULT_TOLERANCE_SECONDS = 300;

	public const OK                  = 'ok';
	public const MISSING_HEADER      = 'missing_header';
	public const NO_SECRET           = 'no_secret_configured';
	public const MALFORMED_HEADER    = 'malformed_header';
	public const OUTSIDE_TOLERANCE   = 'timestamp_outside_tolerance';
	public const SIGNATURE_MISMATCH  = 'signature_mismatch';

	/**
	 * Parses the header into a timestamp and every v1 value.
	 *
	 * Unknown keys are ignored so a future scheme can sit beside v1. Exactly one
	 * `t` and at least one well-formed `v1` are required.
	 *
	 * @param string $header Header value.
	 * @return array{0:int,1:list<string>}|null
	 */
	public static function parse_header( string $header ): ?array {
		$timestamp  = null;
		$signatures = array();
		foreach ( explode( ',', $header ) as $part ) {
			$position = strpos( $part, '=' );
			if ( false === $position ) {
				return null;
			}
			$key   = trim( substr( $part, 0, $position ) );
			$value = trim( substr( $part, $position + 1 ) );
			if ( '' === $key ) {
				return null;
			}
			if ( 't' === $key ) {
				if ( null !== $timestamp || 1 !== preg_match( '/^[0-9]{1,15}$/', $value ) ) {
					return null;
				}
				$timestamp = (int) $value;
			} elseif ( 'v1' === $key ) {
				if ( 1 !== preg_match( '/^[0-9a-f]{64}$/', $value ) ) {
					return null;
				}
				$signatures[] = $value;
			}
		}
		if ( null === $timestamp || array() === $signatures ) {
			return null;
		}
		return array( $timestamp, $signatures );
	}

	/**
	 * Whether a string is a usable signing secret: 64 hex characters.
	 *
	 * @param string $secret Candidate secret.
	 */
	public static function is_valid_secret( string $secret ): bool {
		return 1 === preg_match( '/^[0-9a-fA-F]{64}$/', $secret );
	}

	/**
	 * Raw HMAC-SHA256 bytes over `<t>.<raw body>`.
	 *
	 * @param string $secret_hex 64 hex characters.
	 * @param int    $timestamp  Unix seconds from the header.
	 * @param string $raw_body   The exact bytes received.
	 * @throws \InvalidArgumentException When the secret is not 64 hex characters.
	 */
	public static function compute( string $secret_hex, int $timestamp, string $raw_body ): string {
		if ( ! self::is_valid_secret( $secret_hex ) ) {
			throw new \InvalidArgumentException( 'a webhook secret must be 64 hex characters' );
		}
		return hash_hmac( 'sha256', $timestamp . '.' . $raw_body, (string) hex2bin( $secret_hex ), true );
	}

	/**
	 * Verifies a delivery. Valid when any trusted secret matches any v1 value.
	 *
	 * @param string|null  $header    Gateway-Signature header.
	 * @param string       $raw_body  Raw request body, before any JSON parsing.
	 * @param list<string> $secrets   Every secret this store trusts, newest first.
	 * @param int          $now       Current unix time.
	 * @param int          $tolerance Allowed clock distance in seconds.
	 * @return string One of the result constants; only OK means verified.
	 */
	public static function verify( ?string $header, string $raw_body, array $secrets, int $now, int $tolerance = self::DEFAULT_TOLERANCE_SECONDS ): string {
		if ( null === $header || '' === $header ) {
			return self::MISSING_HEADER;
		}
		$secrets = array_values( array_filter( $secrets, array( self::class, 'is_valid_secret' ) ) );
		if ( array() === $secrets ) {
			return self::NO_SECRET;
		}
		$parsed = self::parse_header( $header );
		if ( null === $parsed ) {
			return self::MALFORMED_HEADER;
		}
		list( $timestamp, $candidates ) = $parsed;
		if ( abs( $now - $timestamp ) > $tolerance ) {
			return self::OUTSIDE_TOLERANCE;
		}
		$matched = false;
		// Every pair is compared, so the time taken does not reveal which secret or position matched.
		foreach ( $secrets as $secret ) {
			$expected = self::compute( $secret, $timestamp, $raw_body );
			foreach ( $candidates as $candidate ) {
				if ( hash_equals( $expected, (string) hex2bin( $candidate ) ) ) {
					$matched = true;
				}
			}
		}
		return $matched ? self::OK : self::SIGNATURE_MISMATCH;
	}
}
