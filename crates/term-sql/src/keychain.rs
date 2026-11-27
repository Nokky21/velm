use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

pub const KEYCHAIN_SERVICE: &str = "term-sql";

/// Connection string wrapper whose `Debug` implementation never reveals secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretUrl(String);

impl SecretUrl {
    pub fn new(url: impl Into<String>) -> Self {
        Self(url.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretUrl([REDACTED])")
    }
}

impl Drop for SecretUrl {
    fn drop(&mut self) {
        // Clear contents on drop to minimize secret lifetime in memory.
        self.0.clear();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeychainError {
    NotFound { name: String },
    UnsupportedPlatform,
    Backend { message: String },
}

impl fmt::Display for KeychainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { name } => {
                write!(f, "no encuentro la conexión '{name}' en el Llavero")
            }
            Self::UnsupportedPlatform => {
                write!(f, "el Llavero nativo solo está disponible en macOS")
            }
            Self::Backend { message } => {
                write!(f, "error del Llavero: {message}")
            }
        }
    }
}

impl std::error::Error for KeychainError {}

/// Storage abstraction over the macOS Keychain (Llavero) and in-memory test backends.
pub trait KeychainStore: Send + Sync {
    fn get(&self, name: &str) -> Result<SecretUrl, KeychainError>;
    fn set(&self, name: &str, url: &str) -> Result<(), KeychainError>;
    fn delete(&self, name: &str) -> Result<(), KeychainError>;
}

/// Production keychain store: delegates to macOS Keychain (`security-framework`),
/// or returns `UnsupportedPlatform` on non-macOS targets.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemKeychain;

impl KeychainStore for SystemKeychain {
    fn get(&self, name: &str) -> Result<SecretUrl, KeychainError> {
        #[cfg(target_os = "macos")]
        {
            macos::get(name)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = name;
            Err(KeychainError::UnsupportedPlatform)
        }
    }

    fn set(&self, name: &str, url: &str) -> Result<(), KeychainError> {
        #[cfg(target_os = "macos")]
        {
            macos::set(name, url)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (name, url);
            Err(KeychainError::UnsupportedPlatform)
        }
    }

    fn delete(&self, name: &str) -> Result<(), KeychainError> {
        #[cfg(target_os = "macos")]
        {
            macos::delete(name)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = name;
            Err(KeychainError::UnsupportedPlatform)
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{KeychainError, SecretUrl, KEYCHAIN_SERVICE};
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    pub fn get(name: &str) -> Result<SecretUrl, KeychainError> {
        match get_generic_password(KEYCHAIN_SERVICE, name) {
            Ok(bytes) => {
                let raw = String::from_utf8(bytes).map_err(|_| KeychainError::Backend {
                    message: String::from("la entrada del Llavero no es UTF-8 válido"),
                })?;
                Ok(SecretUrl::new(raw))
            }
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Err(KeychainError::NotFound {
                name: name.to_owned(),
            }),
            Err(err) => Err(KeychainError::Backend {
                message: format!("código OSStatus {}", err.code()),
            }),
        }
    }

    pub fn set(name: &str, url: &str) -> Result<(), KeychainError> {
        set_generic_password(KEYCHAIN_SERVICE, name, url.as_bytes()).map_err(|err| {
            KeychainError::Backend {
                message: format!("código OSStatus {}", err.code()),
            }
        })
    }

    pub fn delete(name: &str) -> Result<(), KeychainError> {
        match delete_generic_password(KEYCHAIN_SERVICE, name) {
            Ok(()) => Ok(()),
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Err(KeychainError::NotFound {
                name: name.to_owned(),
            }),
            Err(err) => Err(KeychainError::Backend {
                message: format!("código OSStatus {}", err.code()),
            }),
        }
    }
}

/// Thread-safe in-memory keychain backend for unit and integration tests.
#[derive(Debug, Default)]
pub struct MemoryKeychain {
    entries: Mutex<HashMap<String, String>>,
}

impl MemoryKeychain {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeychainStore for MemoryKeychain {
    fn get(&self, name: &str) -> Result<SecretUrl, KeychainError> {
        let guard = self.entries.lock().map_err(|_| KeychainError::Backend {
            message: String::from("bloqueo interno corrupto"),
        })?;
        guard
            .get(name)
            .map(|v| SecretUrl::new(v.clone()))
            .ok_or_else(|| KeychainError::NotFound {
                name: name.to_owned(),
            })
    }

    fn set(&self, name: &str, url: &str) -> Result<(), KeychainError> {
        let mut guard = self.entries.lock().map_err(|_| KeychainError::Backend {
            message: String::from("bloqueo interno corrupto"),
        })?;
        guard.insert(name.to_owned(), url.to_owned());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), KeychainError> {
        let mut guard = self.entries.lock().map_err(|_| KeychainError::Backend {
            message: String::from("bloqueo interno corrupto"),
        })?;
        guard
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| KeychainError::NotFound {
                name: name.to_owned(),
            })
    }
}

/// Validates that a Postgres connection string starts with `postgres://` or `postgresql://`
/// without echoing the secret back in the error.
pub fn validate_postgres_url_prefix(raw: &str) -> Result<(), &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("la cadena de conexión no puede estar vacía");
    }
    if !(trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://")) {
        return Err("la cadena de conexión debe empezar por postgres:// o postgresql://");
    }
    if trimmed.chars().any(char::is_control) {
        return Err("la cadena de conexión contiene caracteres de control no válidos");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_url_debug_redacts_credentials() {
        let secret = SecretUrl::new("postgres://user:supersecret@localhost:5432/app");
        let debug = format!("{secret:?}");
        assert!(!debug.contains("supersecret"));
        assert!(!debug.contains("user"));
        assert!(!debug.contains("localhost"));
        assert_eq!(debug, "SecretUrl([REDACTED])");
    }

    #[test]
    fn memory_keychain_roundtrip_and_missing_message() {
        let kc = MemoryKeychain::new();
        let err = kc.get("local-card").unwrap_err();
        assert_eq!(
            err.to_string(),
            "no encuentro la conexión 'local-card' en el Llavero"
        );

        kc.set("local-card", "postgres://postgres@127.0.0.1:5432/card")
            .unwrap();
        assert_eq!(
            kc.get("local-card").unwrap().as_str(),
            "postgres://postgres@127.0.0.1:5432/card"
        );

        kc.delete("local-card").unwrap();
        assert!(matches!(
            kc.get("local-card"),
            Err(KeychainError::NotFound { .. })
        ));
    }

    #[test]
    fn url_prefix_validation() {
        assert!(validate_postgres_url_prefix("postgres://user@localhost/db").is_ok());
        assert!(validate_postgres_url_prefix("postgresql://user@localhost/db").is_ok());
        assert!(validate_postgres_url_prefix("mysql://user@localhost/db").is_err());
        assert!(validate_postgres_url_prefix("").is_err());
    }
}
