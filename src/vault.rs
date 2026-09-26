use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

use crate::mcp;

/// Where a session secret comes from. The session call accepts every source.
/// Version one materializes [`LocalBackend`] and [`McpBackend`].
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
    /// Locator must be `server/tool` for source mcp.
    BadLocator { name: String },
    /// MCP fetch failed. Message never includes secret bytes.
    McpFetch { name: String, detail: String },
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
            Self::BadLocator { name } => {
                write!(
                    f,
                    "secret \"{name}\" mcp locator must be server/tool"
                )
            }
            Self::McpFetch { name, detail } => {
                write!(f, "secret \"{name}\" mcp fetch failed: {detail}")
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
    fn materialize(
        &self,
        request: &SecretRequest,
        ctx: &VaultContext,
    ) -> Result<Zeroizing<Vec<u8>>, VaultError>;
}

/// Context for materializing secrets. `mcp_dir` is only used by source mcp.
#[derive(Clone, Debug)]
pub struct VaultContext {
    pub mcp_dir: PathBuf,
}

impl VaultContext {
    pub fn new(mcp_dir: impl Into<PathBuf>) -> Self {
        Self {
            mcp_dir: mcp_dir.into(),
        }
    }
}

pub struct LocalBackend;

impl SecretSourceBackend for LocalBackend {
    fn source(&self) -> SecretSource {
        SecretSource::Local
    }

    fn materialize(
        &self,
        request: &SecretRequest,
        _ctx: &VaultContext,
    ) -> Result<Zeroizing<Vec<u8>>, VaultError> {
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
                _ctx: &VaultContext,
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
unimplemented_backend!(ChainBackend, Chain);
unimplemented_backend!(IpfsBackend, Ipfs);

impl SecretSourceBackend for McpBackend {
    fn source(&self) -> SecretSource {
        SecretSource::Mcp
    }

    fn materialize(
        &self,
        request: &SecretRequest,
        ctx: &VaultContext,
    ) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        let locator = request
            .locator
            .as_ref()
            .map(|value| value.as_str().trim())
            .unwrap_or("");
        if locator.is_empty() {
            return Err(VaultError::MissingLocator {
                name: request.name.clone(),
                source: SecretSource::Mcp,
            });
        }
        let (server, tool) = parse_mcp_locator(locator).ok_or_else(|| VaultError::BadLocator {
            name: request.name.clone(),
        })?;
        mcp::fetch_secret_bytes(&ctx.mcp_dir, server, tool, &request.name).map_err(|err| {
            VaultError::McpFetch {
                name: request.name.clone(),
                detail: err.to_string(),
            }
        })
    }
}

/// Locator shape for source mcp: `server/tool`.
pub fn parse_mcp_locator(locator: &str) -> Option<(&str, &str)> {
    let locator = locator.trim();
    let (server, tool) = locator.split_once('/')?;
    let server = server.trim();
    let tool = tool.trim();
    if server.is_empty() || tool.is_empty() || tool.contains('/') {
        return None;
    }
    Some((server, tool))
}

struct StoredSecret {
    name: String,
    value: Zeroizing<Vec<u8>>,
}

/// Secrets for one session. Dropping the vault clears them.
pub struct Vault {
    secrets: Vec<StoredSecret>,
    ctx: VaultContext,
}

impl Vault {
    pub fn open(requests: &[SecretRequest], mcp_dir: impl Into<PathBuf>) -> Result<Self, VaultError> {
        let ctx = VaultContext::new(mcp_dir);
        let mut seen = HashSet::new();
        let mut secrets = Vec::with_capacity(requests.len());
        for request in requests {
            if request.name.trim().is_empty() {
                return Err(VaultError::EmptyName);
            }
            if !seen.insert(request.name.clone()) {
                return Err(VaultError::DuplicateName(request.name.clone()));
            }
            let value = materialize(request, &ctx)?;
            secrets.push(StoredSecret {
                name: request.name.clone(),
                value,
            });
        }
        Ok(Self { secrets, ctx })
    }

    pub fn mcp_dir(&self) -> &Path {
        &self.ctx.mcp_dir
    }

