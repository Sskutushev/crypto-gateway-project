<?php
/**
 * Event and status mapping onto an order.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt\Tests\Unit;

use CryptoGatewayUsdt\Action;
use CryptoGatewayUsdt\Meta;
use CryptoGatewayUsdt\OrderSync;
use CryptoGatewayUsdt\Tests\FakeOrder;
use PHPUnit\Framework\TestCase;

final class OrderSyncTest extends TestCase {

	private const INTENT   = '00000000-0000-0000-0000-000000000004';
	private const TRANSFER = '00000000-0000-0000-0000-000000000006';

	private static function event( string $type, array $attributes = array() ): ?Action {
		return OrderSync::action_for_event( $type, $attributes + array( 'payment_intent_id' => self::INTENT ), self::INTENT );
	}

	public function test_paid_completes_the_order_exactly_once(): void {
		$order  = new FakeOrder( 'pending' );
		$action = self::event( OrderSync::EVENT_PAID, array( 'transfer_id' => self::TRANSFER ) );
		self::assertNotNull( $action );

		self::assertTrue( OrderSync::apply( $order, $action ) );
		self::assertFalse( OrderSync::apply( $order, $action ) );
		self::assertFalse( OrderSync::apply( $order, OrderSync::action_for_status( 'paid', self::INTENT ) ), 'the reconciler seeing the same fact' );

		self::assertSame( 'processing', $order->status );
		self::assertSame( array( self::INTENT ), $order->completions );
		self::assertSame( 'paid', $order->meta[ Meta::INTENT_STATUS ] );
		self::assertStringContainsString( self::TRANSFER, implode( "\n", $order->notes ) );
	}

	public function test_paid_completes_an_order_held_for_a_partial_payment(): void {
		$order = new FakeOrder( 'pending' );
		OrderSync::apply( $order, self::event( OrderSync::EVENT_PARTIALLY_PAID ) );
		self::assertSame( 'on-hold', $order->status );
		OrderSync::apply( $order, self::event( OrderSync::EVENT_PAID ) );
		self::assertSame( 'processing', $order->status );
		self::assertCount( 1, $order->completions );
	}

	public function test_partially_paid_holds_and_never_completes(): void {
		$order  = new FakeOrder( 'pending' );
		$action = self::event( OrderSync::EVENT_PARTIALLY_PAID, array( 'transfer_id' => self::TRANSFER ) );

		self::assertTrue( OrderSync::apply( $order, $action ) );
		self::assertFalse( OrderSync::apply( $order, $action ) );
		self::assertFalse( OrderSync::apply( $order, OrderSync::action_for_status( 'partially_paid', self::INTENT ) ) );

		self::assertSame( 'on-hold', $order->status );
		self::assertSame( array(), $order->completions );
		self::assertCount( 1, $order->notes );
	}

	public function test_a_partial_payment_resolved_by_hand_is_not_put_back_on_hold(): void {
		$order = new FakeOrder( 'pending' );
		OrderSync::apply( $order, self::event( OrderSync::EVENT_PARTIALLY_PAID ) );
		$order->status = 'pending';
		OrderSync::apply( $order, OrderSync::action_for_status( 'partially_paid', self::INTENT ) );
		self::assertSame( 'pending', $order->status );
	}

	public function test_cancelled_cancels_an_unpaid_order(): void {
		$order = new FakeOrder( 'pending' );
		self::assertTrue( OrderSync::apply( $order, self::event( OrderSync::EVENT_CANCELLED ) ) );
		self::assertSame( 'cancelled', $order->status );
		self::assertFalse( OrderSync::apply( $order, self::event( OrderSync::EVENT_CANCELLED ) ) );
	}

	public function test_cancelled_never_touches_a_paid_order_and_says_so_once(): void {
		$order = new FakeOrder( 'processing' );
		self::assertFalse( OrderSync::apply( $order, self::event( OrderSync::EVENT_CANCELLED ) ) );
		self::assertSame( 'processing', $order->status );
		self::assertCount( 1, $order->notes );
		OrderSync::apply( $order, OrderSync::action_for_status( 'cancelled', self::INTENT ) );
		self::assertCount( 1, $order->notes );
	}

	public function test_a_reopened_order_is_not_cancelled_again_by_the_same_fact(): void {
		$order = new FakeOrder( 'pending' );
		OrderSync::apply( $order, self::event( OrderSync::EVENT_CANCELLED ) );
		$order->status = 'pending';
		self::assertFalse( OrderSync::apply( $order, OrderSync::action_for_status( 'cancelled', self::INTENT ) ) );
		self::assertSame( 'pending', $order->status );
	}

	public function test_paid_on_an_order_in_a_non_payable_status_only_adds_a_note(): void {
		$order = new FakeOrder( 'refunded' );
		self::assertFalse( OrderSync::apply( $order, self::event( OrderSync::EVENT_PAID ) ) );
		self::assertSame( 'refunded', $order->status );
		self::assertSame( array(), $order->completions );
		self::assertCount( 1, $order->notes );
	}

	public function test_overpaid_adds_a_note_with_the_exact_remainder(): void {
		$order = new FakeOrder( 'processing' );
		$order->meta[ Meta::ASSET_DECIMALS ] = 6;
		$order->meta[ Meta::ASSET_SYMBOL ]   = 'USDT';
		$action                              = self::event( OrderSync::EVENT_OVERPAID, array( 'remainder_raw' => '1500001' ) );
		self::assertNotNull( $action );
		OrderSync::apply( $order, $action );
		self::assertSame( 'processing', $order->status );
		self::assertStringContainsString( '1.500001 USDT', $order->notes[0] );
		self::assertStringContainsString( '1500001', $order->notes[0] );
		self::assertArrayNotHasKey( Meta::INTENT_STATUS, $order->meta );
	}

	public function test_overpaid_without_decimals_still_names_the_raw_remainder(): void {
		$order = new FakeOrder( 'processing' );
		OrderSync::apply( $order, self::event( OrderSync::EVENT_OVERPAID, array( 'remainder_raw' => '7' ) ) );
		self::assertStringContainsString( '7 raw units', $order->notes[0] );
	}

	public function test_an_overpaid_event_without_a_valid_remainder_is_not_acted_on(): void {
		self::assertNull( self::event( OrderSync::EVENT_OVERPAID, array( 'remainder_raw' => '-5' ) ) );
		self::assertNull( self::event( OrderSync::EVENT_OVERPAID ) );
	}

	public function test_test_and_unknown_events_map_to_nothing(): void {
		self::assertNull( self::event( OrderSync::EVENT_TEST ) );
		self::assertNull( self::event( 'payment_intent.refunded' ) );
		self::assertNull( self::event( '' ) );
	}

	public function test_an_unknown_status_changes_nothing_and_records_nothing(): void {
		$order  = new FakeOrder( 'pending' );
		$action = OrderSync::action_for_status( 'something_new', self::INTENT );
		self::assertSame( Action::NONE, $action->kind );
		self::assertFalse( OrderSync::apply( $order, $action ) );
		self::assertSame( 'pending', $order->status );
		self::assertArrayNotHasKey( Meta::INTENT_STATUS, $order->meta );
		self::assertSame( array(), $order->notes );
	}

	public function test_waiting_statuses_only_record_the_status(): void {
		foreach ( array( 'requires_quote', 'awaiting_payment', 'risk_hold' ) as $status ) {
			$order = new FakeOrder( 'pending' );
			self::assertFalse( OrderSync::apply( $order, OrderSync::action_for_status( $status, self::INTENT ) ) );
			self::assertSame( 'pending', $order->status );
			self::assertSame( $status, $order->meta[ Meta::INTENT_STATUS ] );
		}
	}

	public function test_expiry_is_noted_once_per_quote_attempt(): void {
		$order                              = new FakeOrder( 'pending' );
		$order->meta[ Meta::QUOTE_ATTEMPT ] = 1;
		$expired                            = OrderSync::action_for_status( 'expired', self::INTENT );
		self::assertTrue( OrderSync::apply( $order, $expired ) );
		self::assertFalse( OrderSync::apply( $order, $expired ) );
		$order->meta[ Meta::QUOTE_ATTEMPT ] = 2;
		self::assertTrue( OrderSync::apply( $order, $expired ) );
		self::assertCount( 2, $order->notes );
		self::assertSame( 'pending', $order->status );
	}

	public function test_event_ids_are_remembered_and_bounded(): void {
		$order = new FakeOrder( 'pending' );
		self::assertFalse( OrderSync::seen_event( $order, 'e-0' ) );
		for ( $i = 0; $i < OrderSync::MAX_REMEMBERED_EVENTS + 5; $i++ ) {
			OrderSync::remember_event( $order, 'e-' . $i );
		}
		OrderSync::remember_event( $order, 'e-' . ( OrderSync::MAX_REMEMBERED_EVENTS + 4 ) );
		self::assertCount( OrderSync::MAX_REMEMBERED_EVENTS, $order->meta[ Meta::EVENTS ] );
		self::assertFalse( OrderSync::seen_event( $order, 'e-0' ) );
		self::assertTrue( OrderSync::seen_event( $order, 'e-5' ) );
		self::assertTrue( OrderSync::seen_event( $order, 'e-' . ( OrderSync::MAX_REMEMBERED_EVENTS + 4 ) ) );
	}
}
