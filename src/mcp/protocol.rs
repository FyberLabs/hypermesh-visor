//! Deliberately bounded MCP client capability surface.
use super::McpError;
use serde_json::Value;

pub const VERSION: &str = "2025-11-25";

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

/// We advertise no roots/sampling/elicitation capabilities. Ping remains mandatory.
pub fn server_reply(value: &Value) -> Option<Value> {
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
