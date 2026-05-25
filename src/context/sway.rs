use crate::context::AppContext;
use anyhow::{Context, Result};
use serde_json::Value;
use std::process::Command;

pub fn focused_context() -> Result<Option<AppContext>> {
    let output = Command::new("swaymsg")
        .args(["-t", "get_tree", "--raw"])
        .output()
        .context("failed to run swaymsg")?;

    if !output.status.success() {
        return Ok(None);
    }

    let tree: Value =
        serde_json::from_slice(&output.stdout).context("failed to parse sway tree")?;
    Ok(find_focused_node(&tree).map(node_to_context))
}

fn find_focused_node(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(map) => {
            if map.get("focused").and_then(Value::as_bool) == Some(true) {
                return Some(value);
            }

            for key in ["nodes", "floating_nodes"] {
                if let Some(Value::Array(children)) = map.get(key) {
                    if let Some(found) = children.iter().find_map(find_focused_node) {
                        return Some(found);
                    }
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(find_focused_node),
        _ => None,
    }
}

fn node_to_context(node: &Value) -> AppContext {
    let pid = node
        .get("pid")
        .and_then(Value::as_u64)
        .map(|pid| pid as u32);
    let app_id = node
        .get("app_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let title = node.get("name").and_then(Value::as_str).map(str::to_owned);
    let class = node
        .get("window_properties")
        .and_then(|props| props.get("class"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let instance = node
        .get("window_properties")
        .and_then(|props| props.get("instance"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let container_id = node.get("id").map(|id| id.to_string());

    AppContext {
        compositor: Some("sway".to_string()),
        app_id,
        class,
        instance,
        title,
        pid,
        exe: None,
        cwd: None,
        container_id,
    }
}