    pub fn get(&self, name: &str) -> Option<Zeroizing<Vec<u8>>> {
        self.secrets
            .iter()
            .find(|secret| secret.name == name)
            .map(|secret| Zeroizing::new(secret.value.to_vec()))
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.secrets.iter().any(|secret| secret.name == name)
    }

    /// Adds one secret after the session is already open. The value stays in the vault.
    pub(crate) fn insert(&mut self, request: &SecretRequest) -> Result<String, VaultError> {
        if request.name.trim().is_empty() {
            return Err(VaultError::EmptyName);
        }
        if self.contains(&request.name) {
            return Err(VaultError::DuplicateName(request.name.clone()));
        }
        let value = materialize(request, &self.ctx)?;
        let name = request.name.clone();
        self.secrets.push(StoredSecret {
            name: name.clone(),
            value,
        });
        Ok(name)
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

fn materialize(
    request: &SecretRequest,
    ctx: &VaultContext,
) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    let backend = backend(request.source);
    if backend.source() != request.source {
        return Err(VaultError::NotImplemented(request.source));
    }
    backend.materialize(request, ctx)
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

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

    fn empty_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hm-vault-empty-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn local_secret_round_trips_and_debug_hides_it() {
        let vault = Vault::open(&[local("password", "hunter2")], empty_dir()).unwrap();
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
        for source in [SecretSource::Url, SecretSource::Chain, SecretSource::Ipfs] {
            let err = Vault::open(&[remote(source, Some(locator))], empty_dir()).unwrap_err();
            assert_eq!(err, VaultError::NotImplemented(source));
            let text = err.to_string();
            assert!(!text.contains("password"));
            assert!(!text.contains("example.invalid"));
            assert!(text.contains(source.as_str()));
        }
    }

    #[test]
    fn mcp_locator_must_be_server_slash_tool() {
        assert_eq!(parse_mcp_locator("vault-fixture/get_secret"), Some(("vault-fixture", "get_secret")));
        assert!(parse_mcp_locator("noslash").is_none());
        assert!(parse_mcp_locator("/tool").is_none());
        assert!(parse_mcp_locator("server/").is_none());
        let err = Vault::open(
            &[remote(SecretSource::Mcp, Some("not-a-locator"))],
            empty_dir(),
        )
        .unwrap_err();
        assert!(matches!(err, VaultError::BadLocator { .. }));
    }

    #[test]
    fn mcp_source_fetches_via_short_lived_stdio() {
        let dir = std::env::temp_dir().join(format!("hm-vault-mcp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("secret-mcp.py");
        fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json, sys
def recv():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    return json.loads(line)
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
while True:
    msg = recv()
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"secrets","version":"0"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"get_secret","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"content":[{"type":"text","text":"from-mcp"}]}})
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
        fs::write(
            dir.join("mcp.json"),
            format!(
                r#"{{"mcpServers":{{"vault-fixture":{{"command":"{}","type":"stdio"}}}}}}"#,
                script.display()
            ),
        )
        .unwrap();

        let vault = Vault::open(
            &[remote(SecretSource::Mcp, Some("vault-fixture/get_secret"))],
            &dir,
        )
        .unwrap();
        assert_eq!(vault.get("token").unwrap().as_slice(), b"from-mcp");
        let rendered = format!("{vault:?}");
        assert!(!rendered.contains("from-mcp"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_source_still_requires_a_locator() {
        let err = Vault::open(&[remote(SecretSource::Url, None)], empty_dir()).unwrap_err();
        assert!(matches!(err, VaultError::MissingLocator { .. }));
        let err = Vault::open(&[remote(SecretSource::Mcp, None)], empty_dir()).unwrap_err();
        assert!(matches!(err, VaultError::MissingLocator { .. }));
    }

    #[test]
    fn duplicate_and_empty_names_fail() {
        assert!(matches!(
            Vault::open(&[local("  ", "x")], empty_dir()),
            Err(VaultError::EmptyName)
        ));
        assert!(matches!(
            Vault::open(&[local("a", "x"), local("a", "y")], empty_dir()),
            Err(VaultError::DuplicateName(_))
        ));
        assert!(matches!(
            Vault::open(&[local("a", "")], empty_dir()),
            Err(VaultError::MissingValue { .. })
        ));
    }
}
