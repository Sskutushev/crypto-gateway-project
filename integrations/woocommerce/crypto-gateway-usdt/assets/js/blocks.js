/* Registers the USDT (TRC20) method with the Cart and Checkout Blocks.
 * Plain script on WordPress and WooCommerce globals: no build step. */
( function () {
	'use strict';

	var registry = window.wc && window.wc.wcBlocksRegistry;
	var settingsApi = window.wc && window.wc.wcSettings;
	var element = window.wp && window.wp.element;
	var entities = window.wp && window.wp.htmlEntities;
	if ( ! registry || ! settingsApi || ! element || ! entities ) {
		return;
	}

	var settings = settingsApi.getSetting( 'crypto_gateway_usdt_data', {} );
	var title = entities.decodeEntities( settings.title || 'USDT (TRC20)' );
	var description = entities.decodeEntities( settings.description || '' );

	function Content() {
		return element.createElement( 'p', null, description );
	}

	function Label( props ) {
		var PaymentMethodLabel = props.components && props.components.PaymentMethodLabel;
		return PaymentMethodLabel
			? element.createElement( PaymentMethodLabel, { text: title } )
			: element.createElement( 'span', null, title );
	}

	registry.registerPaymentMethod( {
		name: 'crypto_gateway_usdt',
		label: element.createElement( Label, null ),
		content: element.createElement( Content, null ),
		edit: element.createElement( Content, null ),
		canMakePayment: function () {
			return true;
		},
		ariaLabel: title,
		supports: {
			features: settings.supports || [ 'products' ],
		},
	} );
}() );
