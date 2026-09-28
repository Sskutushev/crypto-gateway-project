<?php
/**
 * The WooCommerce payment method.
 *
 * @package CryptoGatewayUsdt
 */

declare(strict_types=1);

namespace CryptoGatewayUsdt;

/**
 * USDT (TRC20) through a self-hosted, non-custodial payment gateway.
 */
class Gateway extends \WC_Payment_Gateway {

	/**
	 * Constructor.
	 */
	public function __construct() {
		$this->id                 = Plugin::GATEWAY_ID;
		$this->has_fields         = false;
		$this->method_title       = __( 'USDT (TRC20)', 'crypto-gateway-usdt' );
		$this->method_description = __( 'Accept USDT on TRON through your own self-hosted, non-custodial payment gateway. The buyer pays on the gateway\'s hosted page; the order is completed only by a signed webhook or a status check against the gateway.', 'crypto-gateway-usdt' );
		$this->supports           = array( 'products' );

		$this->init_form_fields();
		$this->init_settings();

		$this->title       = (string) $this->get_option( 'title' );
		$this->description = (string) $this->get_option( 'description' );
		if ( 'yes' === $this->get_option( 'test_mode' ) ) {
			$this->title .= ' ' . __( '(test network)', 'crypto-gateway-usdt' );
		}

		add_action( 'woocommerce_update_options_payment_gateways_' . $this->id, array( $this, 'process_admin_options' ) );
	}

	/**
	 * Settings fields.
	 */
	public function init_form_fields(): void {
		$this->form_fields = array(
			'enabled'                 => array(
				'title'   => __( 'Enable', 'crypto-gateway-usdt' ),
				'type'    => 'checkbox',
				'label'   => __( 'Accept USDT (TRC20) payments', 'crypto-gateway-usdt' ),
				'default' => 'no',
			),
			'title'                   => array(
				'title'   => __( 'Title', 'crypto-gateway-usdt' ),
				'type'    => 'text',
				'default' => __( 'USDT (TRC20)', 'crypto-gateway-usdt' ),
			),
			'description'             => array(
				'title'   => __( 'Description', 'crypto-gateway-usdt' ),
				'type'    => 'textarea',
				'default' => __( 'Pay with USDT on the TRON network. You will be sent to a payment page with the exact amount and address.', 'crypto-gateway-usdt' ),
			),
			'test_mode'               => array(
				'title'       => __( 'Test network label', 'crypto-gateway-usdt' ),
				'type'        => 'checkbox',
				'label'       => __( 'Show "(test network)" next to the method title', 'crypto-gateway-usdt' ),
				'description' => __( 'A label only. Which network is used is decided by the gateway operator (Nile testnet or mainnet); tick this while the gateway runs on Nile.', 'crypto-gateway-usdt' ),
				'default'     => 'no',
			),
			'base_url'                => array(
				'title'       => __( 'Gateway base URL', 'crypto-gateway-usdt' ),
				'type'        => 'text',
				'description' => __( 'For example https://pay.example.com. HTTPS is required, except for a gateway on localhost.', 'crypto-gateway-usdt' ),
				'default'     => '',
			),
			'api_key'                 => array(
				'title'       => __( 'Merchant API key', 'crypto-gateway-usdt' ),
				'type'        => 'password',
				'description' => __( 'Issued once by the operator with "gateway-worker admin api-key-issue".', 'crypto-gateway-usdt' ),
				'default'     => '',
			),
			'asset_id'                => array(
				'title'       => __( 'USDT asset id', 'crypto-gateway-usdt' ),
				'type'        => 'text',
				'description' => __( 'The asset UUID the operator gave you for USDT TRC20.', 'crypto-gateway-usdt' ),
				'default'     => '',
			),
			'supported_currencies'    => array(
				'title'       => __( 'Priced store currencies', 'crypto-gateway-usdt' ),
				'type'        => 'text',
				'description' => __( 'Comma-separated ISO codes the operator publishes prices for, e.g. "USD, EUR". The method is offered only when the store currency is listed; otherwise every quote would be refused.', 'crypto-gateway-usdt' ),
				'default'     => 'USD',
			),
			'reference_prefix'        => array(
				'title'       => __( 'Order reference prefix', 'crypto-gateway-usdt' ),
				'type'        => 'text',
				'description' => __( 'Prepended to the order number in the gateway reference. Use a different prefix per store if several stores share one merchant.', 'crypto-gateway-usdt' ),
				'default'     => 'wc-',
			),
			'webhook_secret'          => array(
				'title'       => __( 'Webhook signing secret', 'crypto-gateway-usdt' ),
				'type'        => 'password',
				'description' => sprintf(
					/* translators: %s: webhook URL */
					__( '64 hex characters, shown once by "gateway-worker admin webhook-add". Register this URL: %s', 'crypto-gateway-usdt' ),
					// rest_url() needs the rewrite object, which exists only from init on.
					'<code>' . esc_html( did_action( 'init' ) ? rest_url( WebhookController::ROUTE_NAMESPACE . WebhookController::ROUTE ) : '/wp-json/' . WebhookController::ROUTE_NAMESPACE . WebhookController::ROUTE ) . '</code>'
				),
				'default'     => '',
			),
			'webhook_secret_previous' => array(
				'title'       => __( 'Previous webhook secret', 'crypto-gateway-usdt' ),
				'type'        => 'password',
				'description' => __( 'During a secret rotation: keep the old secret here until the transition ends, then clear it.', 'crypto-gateway-usdt' ),
				'default'     => '',
			),
			'reconcile_after_minutes' => array(
				'title'       => __( 'Status check after (minutes)', 'crypto-gateway-usdt' ),
				'type'        => 'number',
				'description' => __( 'Unpaid orders older than this are checked against the gateway every five minutes, in case a webhook was missed.', 'crypto-gateway-usdt' ),
				'default'     => '10',
				'custom_attributes' => array(
					'min' => '2',
					'max' => '1440',
				),
			),
		);
	}

