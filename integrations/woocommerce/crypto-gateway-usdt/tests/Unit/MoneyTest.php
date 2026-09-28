<?php
/**
 * Exact minor-unit conversion.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt\Tests\Unit;

use CryptoGatewayUsdt\Money;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;

final class MoneyTest extends TestCase {

	public function test_exponents_follow_iso_4217(): void {
		self::assertSame( 2, Money::exponent( 'USD' ) );
		self::assertSame( 2, Money::exponent( 'eur' ) );
		self::assertSame( 0, Money::exponent( 'JPY' ) );
		self::assertSame( 0, Money::exponent( 'KRW' ) );
		self::assertSame( 3, Money::exponent( 'KWD' ) );
		self::assertSame( 3, Money::exponent( 'BHD' ) );
		self::assertSame( 4, Money::exponent( 'CLF' ) );
	}

	public function test_an_exponent_needs_a_three_letter_code(): void {
		$this->expectException( \InvalidArgumentException::class );
		Money::exponent( 'US' );
	}

	/**
	 * @return array<string,array{string,int,string}>
	 */
	public static function conversions(): array {
		return array(
			'two decimals'                 => array( '49.99', 2, '4999' ),
			'two decimals, whole'          => array( '50', 2, '5000' ),
			'two decimals, one digit'      => array( '0.5', 2, '50' ),
			'two decimals, trailing zeros' => array( '12.3400', 2, '1234' ),
			'smallest two-decimal amount'  => array( '0.01', 2, '1' ),
			'zero decimals'                => array( '1500', 0, '1500' ),
			'zero decimals, .00 suffix'    => array( '1500.00', 0, '1500' ),
			'three decimals'               => array( '1.234', 3, '1234' ),
			'three decimals, short'        => array( '1.2', 3, '1200' ),
			'leading zeros'                => array( '0007.10', 2, '710' ),
			'float-hostile value'          => array( '0.29', 2, '29' ),
			'beyond float precision'       => array( '90071992547409.93', 2, '9007199254740993' ),
			'largest the gateway accepts'  => array( '92233720368547758.07', 2, '9223372036854775807' ),
		);
	}

	#[DataProvider( 'conversions' )]
	public function test_amounts_convert_exactly( string $amount, int $exponent, string $expected ): void {
		self::assertSame( $expected, Money::to_minor_units( $amount, $exponent ) );
	}

	/**
	 * @return array<string,array{string,int}>
	 */
	public static function refusals(): array {
		return array(
			'more precision than the currency' => array( '49.999', 2 ),
			'fraction on a zero-decimal code'  => array( '1500.5', 0 ),
			'zero'                             => array( '0.00', 2 ),
			'negative'                         => array( '-1.00', 2 ),
			'exponent notation'                => array( '1e3', 2 ),
			'thousands separator'              => array( '1,000.00', 2 ),
			'empty'                            => array( '', 2 ),
			'whitespace'                       => array( ' 1.00', 2 ),
			'bare dot'                         => array( '1.', 2 ),
			'above signed 64-bit'              => array( '92233720368547758.08', 2 ),
			'far above signed 64-bit'          => array( '100000000000000000000', 0 ),
		);
	}

	#[DataProvider( 'refusals' )]
	public function test_amounts_that_cannot_be_charged_exactly_are_refused( string $amount, int $exponent ): void {
		$this->expectException( \InvalidArgumentException::class );
		Money::to_minor_units( $amount, $exponent );
	}

	public function test_raw_token_amounts_render_exactly(): void {
		self::assertSame( '4.999', Money::from_raw( '4999000', 6 ) );
		self::assertSame( '0.000001', Money::from_raw( '1', 6 ) );
		self::assertSame( '12', Money::from_raw( '12000000', 6 ) );
		self::assertSame( '0', Money::from_raw( '0', 6 ) );
		self::assertSame( '123456789012345678901234567890.123456', Money::from_raw( '123456789012345678901234567890123456', 6 ) );
		self::assertSame( '77', Money::from_raw( '77', 0 ) );
	}

	public function test_a_raw_amount_must_be_an_integer_string(): void {
		$this->expectException( \InvalidArgumentException::class );
		Money::from_raw( '1.5', 6 );
	}
}
