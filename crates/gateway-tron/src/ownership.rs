//! Proof that whoever registers a collector address controls its key.
//!
//! A merchant-owned collector receives that merchant's money, so registering
//! an address the merchant does not control would route payments to a
//! stranger. The merchant signs a fixed statement with the wallet that holds
//! the address (`TronLink` `signMessageV2`, TIP-191), and the gateway recovers
//! the signer and compares addresses as canonical bytes.
//!
//! The statement names the gateway purpose, the merchant, the address and the
//! moment it was issued, so a signature made for one merchant, one address or
//! long ago cannot be replayed for another registration.

use alloy_primitives::{Signature, keccak256};
use gateway_domain::AddressKey;
use thiserror::Error;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::address::{decode_hex, from_evm_bytes, to_base58};

/// The prefix TRON wallets put in front of a signed message (TIP-191).
const TRON_MESSAGE_PREFIX: &str = "\x19TRON Signed Message:\n";

/// How long a signed statement stays usable after it was issued.
pub const OWNERSHIP_PROOF_TTL: Duration = Duration::hours(24);

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OwnershipError {
    #[error("the address is not a TRON address")]
    Address,
    #[error("the signature is not 65 bytes of hex")]
    Signature,
    #[error("no public key can be recovered from this signature")]
    Unrecoverable,
    #[error("the statement was signed by another address")]
    WrongSigner,
    #[error("the statement was issued in the future or more than 24 hours ago")]
    Expired,
}

/// The exact text the merchant signs.
///
/// # Errors
///
/// Returns [`OwnershipError::Address`] when `address` is not a TRON address.
pub fn ownership_statement(
    merchant_id: Uuid,
    address: &AddressKey,
    issued_at: OffsetDateTime,
) -> Result<String, OwnershipError> {
    let address = to_base58(address).map_err(|_| OwnershipError::Address)?;
    let issued = issued_at
        .format(&Rfc3339)
        .map_err(|_| OwnershipError::Expired)?;
    Ok(format!(
        "Self-hosted payment gateway: collector ownership\nmerchant: {merchant_id}\naddress: {address}\nissued: {issued}"
    ))
}

/// Verifies that `signature_hex` is the owner of `address` signing the
/// statement for `merchant_id` issued at `issued_at`, and that it is fresh.
///
/// # Errors
///
/// Returns [`OwnershipError`] naming why the proof does not hold.
pub fn verify_ownership(
    merchant_id: Uuid,
    address: &AddressKey,
    issued_at: OffsetDateTime,
    signature_hex: &str,
    now: OffsetDateTime,
) -> Result<(), OwnershipError> {
    if issued_at > now + Duration::minutes(5) || now - issued_at > OWNERSHIP_PROOF_TTL {
        return Err(OwnershipError::Expired);
    }
    let statement = ownership_statement(merchant_id, address, issued_at)?;
    let signer = recover_signer(&statement, signature_hex)?;
    if signer.as_bytes() == address.as_bytes() {
        Ok(())
    } else {
        Err(OwnershipError::WrongSigner)
    }
}

/// The TRON address that produced a TIP-191 signature over `message`.
///
/// # Errors
///
/// Returns [`OwnershipError`] when the signature is malformed or unrecoverable.
pub fn recover_signer(message: &str, signature_hex: &str) -> Result<AddressKey, OwnershipError> {
    let digest = message_digest(message);
    let trimmed = signature_hex.trim();
    let raw = decode_hex(trimmed.strip_prefix("0x").unwrap_or(trimmed))
        .map_err(|_| OwnershipError::Signature)?;
    let raw: [u8; 65] = raw.try_into().map_err(|_| OwnershipError::Signature)?;
    let signature = Signature::from_raw_array(&raw).map_err(|_| OwnershipError::Signature)?;
    let signer = signature
        .recover_address_from_prehash(&digest)
        .map_err(|_| OwnershipError::Unrecoverable)?;
    from_evm_bytes(signer.as_slice()).map_err(|_| OwnershipError::Unrecoverable)
}

fn message_digest(message: &str) -> alloy_primitives::B256 {
    let mut payload = Vec::with_capacity(TRON_MESSAGE_PREFIX.len() + 20 + message.len());
    payload.extend_from_slice(TRON_MESSAGE_PREFIX.as_bytes());
    payload.extend_from_slice(message.len().to_string().as_bytes());
    payload.extend_from_slice(message.as_bytes());
    keccak256(payload)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::keccak256;
    use k256::ecdsa::SigningKey;
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    use super::{OwnershipError, message_digest, ownership_statement, verify_ownership};
    use crate::address::from_evm_bytes;

    const MERCHANT: Uuid = Uuid::from_u128(42);

    /// A wallet: its signing key and the TRON address derived from it.
    fn wallet(
        seed: u8,
    ) -> Result<(SigningKey, gateway_domain::AddressKey), Box<dyn std::error::Error>> {
        let key = SigningKey::from_bytes(&[seed; 32].into())?;
        let public = key.verifying_key().to_sec1_point(false);
        let hash = keccak256(&public.as_bytes()[1..]);
        Ok((key, from_evm_bytes(&hash[12..])?))
    }

    /// What a TRON wallet returns for `signMessageV2`: r || s || v with v = 27/28.
    fn sign(key: &SigningKey, message: &str) -> String {
        let digest = message_digest(message);
        let (signature, recovery) = key.sign_prehash_recoverable(digest.as_slice());
        let mut raw = signature.to_bytes().to_vec();
        raw.push(27 + recovery.to_byte());
        alloy_primitives::hex::encode(raw)
    }

    #[test]
    fn the_owner_signing_the_fresh_statement_is_accepted() -> Result<(), Box<dyn std::error::Error>>
    {
        let (key, address) = wallet(7)?;
        let issued = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        let signature = sign(&key, &ownership_statement(MERCHANT, &address, issued)?);
        assert_eq!(
            verify_ownership(
                MERCHANT,
                &address,
                issued,
                &signature,
                issued + Duration::hours(1)
            ),
            Ok(())
        );
        Ok(())
    }

    #[test]
    fn a_signature_for_another_address_merchant_or_time_is_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let (key, address) = wallet(7)?;
        let (_, other_address) = wallet(8)?;
        let issued = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        let now = issued + Duration::hours(1);
        let signature = sign(&key, &ownership_statement(MERCHANT, &address, issued)?);

        // The key signed for its own address; it proves nothing about another.
        assert_eq!(
            verify_ownership(MERCHANT, &other_address, issued, &signature, now),
            Err(OwnershipError::WrongSigner)
        );
        // Reused for a different merchant or issue time, the statement differs.
        assert_eq!(
            verify_ownership(Uuid::from_u128(43), &address, issued, &signature, now),
            Err(OwnershipError::WrongSigner)
        );
        assert_eq!(
            verify_ownership(
                MERCHANT,
                &address,
                issued + Duration::seconds(1),
                &signature,
                now
            ),
            Err(OwnershipError::WrongSigner)
        );
        // Stale, future-dated and malformed proofs are refused.
        assert_eq!(
            verify_ownership(
                MERCHANT,
                &address,
                issued,
                &signature,
                issued + Duration::hours(25)
            ),
            Err(OwnershipError::Expired)
        );
        assert_eq!(
            verify_ownership(
                MERCHANT,
                &address,
                issued,
                &signature,
                issued - Duration::hours(1)
            ),
            Err(OwnershipError::Expired)
        );
        assert_eq!(
            verify_ownership(MERCHANT, &address, issued, "abcd", now),
            Err(OwnershipError::Signature)
        );
        Ok(())
    }
}
