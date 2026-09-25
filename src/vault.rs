use std::collections::HashSet;
use std::fmt;

use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

/// Where a session secret comes from. The session call accepts every source.
/// Version one materializes [`LocalBackend`] only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretSource {
    Local,
    Url,
    Mcp,
    Chain,
    Ipfs,
}

impl SecretSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Url => "url",
            Self::Mcp => "mcp",
            Self::Chain => "chain",
            Self::Ipfs => "ipfs",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum VaultError {
    NotImplemented(SecretSource),
    MissingValue { name: String },
    MissingLocator { name: String, source: SecretSource },
    EmptyName,
    DuplicateName(String),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented(source) => {
                write!(
                    f,
                    "secret source \"{}\" is not implemented",
                    source.as_str()
                )
            }
            Self::MissingValue { name } => {
                write!(f, "local secret \"{name}\" requires a value")
            }
            Self::MissingLocator { name, source } => {
                write!(
                    f,
                    "secret \"{name}\" requires a locator for source \"{}\"",
                    source.as_str()
                )
            }
            Self::EmptyName => write!(f, "secret name is required"),
            Self::DuplicateName(name) => write!(f, "duplicate secret \"{name}\""),
        }
    }
}

impl std::error::Error for VaultError {}

/// One secret entry on `POST /session`.
pub struct SecretRequest {
    pub name: String,
    pub source: SecretSource,
    pub value: Option<Zeroizing<String>>,
    pub locator: Option<Zeroizing<String>>,
}

#[derive(Deserialize)]
struct RawSecret {
    name: String,
    source: SecretSource,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    locator: Option<String>,
}

impl<'de> Deserialize<'de> for SecretRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawSecret::deserialize(deserializer)?;
        Ok(Self {
            name: raw.name,
            source: raw.source,
            value: raw.value.map(Zeroizing::new),
            locator: raw.locator.map(Zeroizing::new),
        })
    }
}

pub trait SecretSourceBackend {
    fn source(&self) -> SecretSource;
    fn materialize(&self, request: &SecretRequest) -> Result<Zeroizing<Vec<u8>>, VaultError>;
}

pub struct LocalBackend;

impl SecretSourceBackend for LocalBackend {
    fn source(&self) -> SecretSource {
        SecretSource::Local
    }

    fn materialize(&self, request: &SecretRequest) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        let Some(value) = request.value.as_ref() else {
            return Err(VaultError::MissingValue {
                name: request.name.clone(),
            });
        };
        if value.is_empty() {
            return Err(VaultError::MissingValue {
                name: request.name.clone(),
            });
        }
        Ok(Zeroizing::new(value.as_bytes().to_vec()))
    }
}

pub struct UrlBackend;
pub struct McpBackend;
pub struct ChainBackend;
pub struct IpfsBackend;

macro_rules! unimplemented_backend {
    ($ty:ident, $source:ident) => {
        impl SecretSourceBackend for $ty {
            fn source(&self) -> SecretSource {
                SecretSource::$source
            }

            fn materialize(
                &self,
                request: &SecretRequest,
            ) -> Result<Zeroizing<Vec<u8>>, VaultError> {
                let locator = request
                    .locator
                    .as_ref()
                    .map(|value| value.as_str())
                    .unwrap_or("");
                if locator.is_empty() {
                    return Err(VaultError::MissingLocator {
                        name: request.name.clone(),
                        source: SecretSource::$source,
                    });
                }
                let _ = locator;
                Err(VaultError::NotImplemented(SecretSource::$source))
            }
        }
    };
}

unimplemented_backend!(UrlBackend, Url);
unimplemented_backend!(McpBackend, Mcp);
unimplemented_backend!(ChainBackend, Chain);
unimplemented_backend!(IpfsBackend, Ipfs);

struct StoredSecret {
    name: String,
    value: Zeroizing<Vec<u8>>,
}

/// Secrets for one session. Dropping the vault clears them.
pub struct Vault {
    secrets: Vec<StoredSecret>,
}