	/**
	 * Base URL: HTTPS unless localhost.
	 *
	 * @param string $key   Field key.
	 * @param mixed  $value Posted value.
	 */
	public function validate_base_url_field( $key, $value ): string {
		$value = (string) $value;
		if ( '' === trim( $value ) ) {
			return '';
		}
		$normalized = Config::normalize_base_url( $value );
		if ( null === $normalized ) {
			\WC_Admin_Settings::add_error( __( 'The gateway base URL must be an https:// address (http:// only for localhost), without credentials, query or fragment.', 'crypto-gateway-usdt' ) );
			return (string) $this->get_option( $key );
		}
		return $normalized;
	}

	/**
	 * API key: 32 to 256 printable characters.
	 *
	 * @param string $key   Field key.
	 * @param mixed  $value Posted value.
	 */
	public function validate_api_key_field( $key, $value ): string {
		$value = trim( (string) $value );
		if ( '' !== $value && ! Config::is_valid_api_key( $value ) ) {
			\WC_Admin_Settings::add_error( __( 'The merchant API key must be 32 to 256 characters.', 'crypto-gateway-usdt' ) );
			return (string) $this->get_option( $key );
		}
		return $value;
	}

	/**
	 * Asset id: a UUID.
	 *
	 * @param string $key   Field key.
	 * @param mixed  $value Posted value.
	 */
	public function validate_asset_id_field( $key, $value ): string {
		$value = strtolower( trim( (string) $value ) );
		if ( '' !== $value && ! Config::is_uuid( $value ) ) {
			\WC_Admin_Settings::add_error( __( 'The USDT asset id must be a UUID.', 'crypto-gateway-usdt' ) );
			return (string) $this->get_option( $key );
		}
		return $value;
	}

