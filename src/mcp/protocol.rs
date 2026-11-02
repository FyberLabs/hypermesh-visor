//! Deliberately bounded MCP client capability surface.
use super::McpError;
use serde_json::Value;

pub const VERSION: &str = "2025-11-25";

#[derive(Debug, Clone, serde::Serialize)]
pub struct Negotiated {
    pub version: String,
    pub tools: bool,
    pub list_changed: bool,
}

pub fn negotiate(response: &Value) -> Result<Negotiated, McpError> {
    let result = response
        .get("result")
        .ok_or_else(|| McpError::message("MCP initialize rejected"))?;
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| matches!(*v, "2024-11-05" | "2025-03-26" | "2025-06-18" | VERSION))
        .ok_or_else(|| McpError::message("MCP unsupported protocol version"))?;
    let caps = result
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| McpError::message("MCP invalid server capabilities"))?;
    let tools = caps.get("tools");
    if tools.is_some_and(|v| !v.is_object())
        || tools
            .and_then(|t| t.get("listChanged"))
            .is_some_and(|v| !v.is_boolean())
    {
        return Err(McpError::message("MCP invalid tools capability"));
    }
    Ok(Negotiated {
        version: version.into(),
        tools: tools.is_some(),
        list_changed: tools
            .and_then(|t| t.get("listChanged"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Validate correlated MCP response envelopes without echoing remote data.
/// A valid JSON-RPC error remains a response, not a transport failure.
pub fn validate_response(response: &Value, expected: &Value) -> Result<(), McpError> {
    if response.get("id") != Some(expected) || response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpError::message("MCP response id/version mismatch"));
    }
    if response.get("method").is_some() {
        return Err(McpError::message("MCP response contains a request method"));
    }
    match (response.get("result"), response.get("error")) {
        (Some(result), None) if result.is_object() => Ok(()),
        (None, Some(error)) if error.is_object()
            && error.get("code").is_some_and(|code| code.is_i64() || code.is_u64())
            && error.get("message").is_some_and(Value::is_string) => Ok(()),
        _ => Err(McpError::message("MCP invalid result/error envelope")),
    }
}

/// We advertise no roots/sampling/elicitation capabilities. Ping remains mandatory.
pub fn server_reply(value: &Value) -> Option<Value> {
    if value.get("result").is_some() || value.get("error").is_some() { return None; }
    let id = value.get("id")?;
    let method = value.get("method")?.as_str()?;
    if value["jsonrpc"] != "2.0" || !(id.is_string() || id.is_number()) {
        return None;
    }
    Some(if method == "ping" {
        serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}})
    } else {
        serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Unsupported client method"}})
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn response_envelopes_require_exactly_one_well_formed_outcome() {
        for body in [json!({"result":{}}), json!({"result":{"isError":true}}),
            json!({"error":{"code":-32601,"message":"private","data":{"anything":true}}})] {
            let mut response = body;
            response["id"] = json!(7);
            response["jsonrpc"] = json!("2.0");
            validate_response(&response, &json!(7)).unwrap();
        }
        for body in [json!({}), json!({"result":null}), json!({"result":[]}),
            json!({"result":{},"error":{"code":-1,"message":"private"}}),
            json!({"error":null}), json!({"error":{}}),
            json!({"error":{"code":1.5,"message":"private"}}),
            json!({"error":{"code":-1,"message":4}}),
            json!({"result":{},"method":"ping"})] {
            let mut response = body;
            response["id"] = json!(7);
            response["jsonrpc"] = json!("2.0");
            let error = validate_response(&response, &json!(7)).unwrap_err().to_string();
            assert!(!error.contains("private"));
            assert!(server_reply(&response).is_none());
        }
        assert!(validate_response(&json!({"id":8,"jsonrpc":"2.0","result":{}}), &json!(7)).is_err());
        assert!(validate_response(&json!({"id":7,"jsonrpc":"1.0","result":{}}), &json!(7)).is_err());
    }

    #[test]
    fn negotiates_only_supported_versions_and_valid_capabilities() {
        for version in ["2024-11-05", "2025-03-26", "2025-06-18", VERSION] {
            let n = negotiate(&json!({"result":{"protocolVersion":version,"capabilities":{"tools":{"listChanged":true}}}})).unwrap();
            assert!(n.tools && n.list_changed);
        }
        for result in [
            json!({"protocolVersion":"future","capabilities":{}}),
            json!({"protocolVersion":VERSION}),
            json!({"protocolVersion":VERSION,"capabilities":{"tools":null}}),
            json!({"protocolVersion":VERSION,"capabilities":{"tools":{"listChanged":"yes"}}}),
        ] {
            assert!(negotiate(&json!({"result":result})).is_err());
        }
        assert!(
            !negotiate(&json!({"result":{"protocolVersion":VERSION,"capabilities":{}}}))
                .unwrap()
                .tools
        );
    }
}
