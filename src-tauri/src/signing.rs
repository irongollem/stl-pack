//! Creator identity for signed `.3pk` manifests (docs/3PK.md `manifest.sig`,
//! issue #6). v1 is local-only: one Ed25519 keypair per install, generated
//! on demand from Settings and reused silently by the release builder — no
//! cloud, no registry, no key revocation (all tracked as #8 follow-ups).
//! OS keychain storage is a later hardening step; the app data dir matches
//! the current threat model (same disk the unpacked STLs already live on).

use crate::error::AppError;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const KEY_FILE_NAME: &str = "signing_key.json";

/// Detached signature written as `manifest.sig`, next to `manifest.json`
/// inside `release.3pk`. Additive to the format: absent reads as Unsigned.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ManifestSignature {
    pub algo: String,
    pub public_key: String,
    pub key_fingerprint: String,
    pub signature: String,
}

/// What Settings shows once a key exists — enough to publish the
/// fingerprint elsewhere; never the private key.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct SigningKeyInfo {
    pub public_key: String,
    pub key_fingerprint: String,
}

/// How a `manifest.sig` (or its absence) checks out against the exact
/// manifest.json bytes read from the same archive.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Type)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SignatureStatus {
    /// No manifest.sig entry — a normal, unsigned pack.
    Unsigned,
    Valid { key_fingerprint: String },
    /// Present but wrong: tampered manifest, wrong key, or malformed sig.
    /// One loud bucket; `reason` says which.
    Invalid { reason: String },
}

#[derive(Serialize, Deserialize)]
struct StoredKey {
    /// base64 of the 32-byte Ed25519 seed — the only secret on disk.
    seed: String,
}

pub fn key_path(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    Ok(app_handle.path().app_data_dir()?.join(KEY_FILE_NAME))
}

/// blake3 of the raw public key bytes, first 16 hex chars. Grouping this
/// into fours for display is a UI concern, not part of the value itself.
fn fingerprint(public_key: &[u8]) -> String {
    blake3::hash(public_key).to_hex()[..16].to_string()
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}
#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) {}

fn generate_and_store(path: &Path) -> Result<SigningKey, AppError> {
    let key = SigningKey::generate(&mut OsRng);
    let stored = StoredKey {
        seed: STANDARD.encode(key.to_bytes()),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::IoError(format!("Failed to create app data dir: {}", e)))?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&stored)?)
        .map_err(|e| AppError::IoError(format!("Failed to write signing key: {}", e)))?;
    restrict_to_owner(path);
    Ok(key)
}

/// Loads the key at `path` when present. None means "no key yet" — the
/// pack path treats that as silently unsigned, never an error.
pub fn load_key(path: &Path) -> Result<Option<SigningKey>, AppError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::IoError(format!("Failed to read signing key: {}", e))),
    };
    let stored: StoredKey = serde_json::from_str(&text)
        .map_err(|e| AppError::ConfigError(format!("Corrupt signing key: {}", e)))?;
    let seed_bytes = STANDARD
        .decode(&stored.seed)
        .map_err(|e| AppError::ConfigError(format!("Corrupt signing key: {}", e)))?;
    let seed: [u8; 32] = seed_bytes
        .try_into()
        .map_err(|_| AppError::ConfigError("Corrupt signing key: wrong seed length".into()))?;
    Ok(Some(SigningKey::from_bytes(&seed)))
}

/// Load the existing key, or generate and persist one — idempotent, so a
/// second call returns the same fingerprint as the first.
pub fn ensure_key(path: &Path) -> Result<SigningKey, AppError> {
    match load_key(path)? {
        Some(key) => Ok(key),
        None => generate_and_store(path),
    }
}

pub fn key_info(key: &SigningKey) -> SigningKeyInfo {
    let verifying = key.verifying_key();
    SigningKeyInfo {
        public_key: STANDARD.encode(verifying.as_bytes()),
        key_fingerprint: fingerprint(verifying.as_bytes()),
    }
}

/// Sign the exact bytes about to be zipped as manifest.json — never a
/// re-serialized copy, so whatever this signs is byte-for-byte what
/// `classify_signature` later verifies.
pub fn sign_manifest(key: &SigningKey, manifest_bytes: &[u8]) -> ManifestSignature {
    let signature = key.sign(manifest_bytes);
    let info = key_info(key);
    ManifestSignature {
        algo: "ed25519".into(),
        public_key: info.public_key,
        key_fingerprint: info.key_fingerprint,
        signature: STANDARD.encode(signature.to_bytes()),
    }
}

