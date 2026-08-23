//! Capability-based pairing tokens.
//!
//! A short pairing code is printed on node start and is only revealed over
//! loopback (`GET /v1/pairing`). `POST /v1/pair` exchanges the code for a
//! bearer token. The token is hashed at rest on the node. Secrets never enter
//! the guest.

use rand::Rng;
use sha2::{Digest, Sha256};

/// Capabilities a paired client may exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Park / unpark, read pairing state.
    Operator,
    /// Create and end leases.
    Lease,
}

/// A stored pairing. The raw token is not kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedToken {
    /// SHA-256 hex of the bearer token.
    pub token_hash: String,
    /// Capabilities granted at pair time.
    pub capabilities: Vec<Capability>,
}

/// Live pairing booth on the node.
#[derive(Debug, Clone)]
pub struct PairingBooth {
    /// Current code (`XXXX-XXXX`). Rotates after a successful pair.
    pub code: String,
    /// Issued tokens (hashed).
    pub tokens: Vec<IssuedToken>,
}

impl PairingBooth {
    /// New booth with a fresh code and no tokens.
    pub fn new() -> Self {
        Self {
            code: generate_code(),
            tokens: Vec::new(),
        }
    }

    /// Exchange a code for a bearer token. Default grants operator + lease
    /// so the same client can park and `berth up`.
    pub fn pair(&mut self, presented: &str) -> Result<String, PairError> {
        if !codes_equal(&self.code, presented) {
            return Err(PairError::BadCode);
        }
        let token = generate_token();
        self.tokens.push(IssuedToken {
            token_hash: hash_token(&token),
            capabilities: vec![Capability::Operator, Capability::Lease],
        });
        self.code = generate_code();
        Ok(token)
    }

    /// Look up a presented bearer token.
    pub fn authorize(&self, token: &str, need: Capability) -> Result<(), PairError> {
        let hash = hash_token(token);
        let issued = self
            .tokens
            .iter()
            .find(|t| t.token_hash == hash)
            .ok_or(PairError::UnknownToken)?;
        if issued.capabilities.contains(&need) {
            Ok(())
        } else {
            Err(PairError::MissingCapability(need))
        }
    }
}

impl Default for PairingBooth {
    fn default() -> Self {
        Self::new()
    }
}

/// Pairing failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PairError {
    /// Code mismatch.
    #[error("pairing code rejected")]
    BadCode,
    /// Token not on this node.
    #[error("unknown pairing token")]
    UnknownToken,
    /// Token lacks the capability.
    #[error("token missing capability {0:?}")]
    MissingCapability(Capability),
}

fn generate_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    let chars: String = (0..8)
        .map(|_| {
            let idx = rng.gen_range(0..ALPHABET.len());
            ALPHABET[idx] as char
        })
        .collect();
    format!("{}-{}", &chars[..4], &chars[4..])
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    hex::encode(bytes)
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn codes_equal(expected: &str, presented: &str) -> bool {
    normalize(expected) == normalize(presented)
}

fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_grants_capabilities_and_rotates_code() {
        let mut booth = PairingBooth::new();
        let code = booth.code.clone();
        let token = booth.pair(&code).expect("pair");
        assert!(booth.authorize(&token, Capability::Lease).is_ok());
        assert!(booth.authorize(&token, Capability::Operator).is_ok());
        assert_ne!(booth.code, code);
        assert_eq!(booth.pair(&code), Err(PairError::BadCode));
    }

    #[test]
    fn unknown_token_rejected() {
        let booth = PairingBooth::new();
        assert_eq!(
            booth.authorize("deadbeef", Capability::Lease),
            Err(PairError::UnknownToken)
        );
    }
}
