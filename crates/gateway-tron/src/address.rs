use gateway_domain::AddressKey;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Every TRON account address starts with this byte in its canonical form.
const TRON_ADDRESS_PREFIX: u8 = 0x41;
const TRON_ADDRESS_BYTES: usize = 21;
const CHECKSUM_BYTES: usize = 4;

/// Decodes a base58check TRON address into its canonical 21 bytes.
///
/// The checksum is verified: a mistyped address is refused rather than turned
/// into a valid-looking key that money could be sent to.
///
/// # Errors
///
/// Returns [`TronAddressError`] for anything that is not a well-formed TRON
/// address.
pub fn from_base58(value: &str) -> Result<AddressKey, TronAddressError> {
    let decoded = bs58::decode(value.trim())
        .into_vec()
        .map_err(|_| TronAddressError::NotBase58)?;
    if decoded.len() != TRON_ADDRESS_BYTES + CHECKSUM_BYTES {
        return Err(TronAddressError::WrongLength);
    }
    let (body, checksum) = decoded.split_at(TRON_ADDRESS_BYTES);
    if checksum != &double_sha256(body)[..CHECKSUM_BYTES] {
        return Err(TronAddressError::BadChecksum);
    }
    if body.first() != Some(&TRON_ADDRESS_PREFIX) {
        return Err(TronAddressError::WrongPrefix);
    }
    AddressKey::new(body.to_vec()).map_err(|_| TronAddressError::WrongLength)
}

/// Encodes canonical bytes back into the base58check form used by wallets and
/// explorers. Display only: comparisons always use the bytes.
///
/// # Errors
///
/// Returns [`TronAddressError`] when the bytes are not a TRON address.
pub fn to_base58(address: &AddressKey) -> Result<String, TronAddressError> {
    let bytes = address.as_bytes();
    if bytes.len() != TRON_ADDRESS_BYTES {
        return Err(TronAddressError::WrongLength);
    }
    if bytes.first() != Some(&TRON_ADDRESS_PREFIX) {
        return Err(TronAddressError::WrongPrefix);
    }
    let mut full = bytes.to_vec();
    full.extend_from_slice(&double_sha256(bytes)[..CHECKSUM_BYTES]);
    Ok(bs58::encode(full).into_string())
}

/// Builds a canonical address from the 20-byte form used inside event logs.
///
/// # Errors
///
/// Returns [`TronAddressError::WrongLength`] unless exactly 20 bytes are
/// supplied.
pub fn from_evm_bytes(bytes: &[u8]) -> Result<AddressKey, TronAddressError> {
    if bytes.len() != TRON_ADDRESS_BYTES - 1 {
        return Err(TronAddressError::WrongLength);
    }
    let mut canonical = Vec::with_capacity(TRON_ADDRESS_BYTES);
    canonical.push(TRON_ADDRESS_PREFIX);
    canonical.extend_from_slice(bytes);
    AddressKey::new(canonical).map_err(|_| TronAddressError::WrongLength)
}

/// Parses the hexadecimal address forms the node API returns, with or without
/// the `41` prefix and with or without `0x`.
///
/// # Errors
///
/// Returns [`TronAddressError`] when the text is not a TRON address in hex.
pub fn from_hex(value: &str) -> Result<AddressKey, TronAddressError> {
    let trimmed = value.trim();
    let trimmed = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    let bytes = decode_hex(trimmed)?;
    match bytes.len() {
        TRON_ADDRESS_BYTES => {
            if bytes.first() != Some(&TRON_ADDRESS_PREFIX) {
                return Err(TronAddressError::WrongPrefix);
            }
            AddressKey::new(bytes).map_err(|_| TronAddressError::WrongLength)
        }
        20 => from_evm_bytes(&bytes),
        _ => Err(TronAddressError::WrongLength),
    }
}

/// Decodes a hexadecimal string into bytes.
///
/// # Errors
///
/// Returns [`TronAddressError::NotHex`] for odd length or non-hex characters.
pub fn decode_hex(value: &str) -> Result<Vec<u8>, TronAddressError> {
    if !value.len().is_multiple_of(2) {
        return Err(TronAddressError::NotHex);
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let raw = value.as_bytes();
    for pair in raw.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

const fn hex_value(byte: u8) -> Result<u8, TronAddressError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(TronAddressError::NotHex),
    }
}

fn double_sha256(bytes: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(bytes);
    Sha256::digest(first).into()
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum TronAddressError {
    #[error("a TRON address must be base58")]
    NotBase58,
    #[error("a TRON address must be 21 bytes plus a 4-byte checksum")]
    WrongLength,
    #[error("the address checksum does not match")]
    BadChecksum,
    #[error("a TRON address starts with 0x41")]
    WrongPrefix,
    #[error("the value is not hexadecimal")]
    NotHex,
}

#[cfg(test)]
mod tests {
    use super::{TronAddressError, from_base58, from_evm_bytes, from_hex, to_base58};

    /// The official USDT TRC20 contract, which is also a convenient known
    /// address vector.
    const USDT_CONTRACT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";
    const USDT_CONTRACT_HEX: &str = "41a614f803b6fd780986a42c78ec9c7f77e6ded13c";

    #[test]
    fn a_known_address_round_trips_through_its_canonical_bytes() -> Result<(), TronAddressError> {
        let key = from_base58(USDT_CONTRACT)?;

        assert_eq!(key.to_hex(), USDT_CONTRACT_HEX);
        assert_eq!(to_base58(&key)?, USDT_CONTRACT);
        Ok(())
    }

    #[test]
    fn the_same_address_in_different_forms_is_the_same_key() -> Result<(), TronAddressError> {
        let from_text = from_base58(USDT_CONTRACT)?;
        let from_prefixed_hex = from_hex(USDT_CONTRACT_HEX)?;
        let from_zero_x = from_hex(&format!("0x{USDT_CONTRACT_HEX}"))?;
        let from_log_bytes = from_hex(&USDT_CONTRACT_HEX[2..])?;

        assert_eq!(from_text, from_prefixed_hex);
        assert_eq!(from_text, from_zero_x);
        assert_eq!(from_text, from_log_bytes);
        Ok(())
    }

    #[test]
    fn a_mistyped_address_is_refused_instead_of_accepted() {
        // One character changed: base58 still decodes, the checksum does not.
        assert_eq!(
            from_base58("TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6u"),
            Err(TronAddressError::BadChecksum)
        );
        assert_eq!(from_base58(""), Err(TronAddressError::WrongLength));
        assert_eq!(from_base58("0OIl"), Err(TronAddressError::NotBase58));
    }

    #[test]
    fn an_address_of_the_wrong_shape_is_refused() {
        assert_eq!(from_hex("deadbeef"), Err(TronAddressError::WrongLength));
        assert_eq!(from_hex("zz"), Err(TronAddressError::NotHex));
        assert_eq!(from_hex("abc"), Err(TronAddressError::NotHex));
        assert_eq!(
            from_evm_bytes(&[0_u8; 19]),
            Err(TronAddressError::WrongLength)
        );
        // A 21-byte value that does not start with 0x41 is not a TRON address,
        // however well-formed it looks.
        assert_eq!(
            from_hex("42a614f803b6fd780986a42c78ec9c7f77e6ded13c"),
            Err(TronAddressError::WrongPrefix)
        );
    }

    #[test]
    fn a_log_address_gains_the_canonical_prefix() -> Result<(), TronAddressError> {
        let key = from_evm_bytes(&super::decode_hex(&USDT_CONTRACT_HEX[2..])?)?;

        assert_eq!(key.to_hex(), USDT_CONTRACT_HEX);
        Ok(())
    }
}
