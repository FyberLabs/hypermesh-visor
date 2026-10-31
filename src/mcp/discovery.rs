//! Shared, bounded tool discovery. Cursors are opaque and scoped to one refresh.
use super::{stdio::ToolInfo, McpError};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::{Duration, Instant};

pub fn list_tools(
    mut request: impl FnMut(Value) -> Result<Value, McpError>,
) -> Result<Vec<ToolInfo>, McpError> {
    let mut tools = Vec::new();
    let mut bytes = 0;
    let mut names = HashSet::new();
    let mut cursors = HashSet::new();
    let mut params = json!({});
    let started = Instant::now();
    for _ in 0..32 {
        if started.elapsed() >= Duration::from_secs(30) {
            return Err(McpError::message("MCP tool discovery deadline exceeded"));
        }
        let response = request(params)?;
        if response.get("error").is_some() {
            return Err(McpError::message("MCP tools/list rejected"));
        }
        let result = response
            .get("result")
            .ok_or_else(|| McpError::message("MCP tools/list missing result"))?;
        let page = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::message("MCP tools/list missing tools"))?;
        for tool in page {
            bytes += tool.to_string().len();
            if bytes > 8 * 1024 * 1024 {
                return Err(McpError::message("MCP tool catalog too large"));
            }
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 1024)
                .ok_or_else(|| McpError::message("MCP invalid tool name"))?;
            if tools.len() >= 4096 || !names.insert(name.to_string()) {
                return Err(McpError::message("MCP tool limit or duplicate name"));
            }
            tools.push(ToolInfo {
                name: name.into(),
                description: optional_text(tool, "description")?,
                title: optional_text(tool, "title")?,
                input_schema: optional_schema(tool, "inputSchema")?,
                output_schema: optional_schema(tool, "outputSchema")?,
                annotations: optional_annotations(tool)?,
                execution: optional_execution(tool)?,
            });
        }
        match result.get("nextCursor") {
            None => return Ok(tools),
            Some(Value::String(cursor))
                if cursor.len() <= 4096 && cursors.insert(cursor.clone()) =>
            {
                params = json!({"cursor":cursor})
            }
            _ => {
                return Err(McpError::message(
                    "MCP invalid or repeated pagination cursor",
                ))
            }
        }
    }
    Err(McpError::message("MCP tool page limit exceeded"))
}

// Preserve schema data without evaluating references or fetching remote content.
// Older servers may omit inputSchema; never synthesize an empty-argument schema.
fn optional_schema(tool: &Value, field: &str) -> Result<Option<Value>, McpError> {
    match tool.get(field) {
        None => Ok(None),
        Some(value) if value.is_object() => Ok(Some(value.clone())),
        _ => Err(McpError::message("MCP tool schema must be an object")),
    }
}

fn optional_text(tool: &Value, field: &str) -> Result<Option<String>, McpError> {
    match tool.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(McpError::message("MCP tool metadata must be text")),
    }
}

fn optional_annotations(tool: &Value) -> Result<Option<Value>, McpError> {
    let Some(value) = tool.get("annotations") else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| McpError::message("MCP annotations must be an object"))?;
    for key in [
        "readOnlyHint",
        "destructiveHint",
        "idempotentHint",
        "openWorldHint",
    ] {
        if object.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(McpError::message("MCP annotation hint must be boolean"));
        }
    }
    if object.get("title").is_some_and(|v| !v.is_string()) {
        return Err(McpError::message("MCP annotation title must be text"));
    }
    Ok(Some(value.clone()))
}

