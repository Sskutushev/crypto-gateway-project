<?php
/**
 * Exact conversions between decimal amounts and integer minor units.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * String arithmetic only: a float never touches an amount.
 */
final class Money {

	/** The gateway stores minor units as a signed 64-bit integer. */
	public const MAX_MINOR_UNITS = '9223372036854775807';

	/**
	 * ISO 4217 currencies whose minor unit is not two decimals.
	 *
	 * @var array<string,int>
	 */
	private const EXPONENTS = array(
		'BIF' => 0,
		'CLP' => 0,
		'DJF' => 0,
		'GNF' => 0,
		'ISK' => 0,
		'JPY' => 0,
		'KMF' => 0,
		'KRW' => 0,
		'PYG' => 0,
		'RWF' => 0,
		'UGX' => 0,
		'UYI' => 0,
		'VND' => 0,
		'VUV' => 0,
		'XAF' => 0,
		'XOF' => 0,
		'XPF' => 0,
		'BHD' => 3,
		'IQD' => 3,
		'JOD' => 3,
		'KWD' => 3,
		'LYD' => 3,
		'OMR' => 3,
		'TND' => 3,
		'CLF' => 4,
		'UYW' => 4,
	);

	/**
	 * The number of decimals in one major unit of an ISO 4217 currency.
	 *
	 * @param string $currency Three-letter code.
	 * @throws \InvalidArgumentException When the code is not three letters.
	 */
	public static function exponent( string $currency ): int {
		$code = strtoupper( $currency );
		if ( 1 !== preg_match( '/^[A-Z]{3}$/', $code ) ) {
			throw new \InvalidArgumentException( 'currency must be a three-letter code' );
		}
		return self::EXPONENTS[ $code ] ?? 2;
	}

	/**
	 * Converts a non-negative decimal string into minor units as a decimal string.
	 *
	 * Refuses instead of rounding: an amount with more precision than the
	 * currency has is not an amount the buyer can be charged.
	 *
	 * @param string $amount   Decimal such as "49.99", "1000", "0.500".
	 * @param int    $exponent Decimals of the currency.
	 * @throws \InvalidArgumentException When the amount is malformed, too precise, zero or too large.
	 */
	public static function to_minor_units( string $amount, int $exponent ): string {
		if ( $exponent < 0 || $exponent > 18 ) {
			throw new \InvalidArgumentException( 'unsupported currency exponent' );
		}
		if ( 1 !== preg_match( '/^([0-9]+)(?:\.([0-9]+))?$/', $amount, $parts ) ) {
			throw new \InvalidArgumentException( 'amount must be a plain non-negative decimal' );
		}
		$whole    = $parts[1];
		$fraction = $parts[2] ?? '';
		if ( strlen( $fraction ) > $exponent ) {
			$excess = substr( $fraction, $exponent );
			if ( '' !== trim( $excess, '0' ) ) {
				throw new \InvalidArgumentException( 'amount has more decimals than the currency' );
			}
			$fraction = substr( $fraction, 0, $exponent );
		}
		$minor = ltrim( $whole . str_pad( $fraction, $exponent, '0' ), '0' );
		if ( '' === $minor ) {
			throw new \InvalidArgumentException( 'amount must be positive' );
		}
		if ( self::compare( $minor, self::MAX_MINOR_UNITS ) > 0 ) {
			throw new \InvalidArgumentException( 'amount exceeds the gateway range' );
		}
		return $minor;
	}

	/**
	 * Renders an integer string of smallest units as an exact decimal.
	 *
	 * Trailing zeros of the fraction are dropped; nothing is rounded.
	 *
	 * @param string $raw      Integer string, e.g. "4999000".
	 * @param int    $decimals Decimals of the unit, e.g. 6 for USDT.
	 * @throws \InvalidArgumentException When the input is not a non-negative integer string.
	 */
	public static function from_raw( string $raw, int $decimals ): string {
		if ( 1 !== preg_match( '/^[0-9]+$/', $raw ) || $decimals < 0 || $decimals > 77 ) {
			throw new \InvalidArgumentException( 'raw amount must be a non-negative integer string' );
		}
		$raw = ltrim( $raw, '0' );
		if ( '' === $raw ) {
			return '0';
		}
		if ( 0 === $decimals ) {
			return $raw;
		}
		$padded   = str_pad( $raw, $decimals + 1, '0', STR_PAD_LEFT );
		$whole    = substr( $padded, 0, -$decimals );
		$fraction = rtrim( substr( $padded, -$decimals ), '0' );
		return '' === $fraction ? $whole : $whole . '.' . $fraction;
	}

	/**
	 * Compares two non-negative integer strings without leading zeros.
	 *
	 * @param string $a Left.
	 * @param string $b Right.
	 */
	private static function compare( string $a, string $b ): int {
		return strlen( $a ) <=> strlen( $b ) ?: strcmp( $a, $b ) <=> 0;
	}
}