	/**
	 * Currencies: normalised list of codes.
	 *
	 * @param string $key   Field key.
	 * @param mixed  $value Posted value.
	 */
	public function validate_supported_currencies_field( $key, $value ): string {
		return implode( ', ', Config::parse_currencies( (string) $value ) );
	}

	/**
	 * Reference prefix: characters that are safe in a reference.
	 *
	 * @param string $key   Field key.
	 * @param mixed  $value Posted value.
	 */
	public function validate_reference_prefix_field( $key, $value ): string {
		$value = trim( (string) $value );
		if ( 1 !== preg_match( '/^[A-Za-z0-9_.:-]{0,32}$/', $value ) ) {
			\WC_Admin_Settings::add_error( __( 'The reference prefix may use letters, digits and _ . : - only, up to 32 characters.', 'crypto-gateway-usdt' ) );
			return (string) $this->get_option( $key );
		}
		return $value;
	}

	/**
	 * Current secret: 64 hex characters.
	 *
	 * @param string $key   Field key.
	 * @param mixed  $value Posted value.
	 */
	public function validate_webhook_secret_field( $key, $value ): string {
		return $this->validate_secret( $key, $value );
	}

	/**
	 * Previous secret: 64 hex characters or empty.
	 *
	 * @param string $key   Field key.
	 * @param mixed  $value Posted value.
	 */
	public function validate_webhook_secret_previous_field( $key, $value ): string {
		return $this->validate_secret( $key, $value );
	}

	/**
	 * Shared secret validation.
	 *
	 * @param string $key   Field key.
	 * @param mixed  $value Posted value.
	 */
	private function validate_secret( string $key, $value ): string {
		$value = strtolower( trim( (string) $value ) );
		if ( '' !== $value && ! Signature::is_valid_secret( $value ) ) {
			\WC_Admin_Settings::add_error( __( 'A webhook signing secret is exactly 64 hex characters.', 'crypto-gateway-usdt' ) );
			return (string) $this->get_option( $key );
		}
		return $value;
	}

	/**
	 * Offered only when configured and the store currency is priced by the operator.
	 */
	public function is_available(): bool {
		if ( ! parent::is_available() ) {
			return false;
		}
		return null === $this->unavailable_reason( get_woocommerce_currency() );
	}

	/**
	 * Why the method cannot take a payment in this currency, or null when it can.
	 *
	 * @param string $currency Store or order currency.
	 */
	public function unavailable_reason( string $currency ): ?string {
		if ( null === Plugin::client() ) {
			return __( 'The USDT payment gateway is not configured.', 'crypto-gateway-usdt' );
		}
		if ( ! Config::is_uuid( (string) $this->get_option( 'asset_id' ) ) ) {
			return __( 'The USDT asset id is not configured.', 'crypto-gateway-usdt' );
		}
		if ( ! in_array( strtoupper( $currency ), Config::parse_currencies( (string) $this->get_option( 'supported_currencies' ) ), true ) ) {
			return __( 'USDT payments are not available in this currency.', 'crypto-gateway-usdt' );
		}
		return null;
	}

	/**
	 * Starts or resumes the payment and sends the buyer to the hosted page.
	 *
	 * Never marks the order paid: only a verified webhook or a status read does.
	 *
	 * @param int $order_id Order id.
	 * @return array<string,string>
	 */
	public function process_payment( $order_id ): array {
		$order = wc_get_order( $order_id );
		if ( ! $order instanceof \WC_Order ) {
			wc_add_notice( __( 'The order could not be found.', 'crypto-gateway-usdt' ), 'error' );
			return array( 'result' => 'failure' );
		}
		try {
			$url = ( new PaymentFlow( $this ) )->start( $order );
		} catch ( UserFacingError $error ) {
			wc_add_notice( $error->getMessage(), 'error' );
			return array( 'result' => 'failure' );
		}
		if ( function_exists( 'WC' ) && WC()->cart ) {
			WC()->cart->empty_cart();
		}
		return array(
			'result'   => 'success',
			'redirect' => $url,
		);
	}
}
