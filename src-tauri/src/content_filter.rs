//! Session-only access control for mature catalog content.
//!
//! This is deliberately a local family-PC filter, not encryption: the model
//! files still exist on disk. The important boundary here is that neither a
//! credential verifier nor an `include_nsfw` switch is handed to the webview.

use crate::error::AppError;
use once_cell::sync::Lazy;
use pbkdf2::pbkdf2_hmac;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use specta::Type;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Wry};
use tauri_plugin_store::{Store, StoreExt as _};
use uuid::Uuid;

const STORE_PATH: &str = "settings.json";
const PIN_KEY: &str = "nsfw_pin_credential";
const RECOVERY_KEY: &str = "nsfw_recovery_credential";
const PBKDF2_ROUNDS: u32 = 210_000;
const MAX_FAILURES: u8 = 5;
const LOCKOUT: Duration = Duration::from_secs(30);

#[derive(Serialize, Type)]
pub struct NsfwAccessState {
    pub unlocked: bool,
    pub pin_configured: bool,
    pub recovery_configured: bool,
}

#[derive(Serialize, Type)]
pub struct NsfwPinSetup {
    pub state: NsfwAccessState,
    /// Shown once. Only a verifier is retained locally.
    pub recovery_code: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct CredentialRecord {
    salt_hex: String,
    digest_hex: String,
}

struct SessionState {
    unlocked: bool,
    failed_attempts: u8,
    blocked_until: Option<Instant>,
}

static SESSION: Lazy<Mutex<SessionState>> = Lazy::new(|| {
    Mutex::new(SessionState {
        unlocked: false,
        failed_attempts: 0,
        blocked_until: None,
    })
});

pub fn is_unlocked() -> bool {
    SESSION.lock().map(|state| state.unlocked).unwrap_or(false)
}

fn set_unlocked(unlocked: bool) {
    if let Ok(mut state) = SESSION.lock() {
        state.unlocked = unlocked;
        if unlocked {
            state.failed_attempts = 0;
            state.blocked_until = None;
        }
    }
}

fn check_rate_limit() -> Result<(), AppError> {
    let mut state = SESSION
        .lock()
        .map_err(|_| AppError::ConfigError("Content-filter state is unavailable".into()))?;
    if let Some(until) = state.blocked_until {
        if Instant::now() < until {
            return Err(AppError::InvalidInput(
                "Too many attempts; try again in 30 seconds".into(),
            ));
        }
        state.blocked_until = None;
        state.failed_attempts = 0;
    }
    Ok(())
}

fn record_failed_attempt() {
    if let Ok(mut state) = SESSION.lock() {
        state.failed_attempts = state.failed_attempts.saturating_add(1);
        if state.failed_attempts >= MAX_FAILURES {
            state.blocked_until = Some(Instant::now() + LOCKOUT);
        }
    }
}

async fn store(app: &AppHandle) -> Result<std::sync::Arc<Store<Wry>>, AppError> {
    if let Some(store) = app.get_store(STORE_PATH) {
        Ok(store)
    } else {
        app.store(STORE_PATH)
            .map_err(|e| AppError::ConfigError(e.to_string()))
    }
}

fn read_record(store: &Store<Wry>, key: &str) -> Result<Option<CredentialRecord>, AppError> {
    store
        .get(key)
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| AppError::ConfigError(format!("Invalid content-filter credential: {e}")))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect()
}

fn make_record(secret: &str) -> CredentialRecord {
    let salt = Uuid::new_v4().into_bytes();
    let mut digest = [0_u8; 32];
    pbkdf2_hmac::<Sha256>(secret.as_bytes(), &salt, PBKDF2_ROUNDS, &mut digest);
    CredentialRecord {
        salt_hex: hex(&salt),
        digest_hex: hex(&digest),
    }
}

fn verify(secret: &str, record: &CredentialRecord) -> bool {
    let Some(salt) = decode_hex(&record.salt_hex) else {
        return false;
    };
    let Some(expected) = decode_hex(&record.digest_hex) else {
        return false;
    };
    let mut actual = [0_u8; 32];
    pbkdf2_hmac::<Sha256>(secret.as_bytes(), &salt, PBKDF2_ROUNDS, &mut actual);
    constant_time_eq(&actual, &expected)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn validate_pin(pin: &str) -> Result<(), AppError> {
    if !(4..=12).contains(&pin.len()) || !pin.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AppError::InvalidInput(
            "PIN must contain 4–12 digits".into(),
        ));
    }
    Ok(())
}

