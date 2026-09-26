//! Open API-keyed stream for a session that is already running.
//!
//! `POST /prompt` still forwards one prompt through the supervisor door.
//! This stream does not. After the session is open, a caller with `X-Api-Key`
//! can deliver more prompts, secrets, and files. The key is checked before
//! the body is parsed and before the session changes.

use std::collections::HashSet;

use base64::Engine;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::prompt::{self, PromptError};
use crate::session::Session;
use crate::vault::{SecretRequest, SecretSource, Vault, VaultError};

pub const MAX_STREAM_BYTES: usize = 1 << 20;

#[derive(Debug)]
pub(crate) enum StreamError {
    Key(PromptError),
    Closed,
    Empty,
    Bad(&'static str),
    Vault(VaultError),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Key(err) => write!(f, "{err}"),
            Self::Closed => write!(f, "session closed or unknown"),
            Self::Empty => write!(f, "stream input is required"),
            Self::Bad(message) => write!(f, "{message}"),
            Self::Vault(err) => write!(f, "{err}"),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum Ack {
    Prompt { accepted: bool },
    File { name: String, bytes: usize },
    Secret { handle: String },
}

#[derive(Serialize)]
pub(crate) struct Inbox {
    pub prompts: Vec<InboxPrompt>,
    pub files: Vec<InboxFile>,
    pub secrets: Vec<InboxSecret>,
}

#[derive(Serialize)]
pub(crate) struct InboxPrompt {
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct InboxFile {
    pub name: String,
    pub bytes: usize,
    pub content_base64: String,
}

#[derive(Serialize)]
pub(crate) struct InboxSecret {
    pub handle: String,
}

enum Item {
    Prompt {
        prompt: String,
        model: Option<String>,
    },
    File {
        name: String,
        bytes: Vec<u8>,
    },
    Secret(SecretRequest),
}

pub(crate) fn inbox(session: &Session) -> Inbox {
    Inbox {
        prompts: session
            .held_prompts()
            .iter()
            .map(|prompt| InboxPrompt {
                prompt: prompt.prompt.clone(),
                model: prompt.model.clone(),
            })
            .collect(),
        files: session
            .held_files()
            .iter()
            .map(|file| InboxFile {
                name: file.name.clone(),
                bytes: file.bytes.len(),
                content_base64: base64::engine::general_purpose::STANDARD.encode(&file.bytes),
            })
            .collect(),
        secrets: session
            .secret_handles()
            .iter()
            .map(|handle| InboxSecret {
                handle: handle.clone(),
            })
            .collect(),
    }
}

pub(crate) fn render(acks: &[Ack]) -> Result<String, StreamError> {
    let mut out = String::new();
    for ack in acks {
        let line = serde_json::to_string(ack)
            .map_err(|_| StreamError::Bad("stream ack could not be encoded"))?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// Rejects a missing or forbidden key before any stream item is parsed.
pub(crate) fn gate(api_key: &str) -> Result<(), StreamError> {
    prompt::validate_api_key(api_key)
        .map_err(StreamError::Key)
        .map(|_| ())
}

/// Checks the key before parsing. A rejected key does not read the body as items.
#[cfg(test)]
fn accept(session: &mut Session, api_key: &str, body: &str) -> Result<Vec<Ack>, StreamError> {
    gate(api_key)?;
    deliver(session, body)
}

pub(crate) fn deliver(session: &mut Session, body: &str) -> Result<Vec<Ack>, StreamError> {
    let items = parse(body)?;
    preflight(session, &items)?;
    let mut acks = Vec::with_capacity(items.len());
    for item in items {
        acks.push(apply(session, item)?);
    }
    Ok(acks)
}

fn preflight(session: &Session, items: &[Item]) -> Result<(), StreamError> {
    let mut names = HashSet::new();
    for item in items {
        let Item::Secret(request) = item else {
            continue;
        };
        if session.vault().contains(&request.name) || !names.insert(request.name.clone()) {
            return Err(StreamError::Vault(VaultError::DuplicateName(
                request.name.clone(),
            )));
        }
        Vault::open(std::slice::from_ref(request)).map_err(StreamError::Vault)?;
    }
    Ok(())
}

fn apply(session: &mut Session, item: Item) -> Result<Ack, StreamError> {
    match item {
        Item::Prompt { prompt, model } => {
            session.push_prompt(prompt, model);
            Ok(Ack::Prompt { accepted: true })
        }
        Item::File { name, bytes } => {
            let len = bytes.len();
            session.push_file(name.clone(), bytes);
            Ok(Ack::File { name, bytes: len })
        }
        Item::Secret(request) => {
            let handle = session.push_secret(&request).map_err(StreamError::Vault)?;
            Ok(Ack::Secret { handle })
        }
    }
}

fn parse(body: &str) -> Result<Vec<Item>, StreamError> {
    let mut items = Vec::new();
    for line in body.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        items.push(parse_line(line)?);
    }
    if items.is_empty() {
        return Err(StreamError::Empty);
    }
    Ok(items)
}

fn parse_line(line: &str) -> Result<Item, StreamError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|_| StreamError::Bad("stream line is not json"))?;
    let kind = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match kind {
        "prompt" => parse_prompt(&value),
        "file" => parse_file(&value),
        "secret" => parse_secret(&value),
        _ => Err(StreamError::Bad("unknown stream kind")),
    }
}

fn parse_prompt(value: &serde_json::Value) -> Result<Item, StreamError> {
    let prompt = value
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if prompt.is_empty() {
        return Err(StreamError::Bad("prompt is required"));
    }
    let model = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string);
    Ok(Item::Prompt { prompt, model })
}

fn parse_file(value: &serde_json::Value) -> Result<Item, StreamError> {
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(StreamError::Bad("file name is required"));
    }
    let encoded = value
        .get("content_base64")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if encoded.is_empty() {
        return Err(StreamError::Bad("file content is required"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| StreamError::Bad("file content is not base64"))?;
    Ok(Item::File { name, bytes })
}

fn parse_secret(value: &serde_json::Value) -> Result<Item, StreamError> {
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(StreamError::Bad("secret name is required"));
    }
    let source = match value
        .get("source")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
    {
        "local" => SecretSource::Local,
        "url" => SecretSource::Url,
        "mcp" => SecretSource::Mcp,
        "chain" => SecretSource::Chain,
        "ipfs" => SecretSource::Ipfs,
        _ => return Err(StreamError::Bad("secret source is required")),
    };
    let secret_value = value
        .get("value")
        .and_then(serde_json::Value::as_str)
        .map(|text| Zeroizing::new(text.to_string()));
    let locator = value
        .get("locator")
        .and_then(serde_json::Value::as_str)
        .map(|text| Zeroizing::new(text.to_string()));
    Ok(Item::Secret(SecretRequest {
        name,
        source,
        value: secret_value,
        locator,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{Agent, Harness, Recipe, Skill};
    use uuid::Uuid;

    const FIXTURE_KEY: &str = "org_fixture_ok";
    const SECRET_VALUE: &str = "stream-secret-value";

    fn session() -> Session {
        let harness = Harness {
            purpose: "drive the desktop".into(),
            recipes: vec![Recipe {
                name: "focus".into(),
                steps: vec!["look".into()],
            }],
            agent: Agent {
                name: "desk".into(),
                instructions: String::new(),
            },
            skills: vec![Skill {
                name: "typing".into(),
                instructions: String::new(),
            }],
        };
        Session::create(
            Uuid::new_v4(),
            harness,
            Vault::open(&[]).unwrap(),
            crate::mcp::McpBundle::default(),
        )
    }

    fn delivery() -> String {
        format!(
            "{}\n{}\n{}\n",
            r#"{"kind":"prompt","prompt":"count the sheep","api_key":"org_fixture_ok"}"#,
            r#"{"kind":"file","name":"note.txt","content_base64":"aGVsbG8="}"#,
            format!(
                r#"{{"kind":"secret","name":"token","source":"local","value":"{SECRET_VALUE}"}}"#
            ),
        )
    }

    #[test]
    fn rejected_key_does_not_parse_or_deliver() {
        let mut session = session();
        let garbage = format!("not-json {SECRET_VALUE}");
        for (key, prefix) in [
            ("", None),
            ("   ", None),
            ("hm_dev_fixture_tail", Some("hm_dev_")),
            ("hm_rtr_fixture_tail", Some("hm_rtr_")),
            ("hm_site_fixture_tail", Some("hm_site_")),
        ] {
            let err = accept(&mut session, key, &garbage).unwrap_err();
            match prefix {
                None => assert!(matches!(err, StreamError::Key(PromptError::MissingKey))),
                Some(prefix) => {
                    assert!(matches!(
                        err,
                        StreamError::Key(PromptError::InvalidKey { prefix: got }) if got == prefix
                    ));
                    let text = err.to_string();
                    assert!(text.contains(prefix));
                    assert!(!text.contains("fixture_tail"));
                    assert!(!text.contains(SECRET_VALUE));
                }
            }
        }
        assert!(session.held_prompts().is_empty());
        assert!(session.held_files().is_empty());
        assert!(session.secret_handles().is_empty());
        assert!(session.vault().get("token").is_none());
    }

    #[test]
    fn fixture_key_delivers_a_prompt_a_file_and_a_secret_handle() {
        let mut session = session();
        let acks = accept(&mut session, FIXTURE_KEY, &delivery()).unwrap();
        let rendered = render(&acks).unwrap();
        assert!(!rendered.contains(FIXTURE_KEY));
        assert!(!rendered.contains(SECRET_VALUE));
        assert!(rendered.contains("\"kind\":\"prompt\""));
        assert!(rendered.contains("\"name\":\"note.txt\""));
        assert!(rendered.contains("\"handle\":\"token\""));

        assert_eq!(session.held_prompts().len(), 1);
        assert_eq!(session.held_prompts()[0].prompt, "count the sheep");
        assert_eq!(session.held_prompts()[0].model, None);
        assert_eq!(session.held_files().len(), 1);
        assert_eq!(session.held_files()[0].name, "note.txt");
        assert_eq!(session.held_files()[0].bytes, b"hello");
        assert_eq!(session.secret_handles(), ["token"]);
        assert_eq!(
            session.vault().get("token").unwrap().as_slice(),
            SECRET_VALUE.as_bytes()
        );

        let inbox = inbox(&session);
        let json = serde_json::to_string(&inbox).unwrap();
        assert!(!json.contains(FIXTURE_KEY));
        assert!(!json.contains(SECRET_VALUE));
        assert!(json.contains("count the sheep"));
        assert!(json.contains("note.txt"));
        assert!(json.contains("token"));
        assert!(inbox.prompts[0].model.is_none());
    }

    #[test]
    fn model_is_kept_only_when_the_caller_set_it() {
        let mut session = session();
        accept(
            &mut session,
            FIXTURE_KEY,
            "{\"kind\":\"prompt\",\"prompt\":\"count the sheep\",\"model\":\"caller-picked\"}\n",
        )
        .unwrap();
        accept(
            &mut session,
            FIXTURE_KEY,
            "{\"kind\":\"prompt\",\"prompt\":\"again\",\"model\":\"  \"}\n",
        )
        .unwrap();
        assert_eq!(
            session.held_prompts()[0].model.as_deref(),
            Some("caller-picked")
        );
        assert_eq!(session.held_prompts()[1].model, None);
        assert_ne!(
            session.held_prompts()[0].model.as_deref(),
            Some("llama-3.1-8b-q4")
        );
    }
}