impl Vault {
    pub fn open(requests: &[SecretRequest]) -> Result<Self, VaultError> {
        let mut seen = HashSet::new();
        let mut secrets = Vec::with_capacity(requests.len());
        for request in requests {
            if request.name.trim().is_empty() {
                return Err(VaultError::EmptyName);
            }
            if !seen.insert(request.name.clone()) {
                return Err(VaultError::DuplicateName(request.name.clone()));
            }
            let value = materialize(request)?;
            secrets.push(StoredSecret {
                name: request.name.clone(),
                value,
            });
        }
        Ok(Self { secrets })
    }

    pub fn get(&self, name: &str) -> Option<Zeroizing<Vec<u8>>> {
        self.secrets
            .iter()
            .find(|secret| secret.name == name)
            .map(|secret| Zeroizing::new(secret.value.to_vec()))
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        for secret in &mut self.secrets {
            secret.value.zeroize();
            secret.name.zeroize();
        }
        self.secrets.clear();
    }
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self
            .secrets
            .iter()
            .map(|secret| secret.name.as_str())
            .collect();
        f.debug_struct("Vault").field("secrets", &names).finish()
    }
}

fn materialize(request: &SecretRequest) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    let backend = backend(request.source);
    if backend.source() != request.source {
        return Err(VaultError::NotImplemented(request.source));
    }
    backend.materialize(request)
}

fn backend(source: SecretSource) -> &'static dyn SecretSourceBackend {
    match source {
        SecretSource::Local => &LocalBackend,
        SecretSource::Url => &UrlBackend,
        SecretSource::Mcp => &McpBackend,
        SecretSource::Chain => &ChainBackend,
        SecretSource::Ipfs => &IpfsBackend,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(name: &str, value: &str) -> SecretRequest {
        SecretRequest {
            name: name.into(),
            source: SecretSource::Local,
            value: Some(Zeroizing::new(value.into())),
            locator: None,
        }
    }

    fn remote(source: SecretSource, locator: Option<&str>) -> SecretRequest {
        SecretRequest {
            name: "token".into(),
            source,
            value: None,
            locator: locator.map(|value| Zeroizing::new(value.into())),
        }
    }

    #[test]
    fn local_secret_round_trips_and_debug_hides_it() {
        let vault = Vault::open(&[local("password", "hunter2")]).unwrap();
        assert_eq!(vault.get("password").unwrap().as_slice(), b"hunter2");
        let rendered = format!("{vault:?}");
        assert!(rendered.contains("password"));
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn backends_name_their_source() {
        assert_eq!(LocalBackend.source(), SecretSource::Local);
        assert_eq!(UrlBackend.source(), SecretSource::Url);
        assert_eq!(McpBackend.source(), SecretSource::Mcp);
        assert_eq!(ChainBackend.source(), SecretSource::Chain);
        assert_eq!(IpfsBackend.source(), SecretSource::Ipfs);
    }

    #[test]
    fn later_sources_are_not_fetched() {
        let locator = "https://user:password@example.invalid/secret";
        for source in [
            SecretSource::Url,
            SecretSource::Mcp,
            SecretSource::Chain,
            SecretSource::Ipfs,
        ] {
            let err = Vault::open(&[remote(source, Some(locator))]).unwrap_err();
            assert_eq!(err, VaultError::NotImplemented(source));
            let text = err.to_string();
            assert!(!text.contains("password"));
            assert!(!text.contains("example.invalid"));
            assert!(text.contains(source.as_str()));
        }
    }

    #[test]
    fn remote_source_still_requires_a_locator() {
        let err = Vault::open(&[remote(SecretSource::Url, None)]).unwrap_err();
        assert!(matches!(err, VaultError::MissingLocator { .. }));
    }

    #[test]
    fn duplicate_and_empty_names_fail() {
        assert!(matches!(
            Vault::open(&[local("  ", "x")]),
            Err(VaultError::EmptyName)
        ));
        assert!(matches!(
            Vault::open(&[local("a", "x"), local("a", "y")]),
            Err(VaultError::DuplicateName(_))
        ));
        assert!(matches!(
            Vault::open(&[local("a", "")]),
            Err(VaultError::MissingValue { .. })
        ));
    }
}
