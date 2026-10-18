//! Credentials helpers (qBittorrent `Utils::Password`, `Utils::APIKey`):
//! PBKDF2-HMAC-SHA512 password hashes, random passwords, API keys and
//! constant-time comparison. All randomness comes from the OS CSPRNG.

use std::num::NonZeroU32;

use aws_lc_rs::pbkdf2;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use subtle::ConstantTimeEq;

/// qBittorrent's PBKDF2 parameters.
const ITERATIONS: u32 = 100_000;
const SALT_LEN: usize = 16;
const HASH_LEN: usize = 64;
/// Alphabet of generated passwords and API keys (no look-alike characters).
const ALPHABET: &[u8] = b"23456789ABCDEFGHIJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz";
pub const API_KEY_PREFIX: &str = "qbt_";
const API_KEY_RANDOM_LEN: usize = 28;
/// Length of the temporary WebUI password.
pub const TEMP_PASSWORD_LEN: usize = 9;

fn iterations() -> NonZeroU32 {
    NonZeroU32::new(ITERATIONS).expect("non-zero")
}

/// Fills `buf` from the OS CSPRNG.
pub fn random_bytes(buf: &mut [u8]) {
    aws_lc_rs::rand::fill(buf).expect("system random number generator failed");
}

/// Uniformly random index below `n` (rejection sampling, no modulo bias).
fn random_below(n: usize) -> usize {
    debug_assert!(n > 0 && n <= 256);
    let limit = 256 - (256 % n);
    loop {
        let mut b = [0u8; 1];
        random_bytes(&mut b);
        if (b[0] as usize) < limit {
            return b[0] as usize % n;
        }
    }
}

/// Random password from qBittorrent's alphabet.
pub fn generate_password(len: usize) -> String {
    (0..len).map(|_| ALPHABET[random_below(ALPHABET.len())] as char).collect()
}

/// `qbt_` + 28 random characters.
pub fn generate_api_key() -> String {
    format!("{API_KEY_PREFIX}{}", generate_password(API_KEY_RANDOM_LEN))
}

/// A stored API key is only honoured if it has the expected shape.
pub fn is_valid_api_key(key: &str) -> bool {
    key.len() == API_KEY_PREFIX.len() + API_KEY_RANDOM_LEN
        && key.starts_with(API_KEY_PREFIX)
        && key[API_KEY_PREFIX.len()..].bytes().all(|b| ALPHABET.contains(&b))
}

/// Constant-time byte comparison (length differences are not hidden, the
/// content is).
pub fn slow_equals(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Hashes `password` in qBittorrent's format: `base64(salt):base64(hash)`.
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; SALT_LEN];
    random_bytes(&mut salt);
    let mut out = [0u8; HASH_LEN];
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA512, iterations(), &salt, password.as_bytes(), &mut out);
    format!("{}:{}", B64.encode(salt), B64.encode(out))
}

/// Verifies `password` against a stored hash. Accepts the plain
/// `salt:hash` form and qBittorrent's ini form `@ByteArray(salt:hash)`.
/// The comparison of the derived key is constant-time.
pub fn verify_password(stored: &str, password: &str) -> bool {
    let stored = stored.trim();
    let stored = stored
        .strip_prefix("@ByteArray(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(stored);
    let Some((salt, hash)) = stored.split_once(':') else { return false };
    let (Ok(salt), Ok(hash)) = (B64.decode(salt), B64.decode(hash)) else { return false };
    if salt.is_empty() || hash.len() != HASH_LEN {
        return false;
    }
    pbkdf2::verify(pbkdf2::PBKDF2_HMAC_SHA512, iterations(), &salt, password.as_bytes(), &hash).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trip_and_format() {
        let h = hash_password("adminadmin");
        let (salt, hash) = h.split_once(':').unwrap();
        assert_eq!(B64.decode(salt).unwrap().len(), 16);
        assert_eq!(B64.decode(hash).unwrap().len(), 64);
        assert!(verify_password(&h, "adminadmin"));
        assert!(!verify_password(&h, "adminadmin2"));
        assert!(!verify_password(&h, ""));
        // ini form
        assert!(verify_password(&format!("@ByteArray({h})"), "adminadmin"));
        // garbage never verifies and never panics
        for bad in ["", ":", "abc", "a:b", "@ByteArray()", "::::", "AAAA:AAAA"] {
            assert!(!verify_password(bad, "x"));
        }
    }

    #[test]
    fn verifies_qbittorrent_generated_hash() {
        // Hash of "adminadmin" as stored by qBittorrent 4.2+ (well-known default).
        let stored = "ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==";
        assert!(verify_password(stored, "adminadmin"));
        assert!(!verify_password(stored, "admin"));
    }

    #[test]
    fn api_keys() {
        let k = generate_api_key();
        assert_eq!(k.len(), 32);
        assert!(is_valid_api_key(&k));
        assert!(!is_valid_api_key("qbt_short"));
        assert!(!is_valid_api_key("abc_0123456789012345678901234567"));
        assert!(!is_valid_api_key("qbt_000000000000000000000000000O")); // '0'/'O' not in alphabet
        assert_ne!(generate_api_key(), generate_api_key());
    }

    #[test]
    fn passwords_use_alphabet() {
        let p = generate_password(TEMP_PASSWORD_LEN);
        assert_eq!(p.len(), 9);
        assert!(p.bytes().all(|b| ALPHABET.contains(&b)));
    }

    #[test]
    fn constant_time_compare() {
        assert!(slow_equals(b"abc", b"abc"));
        assert!(!slow_equals(b"abc", b"abd"));
        assert!(!slow_equals(b"abc", b"abcd"));
        assert!(slow_equals(b"", b""));
    }
}
