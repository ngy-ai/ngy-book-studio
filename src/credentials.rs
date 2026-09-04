//! Secret storage boundary. API keys are never persisted in SQLite or JSON.

use std::{collections::HashMap, sync::Mutex};

use anyhow::{Context as _, Result, ensure};

pub trait CredentialStore: Send + Sync {
    fn set_api_key(&self, endpoint: &str, api_key: &str) -> Result<()>;
    fn api_key(&self, endpoint: &str) -> Result<Option<String>>;
    fn delete_api_key(&self, endpoint: &str) -> Result<()>;
}

const SERVICE_NAME: &str = "dev.moye.epub-reader.openai-compatible";

fn account_name(endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim();
    ensure!(!endpoint.is_empty(), "endpoint is required");
    Ok(format!(
        "endpoint-{}",
        blake3::hash(endpoint.as_bytes()).to_hex()
    ))
}

/// Windows Credential Manager implementation used by the application.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCredentialStore;

#[cfg(target_os = "windows")]
impl CredentialStore for SystemCredentialStore {
    fn set_api_key(&self, endpoint: &str, api_key: &str) -> Result<()> {
        ensure!(!api_key.is_empty(), "API key must not be empty");
        keyring::Entry::new(SERVICE_NAME, &account_name(endpoint)?)
            .context("failed to open Windows Credential Manager")?
            .set_password(api_key)
            .context("failed to store API key in Windows Credential Manager")
    }

    fn api_key(&self, endpoint: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(SERVICE_NAME, &account_name(endpoint)?)
            .context("failed to open Windows Credential Manager")?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => {
                Err(error).context("failed to read API key from Windows Credential Manager")
            }
        }
    }

    fn delete_api_key(&self, endpoint: &str) -> Result<()> {
        let entry = keyring::Entry::new(SERVICE_NAME, &account_name(endpoint)?)
            .context("failed to open Windows Credential Manager")?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => {
                Err(error).context("failed to delete API key from Windows Credential Manager")
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl CredentialStore for SystemCredentialStore {
    fn set_api_key(&self, _: &str, _: &str) -> Result<()> {
        anyhow::bail!("this build only enables the Windows Credential Manager backend")
    }

    fn api_key(&self, _: &str) -> Result<Option<String>> {
        anyhow::bail!("this build only enables the Windows Credential Manager backend")
    }

    fn delete_api_key(&self, _: &str) -> Result<()> {
        anyhow::bail!("this build only enables the Windows Credential Manager backend")
    }
}

/// Test/dependency-injection implementation. It must not be used as a silent
/// production fallback because that would leave secrets in process-managed
/// persistence rather than the operating-system credential store.
#[derive(Debug, Default)]
pub struct MemoryCredentialStore {
    values: Mutex<HashMap<String, String>>,
}

impl CredentialStore for MemoryCredentialStore {
    fn set_api_key(&self, endpoint: &str, api_key: &str) -> Result<()> {
        ensure!(!api_key.is_empty(), "API key must not be empty");
        self.values
            .lock()
            .expect("credential test mutex poisoned")
            .insert(account_name(endpoint)?, api_key.to_string());
        Ok(())
    }

    fn api_key(&self, endpoint: &str) -> Result<Option<String>> {
        Ok(self
            .values
            .lock()
            .expect("credential test mutex poisoned")
            .get(&account_name(endpoint)?)
            .cloned())
    }

    fn delete_api_key(&self, endpoint: &str) -> Result<()> {
        self.values
            .lock()
            .expect("credential test mutex poisoned")
            .remove(&account_name(endpoint)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_is_scoped_by_endpoint_and_deletes_idempotently() {
        let store = MemoryCredentialStore::default();
        store
            .set_api_key("https://one.example/v1", "secret-1")
            .unwrap();
        store
            .set_api_key("https://two.example/v1", "secret-2")
            .unwrap();
        assert_eq!(
            store.api_key("https://one.example/v1").unwrap().as_deref(),
            Some("secret-1")
        );
        store.delete_api_key("https://one.example/v1").unwrap();
        store.delete_api_key("https://one.example/v1").unwrap();
        assert_eq!(store.api_key("https://one.example/v1").unwrap(), None);
        assert_eq!(
            store.api_key("https://two.example/v1").unwrap().as_deref(),
            Some("secret-2")
        );
    }

    #[test]
    fn account_names_do_not_expose_endpoint_text() {
        let account = account_name("https://private.example/v1").unwrap();
        assert!(!account.contains("private.example"));
        assert!(account.starts_with("endpoint-"));
    }
}
