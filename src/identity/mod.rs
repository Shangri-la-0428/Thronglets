//! Node identity and local device binding.
//!
//! The same ed25519 keypair currently serves as:
//! - Thronglets node identity (sign traces)
//! - `device identity` for Oasyce Identity V1
//!
//! Owner / wallet remains a higher-level root account and is modeled as
//! optional metadata that can be imported or manually bound later.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

const IDENTITY_BINDING_SCHEMA_VERSION: &str = "thronglets.identity.v1";
const CONNECTION_FILE_SCHEMA_VERSION: &str = "thronglets.connection.v1";
const CONNECTION_FILE_SIGNING_DOMAIN: &[u8] = b"thronglets.connection.v1";
const OASYCE_LOCAL_BINDING_SCHEMA_VERSION: &str = "oasyce.identity.v1";
pub const DEFAULT_CONNECTION_FILE_TTL_HOURS: u32 = 24;

/// A node's identity: ed25519 keypair + derived addresses.
pub struct NodeIdentity {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityBinding {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_account: Option<String>,
    pub device_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_from_device: Option<String>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSeedScope {
    Trusted,
    Remembered,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionFile {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_account: Option<String>,
    pub primary_device_identity: String,
    pub primary_device_pubkey: String,
    #[serde(default = "default_connection_seed_scope")]
    pub peer_seed_scope: ConnectionSeedScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peer_seeds: Vec<String>,
    pub exported_at: u64,
    pub expires_at: u64,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OasyceLocalBinding {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl NodeIdentity {
    /// Generate a new random identity.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
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
            Ok(Self {
                signing_key,
                verifying_key,
            })
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
        Self::device_identity_from_pubkey(&self.public_key_bytes())
    }

    /// Current Identity V1 device identity.
    pub fn device_identity(&self) -> String {
        self.oasyce_address()
    }

    /// Short hex ID for display (first 8 chars of hex pubkey).
    pub fn short_id(&self) -> String {
        hex::encode(&self.public_key_bytes()[..4])
    }

    pub fn device_identity_from_pubkey(public_key: &[u8; 32]) -> String {
        let hash = Sha256::digest(public_key);
        let addr_bytes = &hash[..20];
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("oasyce").unwrap(), addr_bytes)
            .expect("bech32 encoding should never fail")
    }
}

impl IdentityBinding {
    pub fn load_or_create(path: &Path, node_identity: &NodeIdentity) -> std::io::Result<Self> {
        let mut binding = if path.exists() {
            let bytes = fs::read(path)?;
            let binding: Self = serde_json::from_slice(&bytes).map_err(invalid_data)?;
            binding.verify_for_node(node_identity)?;
            binding
        } else {
            Self::new(node_identity.device_identity())
        };

        if binding.owner_account.is_none()
            && let Some(owner_account) = load_oasyce_owner_account_hint()?
        {
            binding.owner_account = Some(owner_account);
            if binding.binding_source.is_none() {
                binding.binding_source = Some("oasyce_sdk".into());
            }
            binding.updated_at = now_ms();
        }

        binding.save(path)?;
        Ok(binding)
    }

    pub fn import_owner_account_hint(mut self, owner_account: String) -> std::io::Result<Self> {
        self.ensure_owner_compatible(Some(owner_account.as_str()))?;
        if self.owner_account.is_none() {
            self.owner_account = Some(owner_account);
            if self.binding_source.is_none() {
                self.binding_source = Some("oasyce_sdk".into());
            }
            self.updated_at = now_ms();
        }
        Ok(self)
    }

    pub fn new(device_identity: String) -> Self {
        Self {
            schema_version: IDENTITY_BINDING_SCHEMA_VERSION.to_string(),
            owner_account: None,
            device_identity,
            binding_source: None,
            joined_from_device: None,
            updated_at: now_ms(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(invalid_data)?;
        fs::write(path, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn bind_owner_account(mut self, owner_account: String) -> std::io::Result<Self> {
        self.ensure_owner_compatible(Some(owner_account.as_str()))?;
        self.owner_account = Some(owner_account);
        if self.binding_source.is_none() {
            self.binding_source = Some("manual".into());
        }
        self.updated_at = now_ms();
        Ok(self)
    }

    pub fn joined_via_connection(
        mut self,
        owner_account: Option<String>,
        primary_device: String,
    ) -> std::io::Result<Self> {
        self.ensure_owner_compatible(owner_account.as_deref())?;
        self.owner_account = owner_account;
        self.binding_source = Some("connection_file".into());
        self.joined_from_device = Some(primary_device);
        self.updated_at = now_ms();
        Ok(self)
    }

    pub fn owner_account_or_unbound(&self) -> &str {
        self.owner_account.as_deref().unwrap_or("unbound")
    }

    pub fn require_owner_account(&self) -> std::io::Result<&str> {
        self.owner_account.as_deref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "owner account is not bound; run `thronglets owner-bind --owner-account ...` first",
            )
        })
    }

    pub fn binding_source_or_local(&self) -> &str {
        self.binding_source.as_deref().unwrap_or("local")
    }

    pub fn joined_from_device_or_none(&self) -> &str {
        self.joined_from_device.as_deref().unwrap_or("none")
    }

    pub fn ensure_owner_compatible(&self, requested_owner: Option<&str>) -> std::io::Result<()> {
        if let (Some(current), Some(requested)) = (self.owner_account.as_deref(), requested_owner)
            && current != requested
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "device is already bound to owner {current}; refusing to overwrite with {requested}"
                ),
            ));
        }
        Ok(())
    }

