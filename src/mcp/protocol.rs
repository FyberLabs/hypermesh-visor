//! Deliberately bounded MCP client capability surface.
use super::McpError;
use serde_json::Value;

pub const VERSION: &str = "2025-11-25";

#[derive(Debug, Clone, serde::Serialize)]
pub struct Negotiated {
    pub version: String,
    pub tools: bool,
    pub list_changed: bool,
    // Session API exposes this explicitly; compact status serialization excludes it.
    #[serde(skip)]
    pub server_info: Option<Value>,
    #[serde(skip)]
    pub server_capabilities: Value,
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
    let server_capabilities = capability_summary(&result["capabilities"])?;
    let server_info = implementation_info(result.get("serverInfo"))?;
    Ok(Negotiated {
        server_info,
        server_capabilities,
        version: version.into(),
        tools: tools.is_some(),
        list_changed: tools
            .and_then(|t| t.get("listChanged"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

// Fixed schema projection: retain no remote strings, extension payloads, or
// arbitrary keys. Presence and false values stay distinct from absence.
fn capability_summary(value: &Value) -> Result<Value, McpError> {
    let shape = serde_json::json!({
        "tools":{"listChanged":false}, "prompts":{"listChanged":false},
        "resources":{"subscribe":false,"listChanged":false},
        "logging":{}, "completions":{},
        "tasks":{"list":{},"cancel":{},"requests":{"tools":{"call":{}}}}
    });
    fn project(value: &Value, shape: &Value) -> Result<Value, McpError> {
        if shape.is_boolean() {
            return value.as_bool().map(Value::Bool)
                .ok_or_else(|| McpError::message("MCP invalid server capability flag"));
        }
        let object = value.as_object()
            .ok_or_else(|| McpError::message("MCP invalid server capability object"))?;
        let mut result = serde_json::Map::new();
        for (key, child_shape) in shape.as_object().unwrap() {
            if let Some(child) = object.get(key) {
                result.insert(key.clone(), project(child, child_shape)?);
            }
        }
        Ok(Value::Object(result))
    }
    project(value, &shape)
}

// Retain only documented implementation metadata, never initialize instructions.
fn implementation_info(value: Option<&Value>) -> Result<Option<Value>, McpError> {
    let Some(value) = value else { return Ok(None); }; // Compatibility with older/minimal servers.
    if !value.is_object() || value.to_string().len() > 16 * 1024 {
        return Err(McpError::message("MCP invalid or oversized server info"));
    }
    let mut info = serde_json::Map::new();
    for key in ["name", "version"] {
        let text = value.get(key).and_then(Value::as_str)
            .ok_or_else(|| McpError::message("MCP server info requires text name/version"))?;
        info.insert(key.into(), Value::String(text.into()));
    }
    for key in ["title", "description", "websiteUrl"] {
        if let Some(text) = value.get(key) {
            if !text.is_string() { return Err(McpError::message("MCP server info metadata must be text")); }
            info.insert(key.into(), text.clone());
        }
    }
    if let Some(icons) = super::discovery::optional_icons(value)? {
        info.insert("icons".into(), icons);
    }
    Ok(Some(Value::Object(info)))
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

/// Only well-formed notifications may invalidate a discovered catalog.
pub fn is_notification(value: &Value, method: &str) -> bool {
    value["jsonrpc"] == "2.0"
        && value.get("id").is_none()
        && value["method"] == method
        && value.get("result").is_none()
        && value.get("error").is_none()
        && value.get("params").is_none_or(Value::is_object)
}

/// We advertise no roots/sampling/elicitation capabilities. Ping remains mandatory.
pub fn server_reply(value: &Value) -> Option<Value> {
    if value.get("result").is_some() || value.get("error").is_some() { return None; }
    let id = value.get("id")?;
    let method = value.get("method")?.as_str()?;
    if value["jsonrpc"] != "2.0" || !(id.is_string() || id.is_i64() || id.is_u64()) {
        return None;
    }
    Some(if value.get("params").is_some_and(|params| !params.is_object()) {
        serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"Invalid params"}})
    } else if method == "ping" {
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
    fn capability_discovery_preserves_presence_and_ignores_extensions() {
        let caps = serde_json::json!({"resources":{"subscribe":false,"listChanged":true,"extension":"private"},
            "prompts":{},"logging":{"extra":"private"},"completions":{},
            "tasks":{"list":{},"cancel":{},"requests":{"tools":{"call":{}}}},
            "experimental":{"private":{"secret":"private"}},"future":"private"});
        let summary = capability_summary(&caps).unwrap();
        assert_eq!(summary, serde_json::json!({"resources":{"subscribe":false,"listChanged":true},
            "prompts":{},"logging":{},"completions":{},
            "tasks":{"list":{},"cancel":{},"requests":{"tools":{"call":{}}}}}));
        let negotiated = negotiate(&serde_json::json!({"result":{"protocolVersion":VERSION,"capabilities":caps}})).unwrap();
        assert!(!negotiated.tools);
        assert_eq!(negotiated.server_capabilities, summary);
        assert!(!serde_json::to_string(&negotiated).unwrap().contains("resources"));
        assert_eq!(capability_summary(&serde_json::json!({})).unwrap(), serde_json::json!({}));
    }

    #[test]
    fn malformed_known_capabilities_reject_initialization_without_payloads() {
        for caps in [serde_json::json!({"resources":null}), serde_json::json!({"prompts":[]}),
            serde_json::json!({"resources":{"subscribe":"private"}}),
            serde_json::json!({"prompts":{"listChanged":0}}), serde_json::json!({"logging":true}),
            serde_json::json!({"completions":"private"}), serde_json::json!({"tasks":{"list":false}}),
            serde_json::json!({"tasks":{"requests":{"tools":{"call":null}}}})] {
            let error = negotiate(&serde_json::json!({"result":{"protocolVersion":VERSION,"capabilities":caps}})).unwrap_err().to_string();
            assert!(!error.contains("private"));
        }
    }

    #[test]
    fn implementation_metadata_is_bounded_validated_and_excluded_from_status() {
        let info = json!({"name":"server","version":"1","title":"Title","description":"Text",
            "websiteUrl":"https://invalid.example/private","icons":[{"src":"data:image/png;base64,AA=="}],
            "unknown":"private-extra"});
        let negotiated = negotiate(&json!({"result":{"protocolVersion":VERSION,"capabilities":{},"serverInfo":info,"instructions":"private-instructions"}})).unwrap();
        let retained = negotiated.server_info.as_ref().unwrap();
        assert_eq!(retained["name"], "server");
        assert_eq!(retained["icons"], info["icons"]);
        assert!(retained.get("unknown").is_none());
        let summary = serde_json::to_value(&negotiated).unwrap();
        assert_eq!(summary, json!({"version":VERSION,"tools":false,"list_changed":false}));
        for info in [Value::Null, json!({}), json!({"name":"private","version":1}),
            json!({"name":"private","version":"1","title":null}),
            json!({"name":"private","version":"1","icons":[{"src":5}]}),
            json!({"name":"private","version":"1","description":"x".repeat(16*1024)})] {
            let error = implementation_info(Some(&info)).unwrap_err().to_string();
            assert!(!error.contains("private"));
        }
        assert!(implementation_info(None).unwrap().is_none());
    }

    #[test]
    fn server_requests_validate_ids_and_params_without_echoing_payloads() {
        for id in [json!("request"), json!(0), json!(-1), json!(u64::MAX)] {
            for params in [json!({}), json!({"_meta":{"private":true}})] {
                let reply = server_reply(&json!({"jsonrpc":"2.0","id":id,"method":"ping","params":params})).unwrap();
                assert_eq!(reply, json!({"jsonrpc":"2.0","id":id,"result":{}}));
            }
            for params in [Value::Null, json!([]), json!("private"), json!(4)] {
                let reply = server_reply(&json!({"jsonrpc":"2.0","id":id,"method":"ping","params":params})).unwrap();
                assert_eq!(reply["error"]["code"], -32602);
                assert!(!reply.to_string().contains("private"));
            }
        }
        for id in [Value::Null, json!(1.5), json!(true), json!([])] {
            assert!(server_reply(&json!({"jsonrpc":"2.0","id":id,"method":"ping"})).is_none());
        }
        assert_eq!(server_reply(&json!({"jsonrpc":"2.0","id":1,"method":"roots/list"})).unwrap()["error"]["code"], -32601);
        assert!(server_reply(&json!({"jsonrpc":"2.0","method":"ping"})).is_none());
    }

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
