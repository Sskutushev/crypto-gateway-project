<?php
/**
 * A WC_Order stand-in with the methods OrderSync calls.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt\Tests;

/**
 * Records every status change, note and payment_complete call.
 */
final class FakeOrder {

	/** @var array<string,mixed> */
	public array $meta = array();

	/** @var list<string> */
	public array $notes = array();

	/** @var list<string> */
	public array $completions = array();

	public int $saves = 0;

	/**
	 * Constructor.
	 *
	 * @param string $status Initial WooCommerce status without the wc- prefix.
	 */
	public function __construct( public string $status = 'pending' ) {
	}

	public function get_id(): int {
		return 42;
	}

	public function get_status(): string {
		return $this->status;
	}

	/**
	 * Same semantics as WC_Order::has_status.
	 *
	 * @param string|list<string> $status Status or statuses.
	 */
	public function has_status( $status ): bool {
		return in_array( $this->status, (array) $status, true );
	}

	public function is_paid(): bool {
		return in_array( $this->status, array( 'processing', 'completed' ), true );
	}

	/**
	 * Mirrors WC_Order::payment_complete: only from a payable status.
	 *
	 * @param string $transaction_id Transaction id.
	 */
	public function payment_complete( string $transaction_id = '' ): bool {
		if ( ! $this->has_status( array( 'on-hold', 'pending', 'failed', 'cancelled' ) ) ) {
			return false;
		}
		$this->completions[] = $transaction_id;
		$this->status        = 'processing';
		++$this->saves;
		return true;
	}

	public function update_status( string $status, string $note = '' ): bool {
		$this->status = $status;
		if ( '' !== $note ) {
			$this->notes[] = $note;
		}
		++$this->saves;
		return true;
	}

	public function add_order_note( string $note ): int {
		$this->notes[] = $note;
		return count( $this->notes );
	}

	/**
	 * Same signature as WC_Data::get_meta with $single = true.
	 *
	 * @param string $key    Key.
	 * @param bool   $single Ignored.
	 * @return mixed
	 */
	public function get_meta( string $key, bool $single = true ) {
		return $this->meta[ $key ] ?? '';
	}

	/**
	 * Same as WC_Data::update_meta_data.
	 *
	 * @param string $key   Key.
	 * @param mixed  $value Value.
	 */
	public function update_meta_data( string $key, $value ): void {
		$this->meta[ $key ] = $value;
	}

	public function save(): int {
		++$this->saves;
		return $this->get_id();
	}
}
