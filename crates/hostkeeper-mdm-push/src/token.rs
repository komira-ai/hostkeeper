//! A device's push token: 32 raw bytes, as NanoHUB keeps them and as an `MDM` `TokenUpdate`
//! carries them, and 64 lower-case hex digits in the URL of APNs.
//!
//! The migration of NanoHUB's enrollments (CM14) reads the raw bytes of `<id>.token` and keeps
//! them as they are; this is the one place the hex form is made.

/// How many bytes a token has.
pub const TOKEN_BYTES: usize = 32;

/// Why a token was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// The token is not 32 bytes.
    #[error("a push token is {TOKEN_BYTES} bytes")]
    Length,
    /// The hex text has a character that is not a hex digit, or is not 64 digits.
    #[error("a push token in hex is {} hex digits", TOKEN_BYTES * 2)]
    Hex,
}

/// A device token.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceToken([u8; TOKEN_BYTES]);

impl DeviceToken {
    /// The token of 32 raw bytes.
    ///
    /// # Errors
    /// [`TokenError::Length`]: `bytes` is not 32 bytes.
    pub fn from_raw(bytes: &[u8]) -> Result<Self, TokenError> {
        <[u8; TOKEN_BYTES]>::try_from(bytes)
            .map(Self)
            .map_err(|_| TokenError::Length)
    }

    /// The token written as 64 hex digits, either case.
    ///
    /// # Errors
    /// [`TokenError::Hex`]: it is not 64 hex digits.
    pub fn from_hex(text: &str) -> Result<Self, TokenError> {
        let raw = hex::decode(text).map_err(|_| TokenError::Hex)?;
        Self::from_raw(&raw).map_err(|_| TokenError::Hex)
    }

    /// The 32 raw bytes, as a store keeps them.
    #[must_use]
    pub fn raw(&self) -> &[u8; TOKEN_BYTES] {
        &self.0
    }

    /// The 64 lower-case hex digits of the URL.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceToken(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_raw_token_is_written_as_64_lower_case_hex_digits() {
        let mut raw = [0_u8; 32];
        raw[0] = 0xAB;
        raw[31] = 0x0F;
        let token = DeviceToken::from_raw(&raw).unwrap();
        let hex = token.to_hex();
        assert_eq!(hex.len(), 64);
        assert!(hex.starts_with("ab00"));
        assert!(hex.ends_with("000f"));
        assert_eq!(token.raw(), &raw);
    }

    #[test]
    fn hex_of_either_case_gives_the_same_token_and_a_round_trip_keeps_it() {
        let lower = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let a = DeviceToken::from_hex(lower).unwrap();
        let b = DeviceToken::from_hex(&lower.to_uppercase()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.to_hex(), lower);
        assert_eq!(DeviceToken::from_raw(a.raw()).unwrap(), a);
    }

    #[test]
    fn a_token_of_another_length_or_with_a_non_hex_digit_is_refused() {
        assert_eq!(DeviceToken::from_raw(&[0; 31]), Err(TokenError::Length));
        assert_eq!(DeviceToken::from_raw(&[0; 33]), Err(TokenError::Length));
        assert_eq!(DeviceToken::from_hex("abc"), Err(TokenError::Hex));
        assert_eq!(DeviceToken::from_hex(&"g".repeat(64)), Err(TokenError::Hex));
        assert_eq!(
            DeviceToken::from_hex(&"ab".repeat(31)),
            Err(TokenError::Hex)
        );
        assert_eq!(
            TokenError::Hex.to_string(),
            "a push token in hex is 64 hex digits"
        );
        assert_eq!(TokenError::Length.to_string(), "a push token is 32 bytes");
    }

    #[test]
    fn a_token_prints_nothing_of_itself() {
        let token = DeviceToken::from_raw(&[7; 32]).unwrap();
        assert_eq!(format!("{token:?}"), "DeviceToken(<redacted>)");
    }
}
