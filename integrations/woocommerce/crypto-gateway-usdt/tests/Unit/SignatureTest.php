<?php
/**
 * Signature verification against the gateway's fixed vector.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt\Tests\Unit;

use CryptoGatewayUsdt\Signature;
use PHPUnit\Framework\TestCase;

/**
 * The vector is the one pinned in examples/webhook-receiver-python/test_receiver.py,
 * derived exactly as crates/gateway-domain/src/webhook.rs signs.
 */
final class SignatureTest extends TestCase {

	private const SECRET_V1    = '6187fe179459663943dbad68a397beea67e72e6194e31e0a8eb7d4b921267fe2';
	private const SECRET_V2    = '825c1d2c393df266b4234708f316bff2b2a49f5277516bb6eee4b90111b45182';
	private const TIMESTAMP    = 1700000000;
	private const SIGNATURE_V1 = '8e539fc858dc0c9c69c22a9f8504e69978e68fc3f3ae98164915456bd91b5467';
	private const SIGNATURE_V2 = '30b6dcd1b271a6d3beced219c3730370d90588c708d60af610611756f3cd26c5';
	private const BODY         = '{"created_at":1700000000,"data":{"attributes":{"attempt_id":"00000000-0000-0000-0000-000000000005",'
		. '"payment_intent_id":"00000000-0000-0000-0000-000000000004","transfer_id":"00000000-0000-0000-0000-000000000006"},'
		. '"id":"00000000-0000-0000-0000-000000000004","object":"payment_intent"},'
		. '"id":"00000000-0000-0000-0000-000000000003","type":"payment_intent.paid"}';

	private static function header( string ...$signatures ): string {
		$header = 't=' . self::TIMESTAMP;
		foreach ( $signatures as $signature ) {
			$header .= ',v1=' . $signature;
		}
		return $header;
	}

	public function test_the_signature_matches_the_gateway_vector(): void {
		self::assertSame( self::SIGNATURE_V1, bin2hex( Signature::compute( self::SECRET_V1, self::TIMESTAMP, self::BODY ) ) );
		self::assertSame( self::SIGNATURE_V2, bin2hex( Signature::compute( self::SECRET_V2, self::TIMESTAMP, self::BODY ) ) );
	}

	public function test_the_key_is_the_decoded_secret_not_its_text(): void {
		$text_keyed = hash_hmac( 'sha256', self::TIMESTAMP . '.' . self::BODY, self::SECRET_V1 );
		self::assertNotSame( $text_keyed, self::SIGNATURE_V1 );
	}

	public function test_a_valid_delivery_inside_the_tolerance_is_accepted(): void {
		self::assertSame( Signature::OK, Signature::verify( self::header( self::SIGNATURE_V1 ), self::BODY, array( self::SECRET_V1 ), self::TIMESTAMP + 10 ) );
		self::assertSame( Signature::OK, Signature::verify( self::header( self::SIGNATURE_V1 ), self::BODY, array( strtoupper( self::SECRET_V1 ) ), self::TIMESTAMP - 300 ) );
	}

	public function test_a_delivery_outside_the_tolerance_is_refused(): void {
		self::assertSame( Signature::OUTSIDE_TOLERANCE, Signature::verify( self::header( self::SIGNATURE_V1 ), self::BODY, array( self::SECRET_V1 ), self::TIMESTAMP + 301 ) );
		self::assertSame( Signature::OUTSIDE_TOLERANCE, Signature::verify( self::header( self::SIGNATURE_V1 ), self::BODY, array( self::SECRET_V1 ), self::TIMESTAMP - 301 ) );
	}