/// Classifies a `manifest.sig` payload (its raw JSON text, however it was
/// read) against the exact manifest.json bytes it claims to cover. Never
/// panics on attacker-authored input: bad JSON, bad base64, a wrong-length
/// key or signature, and a genuine mismatch all land as Invalid with a
/// reason — callers report Unsigned themselves when there's no manifest.sig
/// entry at all.
pub fn classify_signature(sig_json: &str, manifest_bytes: &[u8]) -> SignatureStatus {
    let sig: ManifestSignature = match serde_json::from_str(sig_json) {
        Ok(s) => s,
        Err(_) => {
            return SignatureStatus::Invalid {
                reason: "manifest.sig is not valid JSON".into(),
            }
        }
    };
    if sig.algo != "ed25519" {
        return SignatureStatus::Invalid {
            reason: format!("unsupported signature algorithm '{}'", sig.algo),
        };
    }
    if verify_manifest(manifest_bytes, &sig) {
        SignatureStatus::Valid {
            key_fingerprint: sig.key_fingerprint,
        }
    } else {
        SignatureStatus::Invalid {
            reason: "signature does not match the manifest — it may be tampered, or signed by a different key".into(),
        }
    }
}

fn verify_manifest(manifest_bytes: &[u8], sig: &ManifestSignature) -> bool {
    let Ok(key_bytes) = STANDARD.decode(&sig.public_key) else {
        return false;
    };
    let Ok(key_bytes): Result<[u8; 32], _> = key_bytes.try_into() else {
        return false;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(&key_bytes) else {
        return false;
    };
    let Ok(sig_bytes) = STANDARD.decode(&sig.signature) else {
        return false;
    };
    let Ok(sig_bytes): Result<[u8; 64], _> = sig_bytes.try_into() else {
        return false;
    };
    let signature = Signature::from_bytes(&sig_bytes);
    verifying.verify(manifest_bytes, &signature).is_ok()
}

/// Get-or-create the creator's signing key and hand back what Settings can
/// display and publish — the "generate on demand" entry point (#6).
#[tauri::command]
#[specta::specta]
pub async fn ensure_signing_key(app_handle: AppHandle) -> Result<SigningKeyInfo, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = key_path(&app_handle)?;
        let key = ensure_key(&path)?;
        Ok(key_info(&key))
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Signing key task failed: {}", e)))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_key_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stlpack_signing_{}_{}_{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(KEY_FILE_NAME)
    }

    #[test]
    fn ensure_key_is_idempotent() {
        let path = temp_key_path("idempotent");
        let first = ensure_key(&path).unwrap();
        let second = ensure_key(&path).unwrap();
        assert_eq!(
            key_info(&first).key_fingerprint,
            key_info(&second).key_fingerprint
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn load_key_reads_none_when_absent() {
        let path = temp_key_path("absent").parent().unwrap().join("nope.json");
        assert!(load_key(&path).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn generated_key_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_key_path("perms");
        ensure_key(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn sign_then_classify_round_trips_as_valid() {
        let path = temp_key_path("roundtrip");
        let key = ensure_key(&path).unwrap();
        let manifest_bytes = b"{\"format\":\"3pk\"}";
        let sig = sign_manifest(&key, manifest_bytes);
        let sig_json = serde_json::to_string(&sig).unwrap();
        let status = classify_signature(&sig_json, manifest_bytes);
        assert_eq!(
            status,
            SignatureStatus::Valid {
                key_fingerprint: key_info(&key).key_fingerprint
            }
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn classify_flags_a_tampered_manifest_as_invalid() {
        let path = temp_key_path("tampered");
        let key = ensure_key(&path).unwrap();
        let sig = sign_manifest(&key, b"original bytes");
        let sig_json = serde_json::to_string(&sig).unwrap();
        let status = classify_signature(&sig_json, b"original byteS");
        assert!(matches!(status, SignatureStatus::Invalid { .. }));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn classify_flags_a_signature_from_a_different_key_as_invalid() {
        let path_a = temp_key_path("key_a");
        let path_b = temp_key_path("key_b");
        let key_a = ensure_key(&path_a).unwrap();
        let key_b = ensure_key(&path_b).unwrap();
        let manifest_bytes = b"same bytes both sides";
        // Sign with A, but wear B's public key — the mismatch itself must
        // fail, not just a bytes mismatch.
        let mut sig = sign_manifest(&key_a, manifest_bytes);
        sig.public_key = key_info(&key_b).public_key;
        sig.key_fingerprint = key_info(&key_b).key_fingerprint;
        let sig_json = serde_json::to_string(&sig).unwrap();
        let status = classify_signature(&sig_json, manifest_bytes);
        assert!(matches!(status, SignatureStatus::Invalid { .. }));
        std::fs::remove_dir_all(path_a.parent().unwrap()).ok();
        std::fs::remove_dir_all(path_b.parent().unwrap()).ok();
    }

    #[test]
    fn classify_flags_malformed_json_as_invalid_not_a_crash() {
        let status = classify_signature("{ not json", b"whatever");
        assert!(matches!(status, SignatureStatus::Invalid { .. }));
    }
}