    pub fn verify_for_node(&self, node_identity: &NodeIdentity) -> std::io::Result<()> {
        if self.device_identity.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "device_identity cannot be empty",
            ));
        }
        let expected = node_identity.device_identity();
        if self.device_identity != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "identity binding belongs to device {} but local node is {}",
                    self.device_identity, expected
                ),
            ));
        }
        Ok(())
    }
}

impl ConnectionFile {
    pub fn from_binding(
        binding: &IdentityBinding,
        node_identity: &NodeIdentity,
        ttl_hours: u32,
        peer_seed_scope: ConnectionSeedScope,
        peer_seeds: Vec<String>,
    ) -> std::io::Result<Self> {
        let exported_at = now_ms();
        let mut file = Self {
            schema_version: CONNECTION_FILE_SCHEMA_VERSION.to_string(),
            owner_account: binding.owner_account.clone(),
            primary_device_identity: binding.device_identity.clone(),
            primary_device_pubkey: hex::encode(&node_identity.public_key_bytes()),
            peer_seed_scope,
            peer_seeds,
            exported_at,
            expires_at: exported_at.saturating_add(ttl_hours as u64 * 60 * 60 * 1000),
            signature: String::new(),
        };
        file.sign_with(node_identity);
        Ok(file)
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let bytes = fs::read(path)?;
        let file: Self = serde_json::from_slice(&bytes).map_err(invalid_data)?;
        if file.owner_account.as_deref().is_some_and(str::is_empty) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "owner_account cannot be empty in a connection file",
            ));
        }
        if file.primary_device_identity.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "primary_device_identity cannot be empty",
            ));
        }
        if file.expires_at < file.exported_at {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expires_at must not be earlier than exported_at",
            ));
        }
        file.verify()?;
        if file.is_expired_at(now_ms()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "connection file has expired",
            ));
        }
        Ok(file)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(invalid_data)?;
        fs::write(path, bytes)?;
        Ok(())
    }

    pub fn verify(&self) -> std::io::Result<()> {
        let public_key_vec = hex::decode(&self.primary_device_pubkey).map_err(invalid_data)?;
        if public_key_vec.len() != 32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "primary_device_pubkey must be 32 bytes",
            ));
        }
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(&public_key_vec);
        let derived_device = NodeIdentity::device_identity_from_pubkey(&public_key);
        if derived_device != self.primary_device_identity {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "primary_device_identity does not match signer public key",
            ));
        }

        let signature_vec = hex::decode(&self.signature).map_err(invalid_data)?;
        let signature_bytes: [u8; 64] = signature_vec.try_into().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "signature must be 64 bytes",
            )
        })?;
        let signature = Signature::from_bytes(&signature_bytes);
        if !NodeIdentity::verify(&public_key, &self.signable_bytes(), &signature) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid connection file signature",
            ));
        }
        Ok(())
    }

    pub fn is_expired_at(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at
    }

    pub fn ttl_hours(&self) -> u32 {
        let ttl_ms = self.expires_at.saturating_sub(self.exported_at);
        (ttl_ms / (60 * 60 * 1000)) as u32
    }

    pub fn peer_seed_scope_label(&self) -> &'static str {
        match self.peer_seed_scope {
            ConnectionSeedScope::Trusted => "trusted",
            ConnectionSeedScope::Remembered => "remembered",
        }
    }

    fn sign_with(&mut self, node_identity: &NodeIdentity) {
        let signature = node_identity.sign(&self.signable_bytes());
        self.signature = hex::encode(signature.to_bytes().as_slice());
    }

    fn signable_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(192);
        buf.extend_from_slice(CONNECTION_FILE_SIGNING_DOMAIN);
        push_optional_bytes(&mut buf, Some(self.owner_account.as_deref().unwrap_or("")));
        push_optional_bytes(&mut buf, Some(self.primary_device_identity.as_str()));
        push_optional_bytes(&mut buf, Some(self.primary_device_pubkey.as_str()));
        push_optional_bytes(&mut buf, Some(self.peer_seed_scope_label()));
        buf.extend_from_slice(&(self.peer_seeds.len() as u32).to_le_bytes());
        for seed in &self.peer_seeds {
            push_optional_bytes(&mut buf, Some(seed.as_str()));
        }
        buf.extend_from_slice(&self.exported_at.to_le_bytes());
        buf.extend_from_slice(&self.expires_at.to_le_bytes());
        buf
    }
}

