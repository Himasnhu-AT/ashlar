//! Compile-time string obfuscation.
//!
//! [`obfuscated!`] stores a string XOR-ed with a fixed keystream, so the plaintext does not appear
//! in the binary's read-only data, and recovers it at run time. Use it for values that should not
//! be trivially greppable in a shipped binary — service addresses or keys baked in at build time —
//! not for text shown to the user.
//!
//! This is obfuscation, not encryption: the key travels with the program, so anyone running it can
//! still recover the value. It only raises the bar on a `strings` dump of the file at rest.
//!
//! ```
//! let addr = ashlar::obfuscated!("https://example.invalid/v1");
//! assert_eq!(addr, "https://example.invalid/v1");
//! ```

/// A keystream byte for position `i`. The same function runs at compile time (to obfuscate) and at
/// run time (to recover), so the two always agree. It is a plain mix, not a cryptographic one.
#[inline]
pub const fn keystream(key: u64, i: usize) -> u8 {
    let mixed = key
        .wrapping_add(i as u64)
        .wrapping_mul(0x0100_0000_01B3) // FNV prime: spreads the index across the byte
        .rotate_left((i % 61) as u32);
    (mixed >> ((i % 8) * 8)) as u8
}

/// XOR `src` against the keystream, at compile time. `N` must equal `src.len()`.
pub const fn encode<const N: usize>(src: &[u8], key: u64) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = src[i] ^ keystream(key, i);
        i += 1;
    }
    out
}

/// Recover the original bytes at run time. Not `const`, so the plaintext is built on the heap and
/// never lands in the binary's static data. Invalid UTF-8 (impossible from a `&str` input) yields
/// an empty string rather than panicking.
pub fn decode(enc: &[u8], key: u64) -> String {
    let mut out = Vec::with_capacity(enc.len());
    let mut i = 0;
    while i < enc.len() {
        out.push(enc[i] ^ keystream(key, i));
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// A string literal obfuscated at compile time. Evaluates, each time, to a fresh [`String`] holding
/// the original text; the binary stores only the XOR-ed bytes and the key.
///
/// `obfuscated!("text")` uses a built-in key; `obfuscated!("text", 0xA1B2_C3D4)` sets your own, so
/// two values do not share a keystream.
#[macro_export]
macro_rules! obfuscated {
    ($s:expr) => {
        $crate::obfuscated!($s, 0x9E37_79B9_7F4A_7C15u64)
    };
    ($s:expr, $key:expr) => {{
        const __OBF_KEY: u64 = $key;
        const __OBF_N: usize = $s.len();
        const __OBF_ENC: [u8; __OBF_N] = $crate::obfuscate::encode::<__OBF_N>($s.as_bytes(), __OBF_KEY);
        $crate::obfuscate::decode(&__OBF_ENC, __OBF_KEY)
    }};
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trips_various_strings() {
        assert_eq!(obfuscated!(""), "");
        assert_eq!(obfuscated!("a"), "a");
        assert_eq!(obfuscated!("https://api.example.invalid"), "https://api.example.invalid");
        let utf8 = obfuscated!("key_ö_🔑_1234567890");
        assert_eq!(utf8, "key_ö_🔑_1234567890");
    }

    #[test]
    fn a_custom_key_changes_the_stored_bytes() {
        const S: &str = "same-text";
        const A: [u8; S.len()] = super::encode::<{ S.len() }>(S.as_bytes(), 1);
        const B: [u8; S.len()] = super::encode::<{ S.len() }>(S.as_bytes(), 2);
        assert_ne!(A, B);
        assert_eq!(super::decode(&A, 1), S);
        assert_eq!(super::decode(&B, 2), S);
    }

    #[test]
    fn stored_bytes_are_not_the_plaintext() {
        const S: &str = "find-me-in-the-binary";
        const ENC: [u8; S.len()] = super::encode::<{ S.len() }>(S.as_bytes(), 0x1234);
        // No window of the obfuscated bytes equals the plaintext.
        assert!(!ENC.windows(S.len()).any(|w| w == S.as_bytes()));
        assert_eq!(super::decode(&ENC, 0x1234), S);
    }
}
