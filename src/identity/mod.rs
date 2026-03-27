//! Node identity: ed25519 keypair with Cosmos-compatible bech32 address.
//!
//! The same keypair serves as:
//! - Thronglets node identity (sign traces)
//! - Valid Oasyce/Cosmos wallet address (if you ever want to use it)
//!
//! "身份证可以去开银行卡，但不用银行卡也能用身份证。"

use ed25519_dalek::{SigningKey, VerifyingKey, Signer, Verifier, Signature};
use rand::rngs::OsRng;
use sha2::{Sha256, Digest};
use std::path::Path;
use std::fs;

/// A node's identity: ed25519 keypair + derived addresses.
pub struct NodeIdentity {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl NodeIdentity {
    /// Generate a new random identity.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        Self { signing_key, verifying_key }
    }

    /// Load from a key file, or generate and save if it doesn't exist.
    pub fn load_or_generate(path: &Path) -> std::io::Result<Self> {
        if path.exists() {
            let bytes = fs::read(path)?;
            if bytes.len() != 32 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "key file must be exactly 32 bytes",
                ));
            }
            let signing_key = SigningKey::from_bytes(&bytes.try_into().unwrap());
            let verifying_key = signing_key.verifying_key();
            Ok(Self { signing_key, verifying_key })
        } else {
            let identity = Self::generate();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, identity.signing_key.to_bytes())?;
            // Restrict key file to owner-only read/write
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
            Ok(identity)
        }
    }

    /// Sign arbitrary bytes.
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    /// Get the public key bytes (32 bytes).
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }

    /// Get the secret key bytes (32 bytes). Used for libp2p identity conversion.
    pub fn secret_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    /// Verify a signature against a public key.
    pub fn verify(public_key: &[u8; 32], message: &[u8], signature: &Signature) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(public_key) else {
            return false;
        };
        vk.verify(message, signature).is_ok()
    }

    /// Derive a Cosmos-compatible bech32 address (oasyce1...).
    /// Uses the same derivation as Cosmos SDK: sha256(pubkey)[..20] -> bech32.
    pub fn oasyce_address(&self) -> String {
        let hash = Sha256::digest(self.verifying_key.as_bytes());
        let addr_bytes = &hash[..20];
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("oasyce").unwrap(), addr_bytes)
            .expect("bech32 encoding should never fail")
    }

    /// Short hex ID for display (first 8 chars of hex pubkey).
    pub fn short_id(&self) -> String {
        hex::encode(&self.public_key_bytes()[..4])
    }
}

// We need hex encoding but don't want another dep for just this
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generate_and_sign() {
        let id = NodeIdentity::generate();
        let msg = b"hello thronglets";
        let sig = id.sign(msg);
        assert!(NodeIdentity::verify(&id.public_key_bytes(), msg, &sig));
    }

    #[test]
    fn verify_rejects_wrong_message() {
        let id = NodeIdentity::generate();
        let sig = id.sign(b"correct");
        assert!(!NodeIdentity::verify(&id.public_key_bytes(), b"wrong", &sig));
    }

    #[test]
    fn persistence_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("node.key");

        let id1 = NodeIdentity::load_or_generate(&path).unwrap();
        let id2 = NodeIdentity::load_or_generate(&path).unwrap();

        assert_eq!(id1.public_key_bytes(), id2.public_key_bytes());
    }

    #[test]
    fn oasyce_address_format() {
        let id = NodeIdentity::generate();
        let addr = id.oasyce_address();
        assert!(addr.starts_with("oasyce1"));
    }

    #[test]
    fn short_id_is_8_chars() {
        let id = NodeIdentity::generate();
        assert_eq!(id.short_id().len(), 8);
    }
}
