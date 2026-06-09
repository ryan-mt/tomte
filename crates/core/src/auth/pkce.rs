use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub struct Pkce {
    pub code_verifier: String,
    pub code_challenge: String,
}

pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 64];
    rand::thread_rng().fill_bytes(&mut bytes);
    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    Pkce {
        code_verifier,
        code_challenge,
    }
}

pub fn random_state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// base64url alphabet (RFC 4648 §5) with NO padding — the only characters a
    /// PKCE verifier/challenge or the state nonce may contain.
    fn is_base64url_no_pad(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    #[test]
    fn verifier_and_challenge_are_well_formed_base64url() {
        let pkce = generate_pkce();
        // 64 random bytes → 86 base64url chars, unpadded; within RFC 7636's
        // 43..=128 verifier-length bound.
        assert_eq!(pkce.code_verifier.len(), 86);
        assert!((43..=128).contains(&pkce.code_verifier.len()));
        assert!(is_base64url_no_pad(&pkce.code_verifier));
        // 32-byte SHA-256 digest → 43 base64url chars, unpadded.
        assert_eq!(pkce.code_challenge.len(), 43);
        assert!(is_base64url_no_pad(&pkce.code_challenge));
        assert!(!pkce.code_verifier.contains('=') && !pkce.code_challenge.contains('='));
    }

    #[test]
    fn challenge_is_s256_of_the_verifier() {
        // The challenge MUST be base64url(SHA256(verifier)) (the `S256` method) —
        // if this drifts, the authorization server rejects every token exchange.
        let pkce = generate_pkce();
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(pkce.code_verifier.as_bytes()));
        assert_eq!(pkce.code_challenge, expected);
    }

    #[test]
    fn each_pkce_and_state_is_unique() {
        // Verifiers and CSRF state nonces must be unpredictable — two draws must
        // not collide (a fixed value would defeat PKCE/CSRF entirely).
        assert_ne!(generate_pkce().code_verifier, generate_pkce().code_verifier);
        let s = random_state();
        assert_eq!(s.len(), 43); // 32 random bytes, unpadded base64url
        assert!(is_base64url_no_pad(&s));
        assert_ne!(random_state(), random_state());
    }
}
