<?php
/**
 * Polling for missed webhooks.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * Every five minutes, reads the intent of unpaid orders that have waited
 * longer than the configured delay and applies its status through the same
 * OrderSync path as the webhook.
 */
final class Reconciler {

	public const HOOK     = 'cgusdt_reconcile';
	public const SCHEDULE = 'cgusdt_every_five_minutes';

	/** Orders read per run; the rest wait for the next run. */
	private const BATCH = 20;

	/** Orders older than this are no longer polled; an operator case is handled by hand. */
	private const MAX_AGE_DAYS = 30;

	/**
	 * Hooks the schedule and the job.
	 */
	public static function register(): void {
		add_filter( 'cron_schedules', array( self::class, 'add_schedule' ) ); // phpcs:ignore WordPress.WP.CronInterval.ChangeDetected
		add_action( self::HOOK, array( self::class, 'run' ) );
		add_action( 'init', array( self::class, 'ensure_scheduled' ) );
	}

	/**
	 * Adds a five-minute interval.
	 *
	 * @param array<string,array<string,mixed>> $schedules Schedules.
	 * @return array<string,array<string,mixed>>
	 */
	public static function add_schedule( $schedules ): array {
		$schedules                   = is_array( $schedules ) ? $schedules : array();
		$schedules[ self::SCHEDULE ] = array(
			'interval' => 300,
			'display'  => __( 'Every five minutes', 'crypto-gateway-usdt' ),
		);
		return $schedules;
	}

	/**
	 * Schedules the job if it is missing, e.g. after a failed activation hook.
	 */
	public static function ensure_scheduled(): void {
		if ( false === wp_next_scheduled( self::HOOK ) ) {
			wp_schedule_event( time() + 60, self::SCHEDULE, self::HOOK );
		}
	}

	/**
	 * Removes the job.
	 */
	public static function unschedule(): void {
		wp_clear_scheduled_hook( self::HOOK );
	}

	/**
	 * One reconciliation pass.
	 */
	public static function run(): void {
		$client = Plugin::client();
		if ( null === $client ) {
			return;
		}
		$delay = Plugin::reconcile_after_minutes() * MINUTE_IN_SECONDS;
		$now   = time();
		$ids   = wc_get_orders(
			array(
				'limit'          => 100,
				'return'         => 'ids',
				'payment_method' => Plugin::GATEWAY_ID,
				'status'         => array( 'pending', 'on-hold' ),
				'date_created'   => ( $now - self::MAX_AGE_DAYS * DAY_IN_SECONDS ) . '...' . ( $now - $delay ),
				// Newest first: a recent order is the one a payment is most likely to settle.
				'orderby'        => 'date',
				'order'          => 'DESC',
			)
		);
		$done = 0;
		foreach ( $ids as $order_id ) {
			if ( $done >= self::BATCH ) {
				break;
			}
			$order = wc_get_order( $order_id );
			if ( ! $order instanceof \WC_Order || '' === (string) $order->get_meta( Meta::INTENT_ID, true ) ) {
				continue;
			}
			if ( (int) $order->get_meta( Meta::LAST_CHECKED, true ) > $now - $delay ) {
				continue;
			}
			// After the late-payment window an expired intent can no longer be paid or honoured.
			$late_until = (int) $order->get_meta( Meta::LATE_UNTIL, true );
			if ( 'expired' === $order->get_meta( Meta::INTENT_STATUS, true ) && $late_until > 0 && $late_until < $now ) {
				continue;
			}
			++$done;
			self::check( $order, $client );
		}
	}

	/**
	 * Checks one order now. Used by the job and the admin action.
	 *
	 * @param \WC_Order $order  Order with an intent.
	 * @param ApiClient $client Client.
	 * @return string|null The gateway status, or null when the check failed.
	 */
	public static function check( \WC_Order $order, ApiClient $client ): ?string {
		try {
			return Plugin::with_order_lock(
				$order->get_id(),
				static function () use ( $order, $client ): string {
					$fresh = wc_get_order( $order->get_id() );
					if ( ! $fresh instanceof \WC_Order ) {
						throw new \RuntimeException( 'order disappeared' );
					}
					return Plugin::sync_from_gateway( $fresh, $client );
				}
			);
		} catch ( LockBusy $busy ) {
			return null;
		} catch ( \Throwable $error ) {
			Plugin::log( 'error', 'CGUSDT_RECONCILE_FAILED order=' . $order->get_id() . ' ' . $error->getMessage() );
			return null;
		}
	}
}