fn new_recovery_code() -> String {
    let raw = Uuid::new_v4().simple().to_string().to_uppercase();
    raw.as_bytes()
        .chunks(8)
        .map(|part| std::str::from_utf8(part).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

fn normalize_recovery_code(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .flat_map(char::to_uppercase)
        .collect()
}

async fn state_from_store(store: &Store<Wry>) -> Result<NsfwAccessState, AppError> {
    Ok(NsfwAccessState {
        unlocked: is_unlocked(),
        pin_configured: read_record(store, PIN_KEY)?.is_some(),
        recovery_configured: read_record(store, RECOVERY_KEY)?.is_some(),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn get_nsfw_access_state(app_handle: AppHandle) -> Result<NsfwAccessState, AppError> {
    let store = store(&app_handle).await?;
    state_from_store(&store).await
}

#[tauri::command]
#[specta::specta]
pub async fn unlock_nsfw(
    app_handle: AppHandle,
    pin: Option<String>,
) -> Result<NsfwAccessState, AppError> {
    check_rate_limit()?;
    let store = store(&app_handle).await?;
    if let Some(record) = read_record(&store, PIN_KEY)? {
        let valid = pin.as_deref().is_some_and(|pin| verify(pin, &record));
        if !valid {
            record_failed_attempt();
            return Err(AppError::InvalidInput("Wrong PIN".into()));
        }
    }
    set_unlocked(true);
    state_from_store(&store).await
}

#[tauri::command]
#[specta::specta]
pub async fn lock_nsfw(app_handle: AppHandle) -> Result<NsfwAccessState, AppError> {
    set_unlocked(false);
    let store = store(&app_handle).await?;
    state_from_store(&store).await
}

#[tauri::command]
#[specta::specta]
pub async fn configure_nsfw_pin(
    app_handle: AppHandle,
    pin: String,
) -> Result<NsfwPinSetup, AppError> {
    validate_pin(&pin)?;
    let store = store(&app_handle).await?;
    if read_record(&store, PIN_KEY)?.is_some() {
        return Err(AppError::InvalidInput("A PIN is already configured".into()));
    }
    let recovery_code = new_recovery_code();
    store.set(PIN_KEY, serde_json::to_value(make_record(&pin))?);
    store.set(
        RECOVERY_KEY,
        serde_json::to_value(make_record(&normalize_recovery_code(&recovery_code)))?,
    );
    store.delete("nsfw_pin_hash");
    store.delete("show_nsfw");
    store
        .save()
        .map_err(|e| AppError::ConfigError(e.to_string()))?;
    Ok(NsfwPinSetup {
        state: state_from_store(&store).await?,
        recovery_code,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn change_nsfw_pin(
    app_handle: AppHandle,
    current_pin: String,
    new_pin: String,
) -> Result<NsfwAccessState, AppError> {
    validate_pin(&new_pin)?;
    check_rate_limit()?;
    let store = store(&app_handle).await?;
    let current = read_record(&store, PIN_KEY)?
        .ok_or_else(|| AppError::InvalidInput("No PIN is configured".into()))?;
    if !verify(&current_pin, &current) {
        record_failed_attempt();
        return Err(AppError::InvalidInput("Wrong PIN".into()));
    }
    store.set(PIN_KEY, serde_json::to_value(make_record(&new_pin))?);
    store
        .save()
        .map_err(|e| AppError::ConfigError(e.to_string()))?;
    set_unlocked(true);
    state_from_store(&store).await
}

#[tauri::command]
#[specta::specta]
pub async fn recover_nsfw_pin(
    app_handle: AppHandle,
    recovery_code: String,
    new_pin: String,
) -> Result<NsfwPinSetup, AppError> {
    validate_pin(&new_pin)?;
    check_rate_limit()?;
    let store = store(&app_handle).await?;
    let recovery = read_record(&store, RECOVERY_KEY)?
        .ok_or_else(|| AppError::InvalidInput("No recovery code is configured".into()))?;
    let normalized = normalize_recovery_code(&recovery_code);
    if !verify(&normalized, &recovery) {
        record_failed_attempt();
        return Err(AppError::InvalidInput("Wrong recovery code".into()));
    }
    let next_recovery_code = new_recovery_code();
    store.set(PIN_KEY, serde_json::to_value(make_record(&new_pin))?);
    store.set(
        RECOVERY_KEY,
        serde_json::to_value(make_record(&normalize_recovery_code(&next_recovery_code)))?,
    );
    store
        .save()
        .map_err(|e| AppError::ConfigError(e.to_string()))?;
    set_unlocked(true);
    Ok(NsfwPinSetup {
        state: state_from_store(&store).await?,
        recovery_code: next_recovery_code,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn remove_nsfw_pin(
    app_handle: AppHandle,
    current_pin: String,
) -> Result<NsfwAccessState, AppError> {
    check_rate_limit()?;
    let store = store(&app_handle).await?;
    let current = read_record(&store, PIN_KEY)?
        .ok_or_else(|| AppError::InvalidInput("No PIN is configured".into()))?;
    if !verify(&current_pin, &current) {
        record_failed_attempt();
        return Err(AppError::InvalidInput("Wrong PIN".into()));
    }
    store.delete(PIN_KEY);
    store.delete(RECOVERY_KEY);
    store
        .save()
        .map_err(|e| AppError::ConfigError(e.to_string()))?;
    set_unlocked(false);
    state_from_store(&store).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_record_verifies_only_the_original_secret() {
        let record = make_record("4826");
        assert!(verify("4826", &record));
        assert!(!verify("4827", &record));
    }

    #[test]
    fn recovery_code_is_external_friendly_and_normalized() {
        let code = new_recovery_code();
        assert_eq!(code.len(), 35);
        assert_eq!(normalize_recovery_code(&code).len(), 32);
        assert_eq!(normalize_recovery_code("aa bb-12"), "AABB12");
    }

    #[test]
    fn pin_validation_rejects_short_or_non_numeric_values() {
        assert!(validate_pin("1234").is_ok());
        assert!(validate_pin("123").is_err());
        assert!(validate_pin("123x").is_err());
    }
}
