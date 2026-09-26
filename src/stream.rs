//! Open API-keyed stream for a session that is already running.
//!
//! After the session is open, a caller with `X-Api-Key` can deliver more
//! prompts, secrets, and files. The key is checked before the body is parsed
//! and before the session changes. A `kind: prompt` line is kept on the inbox
//! and also enters [`crate::prompt::PromptPass`]. Secret and file lines stay
//! off that door.

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
    Ok(deliver(session, body)?.acks)
}

pub(crate) struct ParkedPrompt {
    pub prompt: String,
    pub model: Option<String>,
}

pub(crate) struct Parked {
    pub acks: Vec<Ack>,
    pub prompts: Vec<ParkedPrompt>,
    pub file_texts: Vec<String>,
}

pub(crate) fn deliver(session: &mut Session, body: &str) -> Result<Parked, StreamError> {
    let items = parse(body)?;
    preflight(session, &items)?;
    let mut parked = Parked {
        acks: Vec::with_capacity(items.len()),
        prompts: Vec::new(),
        file_texts: Vec::new(),
    };
    for item in items {
        match item {
            Item::Prompt { prompt, model } => {
                session.push_prompt(prompt.clone(), model.clone());
                parked.prompts.push(ParkedPrompt { prompt, model });
                parked.acks.push(Ack::Prompt { accepted: true });
            }
            Item::File { name, bytes } => {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    parked.file_texts.push(text.to_string());
                }
                let len = bytes.len();
                session.push_file(name.clone(), bytes);
                parked.acks.push(Ack::File { name, bytes: len });
            }
            Item::Secret(request) => {
                let handle = session.push_secret(&request).map_err(StreamError::Vault)?;
                parked.acks.push(Ack::Secret { handle });
            }
        }
    }
    Ok(parked)
}

pub(crate) fn vault_needles(session: &Session) -> Vec<String> {
    session
        .secret_handles()
        .iter()
        .filter_map(|handle| {
            let bytes = session.vault().get(handle)?;
            let text = String::from_utf8(bytes.to_vec()).ok()?;
            if text.chars().count() >= 8 {
                Some(text)
            } else {
                None
            }
        })
        .collect()
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
        Vault::open(std::slice::from_ref(request), session.vault().mcp_dir())
            .map_err(StreamError::Vault)?;
    }
    Ok(())
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
            Vault::open(&[], std::env::temp_dir()).unwrap(),
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

    #[test]
    fn a_prompt_line_enters_the_pass_and_stays_on_the_inbox() {
        use crate::prompt::{PromptPass, RecordingDoor, DEFAULT_CATALOG_ID};

        let door = RecordingDoor::ok("baa");
        let pass = PromptPass::new(door.clone());
        let mut session = session();
        let parked = deliver(&mut session, &delivery()).unwrap();
        let needles = vault_needles(&session);
        pass.drive_parked(
            &parked
                .prompts
                .iter()
                .map(|prompt| (prompt.prompt.clone(), prompt.model.clone()))
                .collect::<Vec<_>>(),
            &parked.file_texts,
            FIXTURE_KEY,
            &needles,
        );

        assert_eq!(session.held_prompts().len(), 1);
        assert_eq!(session.held_prompts()[0].prompt, "count the sheep");
        assert!(session.held_prompts()[0].model.is_none());
        assert_eq!(session.held_files().len(), 1);
        assert_eq!(session.secret_handles(), ["token"]);
        assert_eq!(door.calls().len(), 1);
        assert_eq!(door.calls()[0].prompt, "count the sheep");
        assert_eq!(door.calls()[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
        assert!(!door.calls()[0].prompt.contains(SECRET_VALUE));
        assert_eq!(pass.audit()[0].outcome, "forwarded");
        assert_eq!(pass.audit()[0].model.as_deref(), Some(DEFAULT_CATALOG_ID));
    }

    #[test]
    fn secret_and_file_lines_stay_off_the_door() {
        use crate::prompt::{PromptPass, RecordingDoor};

        let door = RecordingDoor::ok("baa");
        let pass = PromptPass::new(door.clone());
        let mut session = session();
        let body = format!(
            "{}\n{}\n",
            r#"{"kind":"file","name":"note.txt","content_base64":"aGVsbG8="}"#,
            format!(
                r#"{{"kind":"secret","name":"token","source":"local","value":"{SECRET_VALUE}"}}"#
            ),
        );
        let parked = deliver(&mut session, &body).unwrap();
        pass.drive_parked(
            &[],
            &parked.file_texts,
            FIXTURE_KEY,
            &vault_needles(&session),
        );
        assert!(door.calls().is_empty());
        assert!(session.held_prompts().is_empty());
        assert_eq!(session.held_files().len(), 1);
        assert_eq!(session.secret_handles(), ["token"]);
        assert!(pass.audit().is_empty());
    }

    #[test]
    fn a_secret_on_an_inbox_line_is_flagged_without_being_stored() {
        use crate::prompt::{PromptPass, RecordingDoor};

        let door = RecordingDoor::ok("baa");
        let pass = PromptPass::new(door.clone());
        let mut session = session();
        let leaked = "vault-secret-value";
        let body = format!(
            "{}\n{}\n",
            format!(r#"{{"kind":"prompt","prompt":"see {leaked} please"}}"#),
            format!(
                r#"{{"kind":"file","name":"note.txt","content_base64":"{}"}}"#,
                base64::engine::general_purpose::STANDARD.encode(leaked)
            ),
        );
        let parked = deliver(&mut session, &body).unwrap();
        pass.drive_parked(
            &parked
                .prompts
                .iter()
                .map(|prompt| (prompt.prompt.clone(), prompt.model.clone()))
                .collect::<Vec<_>>(),
            &parked.file_texts,
            FIXTURE_KEY,
            &[leaked.to_string()],
        );
        assert_eq!(door.calls().len(), 1);
        assert_eq!(door.calls()[0].prompt, format!("see {leaked} please"));
        assert_eq!(
            session.held_prompts()[0].prompt,
            format!("see {leaked} please")
        );
        let json = serde_json::to_string(&pass.audit()).unwrap();
        assert!(!json.contains(leaked));
        let audit = pass.audit();
        assert!(audit.iter().any(|row| {
            row.findings
                .iter()
                .any(|finding| finding.kind == "secret" && finding.place == "prompt")
        }));
        assert!(audit.iter().any(|row| {
            row.findings
                .iter()
                .any(|finding| finding.kind == "secret" && finding.place == "inbox")
        }));
    }
}
