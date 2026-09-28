<?php
/**
 * A checkout failure whose message is safe to show the buyer.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * The message is translated, carries no gateway internals, and is shown as a notice.
 */
final class UserFacingError extends \RuntimeException {
}
