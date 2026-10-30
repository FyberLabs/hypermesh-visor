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
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
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
