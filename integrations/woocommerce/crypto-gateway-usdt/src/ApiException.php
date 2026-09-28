<?php
/**
 * A gateway call that did not produce the expected result.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * Carries the HTTP status and the gateway's error code; status 0 means the
 * request never got an answer (network, TLS, timeout).
 */
final class ApiException extends \RuntimeException {

	/**
	 * Constructor.
	 *
	 * @param string $message    Safe to log: never contains the key or a body with secrets.
	 * @param int    $http_status HTTP status, 0 when there was no response.
	 * @param string $error_code  error.code from the gateway envelope, or a local code.
	 */
	public function __construct( string $message, public readonly int $http_status, public readonly string $error_code ) {
		parent::__construct( $message );
	}

	/**
	 * Whether retrying the same request later may succeed.
	 */
	public function is_transient(): bool {
		return 0 === $this->http_status || $this->http_status >= 500 || 429 === $this->http_status;
	}
}