pub fn identity_binding_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("identity.v1.json")
}

fn oasyce_identity_binding_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".oasyce").join("identity.v1.json"))
}

fn load_oasyce_owner_account_hint() -> std::io::Result<Option<String>> {
    let Some(path) = oasyce_identity_binding_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }

    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    let binding: OasyceLocalBinding = match serde_json::from_slice(&bytes).map_err(invalid_data) {
        Ok(binding) => binding,
        Err(_) => return Ok(None),
    };
    if binding.schema_version != OASYCE_LOCAL_BINDING_SCHEMA_VERSION {
        return Ok(None);
    }

    match binding.account {
        Some(account) if !account.trim().is_empty() => Ok(Some(account)),
        _ => Ok(None),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn default_connection_seed_scope() -> ConnectionSeedScope {
    ConnectionSeedScope::Trusted
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

fn push_optional_bytes(buf: &mut Vec<u8>, value: Option<&str>) {
    if let Some(value) = value {
        let bytes = value.as_bytes();
        buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(bytes);
    } else {
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
    }
}

// We need hex encoding but don't want another dep for just this
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn decode(text: &str) -> Result<Vec<u8>, String> {
        if !text.len().is_multiple_of(2) {
            return Err("hex string must have even length".into());
        }
        let mut bytes = Vec::with_capacity(text.len() / 2);
        let chars: Vec<char> = text.chars().collect();
        for pair in chars.chunks(2) {
            let hi = pair[0]
                .to_digit(16)
                .ok_or_else(|| "invalid hex digit".to_string())?;
            let lo = pair[1]
                .to_digit(16)
                .ok_or_else(|| "invalid hex digit".to_string())?;
            bytes.push(((hi << 4) | lo) as u8);
        }
        Ok(bytes)
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
        assert!(!NodeIdentity::verify(
            &id.public_key_bytes(),
            b"wrong",
            &sig
        ));
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

    #[test]
    fn identity_binding_round_trip() {
        let dir = TempDir::new().unwrap();
        let node = NodeIdentity::generate();
        let path = identity_binding_path(dir.path());

        let binding = IdentityBinding::load_or_create(&path, &node).unwrap();
        assert_eq!(binding.device_identity, node.device_identity());
        assert_eq!(binding.owner_account, None);

        let rebound = binding
            .clone()
            .bind_owner_account("oasyce1owner".into())
            .unwrap();
        rebound.save(&path).unwrap();
        let loaded = IdentityBinding::load_or_create(&path, &node).unwrap();
        assert_eq!(loaded.owner_account.as_deref(), Some("oasyce1owner"));
        assert_eq!(loaded.device_identity, node.device_identity());
    }

    #[test]
    fn conflicting_owner_rebind_is_rejected() {
        let node = NodeIdentity::generate();
        let binding = IdentityBinding::new(node.device_identity())
            .bind_owner_account("oasyce1owner".into())
            .unwrap();
        let error = binding
            .bind_owner_account("oasyce1other".into())
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn mismatched_identity_binding_is_rejected() {
        let dir = TempDir::new().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let path = identity_binding_path(dir.path());

        let binding = IdentityBinding::new(node_a.device_identity());
        binding.save(&path).unwrap();

        let error = IdentityBinding::load_or_create(&path, &node_b).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn connection_file_round_trip() {
        let dir = TempDir::new().unwrap();
        let node = NodeIdentity::generate();
        let binding = IdentityBinding {
            schema_version: IDENTITY_BINDING_SCHEMA_VERSION.into(),
            owner_account: Some("oasyce1owner".into()),
            device_identity: node.device_identity(),
            binding_source: Some("manual".into()),
            joined_from_device: None,
            updated_at: 123,
        };
        let file = ConnectionFile::from_binding(
            &binding,
            &node,
            DEFAULT_CONNECTION_FILE_TTL_HOURS,
            ConnectionSeedScope::Trusted,
            vec!["/ip4/10.0.0.1/tcp/4001".into()],
        )
        .unwrap();
        let path = dir.path().join("device.thronglets.json");
        file.save(&path).unwrap();
        let loaded = ConnectionFile::load(&path).unwrap();
        assert_eq!(loaded.owner_account.as_deref(), Some("oasyce1owner"));
        assert_eq!(loaded.primary_device_identity, node.device_identity());
        assert_eq!(
            loaded.primary_device_pubkey,
            hex::encode(&node.public_key_bytes())
        );
        assert_eq!(loaded.peer_seed_scope, ConnectionSeedScope::Trusted);
        assert_eq!(loaded.peer_seeds.len(), 1);
        assert!(loaded.expires_at > loaded.exported_at);
    }

    #[test]
    fn tampered_connection_file_is_rejected() {
        let dir = TempDir::new().unwrap();
        let node = NodeIdentity::generate();
        let binding = IdentityBinding {
            schema_version: IDENTITY_BINDING_SCHEMA_VERSION.into(),
            owner_account: Some("oasyce1owner".into()),
            device_identity: node.device_identity(),
            binding_source: Some("manual".into()),
            joined_from_device: None,
            updated_at: 123,
        };
        let mut file = ConnectionFile::from_binding(
            &binding,
            &node,
            DEFAULT_CONNECTION_FILE_TTL_HOURS,
            ConnectionSeedScope::Trusted,
            vec!["/ip4/10.0.0.1/tcp/4001".into()],
        )
        .unwrap();
        file.owner_account = Some("oasyce1other".into());
        let path = dir.path().join("device.thronglets.json");
        file.save(&path).unwrap();
        let error = ConnectionFile::load(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn expired_connection_file_is_rejected() {
        let dir = TempDir::new().unwrap();
        let node = NodeIdentity::generate();
        let binding = IdentityBinding {
            schema_version: IDENTITY_BINDING_SCHEMA_VERSION.into(),
            owner_account: Some("oasyce1owner".into()),
            device_identity: node.device_identity(),
            binding_source: Some("manual".into()),
            joined_from_device: None,
            updated_at: 123,
        };
        let mut file = ConnectionFile::from_binding(
            &binding,
            &node,
            DEFAULT_CONNECTION_FILE_TTL_HOURS,
            ConnectionSeedScope::Remembered,
            vec![],
        )
        .unwrap();
        file.exported_at = now_ms().saturating_sub(10_000);
        file.expires_at = now_ms().saturating_sub(1_000);
        file.sign_with(&node);
        let path = dir.path().join("expired.connection.json");
        file.save(&path).unwrap();
        let error = ConnectionFile::load(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn ownerless_connection_file_can_be_created() {
        let node = NodeIdentity::generate();
        let binding = IdentityBinding::new(node.device_identity());
        let file = ConnectionFile::from_binding(
            &binding,
            &node,
            DEFAULT_CONNECTION_FILE_TTL_HOURS,
            ConnectionSeedScope::Trusted,
            vec![],
        )
        .unwrap();
        assert_eq!(file.owner_account, None);
        assert_eq!(file.primary_device_identity, node.device_identity());
    }

    #[test]
    fn manual_owner_bind_preserves_connection_origin() {
        let node = NodeIdentity::generate();
        let binding = IdentityBinding::new(node.device_identity())
            .joined_via_connection(None, "oasyce1primary".into())
            .unwrap()
            .bind_owner_account("oasyce1owner".into())
            .unwrap();
        assert_eq!(binding.owner_account.as_deref(), Some("oasyce1owner"));
        assert_eq!(binding.binding_source.as_deref(), Some("connection_file"));
        assert_eq!(
            binding.joined_from_device.as_deref(),
            Some("oasyce1primary")
        );
    }

    #[test]
    fn import_owner_account_hint_sets_owner_and_source() {
        let node = NodeIdentity::generate();
        let binding = IdentityBinding::new(node.device_identity())
            .import_owner_account_hint("oasyce1owner".into())
            .unwrap();
        assert_eq!(binding.owner_account.as_deref(), Some("oasyce1owner"));
        assert_eq!(binding.binding_source.as_deref(), Some("oasyce_sdk"));
    }

    #[test]
    fn load_or_create_imports_oasyce_owner_hint_when_local_binding_is_unbound() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let oasyce_dir = home.join(".oasyce");
        fs::create_dir_all(&oasyce_dir).unwrap();
        fs::write(
            oasyce_dir.join("identity.v1.json"),
            serde_json::to_vec_pretty(&OasyceLocalBinding {
                schema_version: OASYCE_LOCAL_BINDING_SCHEMA_VERSION.into(),
                principal: None,
                account: Some("oasyce1owner".into()),
                delegate: Some("oasyce1sdkdelegate".into()),
                signer_address: Some("oasyce1sdkdelegate".into()),
                updated_at: Some("2026-04-04T00:00:00Z".into()),
            })
            .unwrap(),
        )
        .unwrap();

        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }

        let data_dir = temp.path().join("data");
        let node = NodeIdentity::generate();
        let path = identity_binding_path(&data_dir);
        let binding = IdentityBinding::load_or_create(&path, &node).unwrap();

        match original_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }

        assert_eq!(binding.owner_account.as_deref(), Some("oasyce1owner"));
        assert_eq!(binding.binding_source.as_deref(), Some("oasyce_sdk"));
        assert_eq!(binding.device_identity, node.device_identity());
    }
}