fn optional_execution(tool: &Value) -> Result<Option<Value>, McpError> {
    let Some(value) = tool.get("execution") else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| McpError::message("MCP execution metadata must be an object"))?;
    if object
        .get("taskSupport")
        .is_some_and(|v| !matches!(v.as_str(), Some("forbidden" | "optional" | "required")))
    {
        return Err(McpError::message("MCP task support value is unsupported"));
    }
    Ok(Some(value.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_opaque_cursors_and_empty_pages() {
        let mut calls = 0;
        let tools = list_tools(|params| {
            calls += 1;
            Ok(match calls {
                1 => {
                    assert_eq!(params, json!({}));
                    json!({"result":{"tools":[],"nextCursor":" a/+== "}})
                }
                2 => {
                    assert_eq!(params, json!({"cursor":" a/+== "}));
                    json!({"result":{"tools":[{"name":"last"}]}})
                }
                _ => panic!("unexpected page"),
            })
        })
        .unwrap();
        assert_eq!(tools[0].name, "last");
    }
    #[test]
    fn schemas_and_titles_survive_pagination_without_evaluating_references() {
        let schema = json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"$defs":{"x":{"$ref":"https://invalid.example/schema"}}});
        let mut page = 0;
        let tools = list_tools(|_| {
            page += 1;
            Ok(if page == 1 { json!({"result":{"tools":[{"name":"first","title":"Search","description":"Find records","inputSchema":schema,"outputSchema":{"type":"object"}}],"nextCursor":"next"}}) }
                else { json!({"result":{"tools":[{"name":"legacy"}]}}) })
        }).unwrap();
        let exported = serde_json::to_value(&tools).unwrap();
        assert_eq!(exported[0]["inputSchema"], schema);
        assert_eq!(exported[0]["outputSchema"], json!({"type":"object"}));
        assert_eq!(exported[0]["title"], "Search");
        assert_eq!(exported[1], json!({"name":"legacy"}));
    }

    #[test]
    fn malformed_metadata_rejects_the_whole_catalog_without_echoing_values() {
        for field in ["inputSchema", "outputSchema", "title", "description"] {
            for bad in [json!(null), json!(3), json!(["private"])] {
                let result = list_tools(|_| {
                    Ok(json!({"result":{"tools":[{"name":"valid"},{"name":"invalid",field:bad}]}}))
                });
                let error = result.unwrap_err().to_string();
                assert!(!error.contains("private"));
            }
        }
        for bad in [json!(false), json!("private")] {
            assert!(list_tools(|_| Ok(
                json!({"result":{"tools":[{"name":"bad","inputSchema":bad}]}})
            ))
            .is_err());
        }
        assert!(list_tools(|_| Ok(json!({"result":{"tools":[{"name":"large","inputSchema":{"description":"x".repeat(8*1024*1024)}}]}}))).is_err());
    }

    #[test]
    fn execution_and_annotations_roundtrip_without_inventing_defaults() {
        for support in ["forbidden", "optional", "required"] {
            let tool = json!({"name":"x","annotations":{"title":"Display","readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false,"extension":{"value":"untrusted"}},"execution":{"taskSupport":support}});
            let tools = list_tools(|_| Ok(json!({"result":{"tools":[tool]}}))).unwrap();
            assert_eq!(serde_json::to_value(&tools[0]).unwrap(), tool);
        }
        let tools = list_tools(|_| Ok(json!({"result":{"tools":[{"name":"legacy"}]}}))).unwrap();
        assert_eq!(
            serde_json::to_value(&tools[0]).unwrap(),
            json!({"name":"legacy"})
        );
    }

    #[test]
    fn invalid_execution_or_annotations_reject_the_entire_catalog() {
        for invalid in [
            json!({"annotations":null}),
            json!({"annotations":[]}),
            json!({"annotations":{"readOnlyHint":"private"}}),
            json!({"annotations":{"destructiveHint":0}}),
            json!({"annotations":{"idempotentHint":null}}),
            json!({"annotations":{"openWorldHint":[]}}),
            json!({"annotations":{"title":false}}),
            json!({"execution":null}),
            json!({"execution":[]}),
            json!({"execution":{"taskSupport":"private"}}),
            json!({"execution":{"taskSupport":false}}),
            json!({"execution":{"taskSupport":null}}),
        ] {
            let mut tool = invalid;
            tool["name"] = json!("bad");
            let error = list_tools(|_| Ok(json!({"result":{"tools":[{"name":"valid"},tool]}})))
                .unwrap_err()
                .to_string();
            assert!(!error.contains("private"));
        }
    }

    #[test]
    fn rejects_cycles_duplicates_invalid_cursors_and_limits() {
        assert!(list_tools(|_| Ok(json!({"result":{"tools":[],"nextCursor":"same"}}))).is_err());
        assert!(
            list_tools(|_| Ok(json!({"result":{"tools":[{"name":"x"},{"name":"x"}]}}))).is_err()
        );
        assert!(list_tools(|_| Ok(json!({"result":{"tools":[],"nextCursor":4}}))).is_err());
        let mut n = 0;
        assert!(list_tools(|_| {
            n += 1;
            Ok(json!({"result":{"tools":[],"nextCursor":n.to_string()}}))
        })
        .is_err());
        assert_eq!(n, 32);
    }
}
