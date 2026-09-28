<?php
/**
 * Merchant API client.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * The three merchant routes the plugin uses, over wp_remote_request.
 */
final class ApiClient {

	private const TIMEOUT_SECONDS = 15;

	/**
	 * Constructor.
	 *
	 * @param string $base_url Validated base URL without a trailing slash.
	 * @param string $api_key  Merchant bearer key.
	 */
	public function __construct( private readonly string $base_url, private readonly string $api_key ) {
	}

	/**
	 * POST /v1/payment-intents.
	 *
	 * @param string $amount_minor    Minor units as a decimal string.
	 * @param string $currency        ISO code.
	 * @param string $reference       Merchant reference, unique per merchant.
	 * @param string $idempotency_key Idempotency-Key.
	 * @param array<string,mixed> $metadata Stored with the intent.
	 * @return array<string,mixed>
	 */
	public function create_intent( string $amount_minor, string $currency, string $reference, string $idempotency_key, array $metadata ): array {
		return $this->request(
			'POST',
			'/v1/payment-intents',
			array(
				'amount_minor' => $amount_minor,
				'currency'     => $currency,
				'reference'    => $reference,
				'metadata'     => (object) $metadata,
			),
			$idempotency_key
		);
	}

	/**
	 * POST /v1/payment-intents/{id}/quotes.
	 *
	 * @param string $intent_id       Intent id.
	 * @param string $asset_id        Allowlisted asset id.
	 * @param string $idempotency_key Idempotency-Key.
	 * @return array<string,mixed>
	 */
	public function create_quote( string $intent_id, string $asset_id, string $idempotency_key ): array {
		return $this->request( 'POST', '/v1/payment-intents/' . rawurlencode( $intent_id ) . '/quotes', array( 'asset_id' => $asset_id ), $idempotency_key );
	}

	/**
	 * GET /v1/payment-intents/{id}.
	 *
	 * @param string $intent_id Intent id.
	 * @return array<string,mixed>
	 */
	public function get_intent( string $intent_id ): array {
		return $this->request( 'GET', '/v1/payment-intents/' . rawurlencode( $intent_id ) );
	}

	/**
	 * POST /v1/payment-intents/{id}/cancel.
	 *
	 * @param string $intent_id       Intent id.
	 * @param string $reason          Kept in the gateway audit trail.
	 * @param string $idempotency_key Idempotency-Key.
	 * @return array<string,mixed>
	 */
	public function cancel_intent( string $intent_id, string $reason, string $idempotency_key ): array {
		return $this->request( 'POST', '/v1/payment-intents/' . rawurlencode( $intent_id ) . '/cancel', array( 'reason' => $reason ), $idempotency_key );
	}

	/**
	 * GET /v1/checkout/{token}: the public buyer view, used for the transaction hash.
	 *
	 * @param string $token Checkout token.
	 * @return array<string,mixed>
	 */
	public function checkout_view( string $token ): array {
		return $this->request( 'GET', '/v1/checkout/' . rawurlencode( $token ), null, null, false );
	}

	/**
	 * The hosted payment page for a checkout token.
	 *
	 * @param string $token Checkout token.
	 */
	public function checkout_url( string $token ): string {
		return $this->base_url . '/checkout/' . rawurlencode( $token );
	}

	/**
	 * One request; any non-2xx answer or unreadable body is an exception.
	 *
	 * @param string                   $method          HTTP method.
	 * @param string                   $path            Path under the base URL.
	 * @param array<string,mixed>|null $body            JSON body.
	 * @param string|null              $idempotency_key Idempotency-Key.
	 * @param bool                     $authenticated   Whether to send the merchant key.
	 * @return array<string,mixed>
	 * @throws ApiException On transport failure, a non-2xx status or a body that is not a JSON object.
	 */
	private function request( string $method, string $path, ?array $body = null, ?string $idempotency_key = null, bool $authenticated = true ): array {
		$headers = array( 'Accept' => 'application/json' );
		if ( $authenticated ) {
			$headers['Authorization'] = 'Bearer ' . $this->api_key;
		}
		if ( null !== $idempotency_key ) {
			$headers['Idempotency-Key'] = $idempotency_key;
		}
		$args = array(
			'method'      => $method,
			'headers'     => $headers,
			'timeout'     => self::TIMEOUT_SECONDS,
			'sslverify'   => true,
			// A redirect would carry the bearer key to an address nobody configured.
			'redirection' => 0,
		);
		if ( null !== $body ) {
			$headers['Content-Type'] = 'application/json';
			$args['headers']         = $headers;
			$args['body']            = (string) wp_json_encode( $body );
		}

		$response = wp_remote_request( $this->base_url . $path, $args );
		if ( is_wp_error( $response ) ) {
			throw new ApiException( $method . ' ' . $path . ' failed: ' . $response->get_error_message(), 0, 'transport_error' );
		}
		$status  = (int) wp_remote_retrieve_response_code( $response );
		$decoded = json_decode( (string) wp_remote_retrieve_body( $response ), true, 64, JSON_BIGINT_AS_STRING );
		if ( $status < 200 || $status >= 300 ) {
			$code    = is_array( $decoded ) && is_string( $decoded['error']['code'] ?? null ) ? $decoded['error']['code'] : 'http_' . $status;
			$message = is_array( $decoded ) && is_string( $decoded['error']['message'] ?? null ) ? $decoded['error']['message'] : 'unexpected response';
			throw new ApiException( $method . ' ' . $path . ' answered ' . $status . ' ' . $code . ': ' . $message, $status, $code );
		}
		if ( ! is_array( $decoded ) ) {
			throw new ApiException( $method . ' ' . $path . ' answered ' . $status . ' with a body that is not JSON', $status, 'invalid_body' );
		}
		return $decoded;
	}
}
