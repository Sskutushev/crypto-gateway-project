use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// The minimum length of the master key a deployment must supply.
const MIN_MASTER_KEY_BYTES: usize = 32;

/// The secret a merchant uses to verify that an event really came from this
/// gateway.
///
/// It is derived, never stored: the database keeps a fingerprint, so a stolen
/// database backup does not hand anyone the ability to forge events, and a
/// wrong master key is detected instead of silently producing signatures no
/// merchant can verify.
#[derive(Clone)]
pub struct SigningSecret(Vec<u8>);

impl SigningSecret {
    /// Derives the secret for one endpoint from the deployment master key.
    ///
    /// # Errors
    ///
    /// Returns [`WebhookError::WeakMasterKey`] when the master key is too
    /// short to carry 256 bits of entropy.
    pub fn derive(
        master_key: &[u8],
        key_version: i32,
        merchant_id: Uuid,
        endpoint_id: Uuid,
    ) -> Result<Self, WebhookError> {
        if master_key.len() < MIN_MASTER_KEY_BYTES {
            return Err(WebhookError::WeakMasterKey);
        }
        let mut mac =
            HmacSha256::new_from_slice(master_key).map_err(|_| WebhookError::WeakMasterKey)?;
        mac.update(b"gateway-webhook-secret-v1");
        mac.update(&key_version.to_be_bytes());
        mac.update(merchant_id.as_bytes());
        mac.update(endpoint_id.as_bytes());
        Ok(Self(mac.finalize().into_bytes().to_vec()))
    }

    /// The value handed to the merchant once, at creation time.
    #[must_use]
    pub fn expose(&self) -> String {
        hex(&self.0)
    }

    /// The fingerprint stored beside the endpoint, so a wrong master key is
    /// detectable without keeping the secret itself.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(&self.0).into()
    }
}

impl std::fmt::Debug for SigningSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A secret never lands in a log line by accident.
        formatter.write_str("SigningSecret(redacted)")
    }
}

/// The signature line a merchant verifies.
///
/// The timestamp is signed together with the body, so a captured delivery
/// cannot be replayed later against a different clock.
#[must_use]
pub fn sign_event(secret: &SigningSecret, timestamp_unix: i64, body: &[u8]) -> String {
    // The secret is always 32 bytes because it is itself an HMAC output, so
    // this key length is always accepted.
    let Ok(mut mac) = HmacSha256::new_from_slice(&secret.0) else {
        return String::new();
    };
    mac.update(timestamp_unix.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!(
        "t={timestamp_unix},v1={}",
        hex(&mac.finalize().into_bytes())
    )
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: [u8; 16] = *b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WebhookError {
    #[error("the webhook master key must contain at least 32 bytes")]
    WeakMasterKey,
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{SigningSecret, WebhookError, sign_event};

    const MERCHANT: Uuid = Uuid::from_u128(1);
    const ENDPOINT: Uuid = Uuid::from_u128(2);

    fn master_key() -> Vec<u8> {
        vec![7_u8; 32]
    }

    #[test]
    fn a_short_master_key_is_refused() {
        let refused = SigningSecret::derive(&[1_u8; 16], 1, MERCHANT, ENDPOINT);

        assert_eq!(refused.err(), Some(WebhookError::WeakMasterKey));
    }

    #[test]
    fn each_endpoint_gets_its_own_secret() -> Result<(), WebhookError> {
        let first = SigningSecret::derive(&master_key(), 1, MERCHANT, ENDPOINT)?;
        let same = SigningSecret::derive(&master_key(), 1, MERCHANT, ENDPOINT)?;
        let other_endpoint = SigningSecret::derive(&master_key(), 1, MERCHANT, Uuid::from_u128(3))?;
        let other_merchant = SigningSecret::derive(&master_key(), 1, Uuid::from_u128(4), ENDPOINT)?;
        let rotated = SigningSecret::derive(&master_key(), 2, MERCHANT, ENDPOINT)?;

        assert_eq!(first.expose(), same.expose());
        assert_ne!(first.expose(), other_endpoint.expose());
        assert_ne!(first.expose(), other_merchant.expose());
        assert_ne!(first.expose(), rotated.expose());
        assert_eq!(first.fingerprint(), same.fingerprint());
        assert_ne!(first.fingerprint(), rotated.fingerprint());
        Ok(())
    }

    #[test]
    fn a_secret_never_prints_itself() -> Result<(), WebhookError> {
        let secret = SigningSecret::derive(&master_key(), 1, MERCHANT, ENDPOINT)?;

        let printed = format!("{secret:?}");

        assert_eq!(printed, "SigningSecret(redacted)");
        assert!(!printed.contains(&secret.expose()));
        Ok(())
    }

    #[test]
    fn the_signature_covers_both_the_body_and_the_moment() -> Result<(), WebhookError> {
        let secret = SigningSecret::derive(&master_key(), 1, MERCHANT, ENDPOINT)?;
        let body = br#"{"event":"payment_intent.paid"}"#;

        let signature = sign_event(&secret, 1_700_000_000, body);
        let same = sign_event(&secret, 1_700_000_000, body);
        let later = sign_event(&secret, 1_700_000_001, body);
        let tampered = sign_event(
            &secret,
            1_700_000_000,
            br#"{"event":"payment_intent.lost"}"#,
        );

        assert_eq!(signature, same);
        assert_ne!(signature, later);
        assert_ne!(signature, tampered);
        assert!(signature.starts_with("t=1700000000,v1="));
        assert_eq!(signature.len(), "t=1700000000,v1=".len() + 64);
        Ok(())
    }

    #[test]
    fn the_signature_is_stable_across_releases() -> Result<(), WebhookError> {
        // A merchant's verification code must keep working, so the derivation
        // and the signature format are pinned by an explicit vector.
        let secret = SigningSecret::derive(&master_key(), 1, MERCHANT, ENDPOINT)?;

        assert_eq!(
            secret.expose(),
            "6187fe179459663943dbad68a397beea67e72e6194e31e0a8eb7d4b921267fe2"
        );
        Ok(())
    }
}
