<?php
/**
 * Another request is processing the same order.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * Thrown by Plugin::with_order_lock; the caller retries later.
 */
final class LockBusy extends \RuntimeException {
}
