//! JSONC lexical extensions only; serde_json still validates the JSON grammar.
//! Replace comments with spaces to avoid joining tokens, and never edit strings.
use super::McpError;
use serde_json::Value;

fn invalid() -> McpError {
    McpError::message("Invalid MCP JSON/JSONC; only comments and trailing commas extend JSON")
}
pub(super) fn parse(raw: &str) -> Result<Value, McpError> {
    let mut bytes = raw.as_bytes().to_vec();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => skip_string(&bytes, &mut i)?,
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && !matches!(bytes[i], b'\n' | b'\r') {
                    bytes[i] = b' ';
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                bytes[i] = b' ';
                bytes[i + 1] = b' ';
                i += 2;
                loop {
                    if i + 1 >= bytes.len() {
                        return Err(invalid());
                    }
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        bytes[i] = b' ';
                        bytes[i + 1] = b' ';
                        i += 2;
                        break;
                    }
                    if !matches!(bytes[i], b'\n' | b'\r') {
                        bytes[i] = b' ';
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    i = 0;
    let mut previous = None;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                skip_string(&bytes, &mut i)?;
                previous = Some(b'"');
            }
            b',' => {
                let next = bytes[i + 1..].iter().find(|b| !b.is_ascii_whitespace());
                // Do not turn [,], {,}, [1,,], or {"x":,} into valid JSON.
                if matches!(next, Some(b']' | b'}'))
                    && previous.is_some_and(|b| !matches!(b, b'[' | b'{' | b',' | b':'))
                {
                    bytes[i] = b' ';
                }
                previous = Some(b',');
                i += 1;
            }
            b if b.is_ascii_whitespace() => i += 1,
            b => {
                previous = Some(b);
                i += 1;
            }
        }
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
fn skip_string(bytes: &[u8], i: &mut usize) -> Result<(), McpError> {
    *i += 1;
    while *i < bytes.len() {
        match bytes[*i] {
            b'\\' => *i += 2,
            b'"' => {
                *i += 1;
                return Ok(());
            }
            _ => *i += 1,
        }
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comments_and_trailing_commas_preserve_string_values() {
        let value = parse(
            r#"{
            // a line comment
            "url": "https://example.com/a//b/*c*/,}",
            "escaped": "a\"//still a string",
            "unicode": "é中", /* a block comment */
            "list": [1, {"x": true,},], // trailing commas
        }"#,
        )
        .unwrap();
        assert_eq!(value["url"], "https://example.com/a//b/*c*/,}");
        assert_eq!(value["escaped"], "a\"//still a string");
        assert_eq!(value["unicode"], "é中");
        assert_eq!(value["list"][1]["x"], true);
        assert_eq!(parse("[1,/*x*/2,]").unwrap(), serde_json::json!([1, 2]));
    }
    #[test]
    fn rejects_malformed_json_and_does_not_join_tokens() {
        for raw in [
            "[,]",
            "{,}",
            "[1,,]",
            r#"{"x":,}"#,
            "/*",
            "/*x*",
            "[1/*x*/2]",
            r#"{"x":tr/*x*/ue}"#,
            "{bare:1}",
            "{'single':1}",
            "[NaN]",
            "[] garbage",
            r#"{"x":"unterminated}"#,
            "[1, // no closing bracket",
        ] {
            assert!(parse(raw).is_err(), "accepted {raw}");
        }
    }
}