	public function test_a_tampered_body_secret_or_timestamp_is_refused(): void {
		$tampered = str_replace( 'payment_intent.paid', 'payment_intent.pai', self::BODY );
		self::assertSame( Signature::SIGNATURE_MISMATCH, Signature::verify( self::header( self::SIGNATURE_V1 ), $tampered, array( self::SECRET_V1 ), self::TIMESTAMP ) );
		self::assertSame( Signature::SIGNATURE_MISMATCH, Signature::verify( self::header( self::SIGNATURE_V1 ), self::BODY . ' ', array( self::SECRET_V1 ), self::TIMESTAMP ) );
		self::assertSame( Signature::SIGNATURE_MISMATCH, Signature::verify( self::header( self::SIGNATURE_V1 ), self::BODY, array( self::SECRET_V2 ), self::TIMESTAMP ) );
		$shifted = 't=' . ( self::TIMESTAMP + 1 ) . ',v1=' . self::SIGNATURE_V1;
		self::assertSame( Signature::SIGNATURE_MISMATCH, Signature::verify( $shifted, self::BODY, array( self::SECRET_V1 ), self::TIMESTAMP ) );
	}

	public function test_rotation_accepts_any_matching_pair(): void {
		$both = self::header( self::SIGNATURE_V2, self::SIGNATURE_V1 );
		self::assertSame( Signature::OK, Signature::verify( $both, self::BODY, array( self::SECRET_V1 ), self::TIMESTAMP ), 'store still on the old secret' );
		self::assertSame( Signature::OK, Signature::verify( $both, self::BODY, array( self::SECRET_V2 ), self::TIMESTAMP ), 'store already on the new secret' );
		self::assertSame( Signature::OK, Signature::verify( self::header( self::SIGNATURE_V1 ), self::BODY, array( self::SECRET_V2, self::SECRET_V1 ), self::TIMESTAMP ), 'new secret configured, old delivery' );
		self::assertSame( Signature::SIGNATURE_MISMATCH, Signature::verify( self::header( self::SIGNATURE_V2 ), self::BODY, array( self::SECRET_V1 ), self::TIMESTAMP ), 'new-only delivery after the old secret was the only one kept' );
	}

	public function test_malformed_headers_are_refused(): void {
		$cases = array(
			'',
			'v1=' . self::SIGNATURE_V1,
			't=' . self::TIMESTAMP,
			't=' . self::TIMESTAMP . ',t=' . self::TIMESTAMP . ',v1=' . self::SIGNATURE_V1,
			't=abc,v1=' . self::SIGNATURE_V1,
			't=' . self::TIMESTAMP . ',v1=' . strtoupper( self::SIGNATURE_V1 ),
			't=' . self::TIMESTAMP . ',v1=' . substr( self::SIGNATURE_V1, 2 ),
			't=' . self::TIMESTAMP . ',garbage,v1=' . self::SIGNATURE_V1,
		);
		foreach ( $cases as $header ) {
			$result = Signature::verify( $header, self::BODY, array( self::SECRET_V1 ), self::TIMESTAMP );
			self::assertContains( $result, array( Signature::MALFORMED_HEADER, Signature::MISSING_HEADER ), $header );
		}
		self::assertSame( Signature::MISSING_HEADER, Signature::verify( null, self::BODY, array( self::SECRET_V1 ), self::TIMESTAMP ) );
	}

	public function test_unknown_schemes_beside_v1_are_ignored(): void {
		$header = 't=' . self::TIMESTAMP . ',v2=zzz,v1=' . self::SIGNATURE_V1;
		self::assertSame( Signature::OK, Signature::verify( $header, self::BODY, array( self::SECRET_V1 ), self::TIMESTAMP ) );
	}

	public function test_no_usable_secret_refuses_everything(): void {
		self::assertSame( Signature::NO_SECRET, Signature::verify( self::header( self::SIGNATURE_V1 ), self::BODY, array(), self::TIMESTAMP ) );
		self::assertSame( Signature::NO_SECRET, Signature::verify( self::header( self::SIGNATURE_V1 ), self::BODY, array( 'short', '' ), self::TIMESTAMP ) );
	}

	public function test_compute_refuses_a_secret_that_is_not_64_hex(): void {
		$this->expectException( \InvalidArgumentException::class );
		Signature::compute( 'not-hex', self::TIMESTAMP, self::BODY );
	}
}
