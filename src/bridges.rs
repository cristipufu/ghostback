//! Agent-runtime bridges and comment-preserving OpenCode configuration edits.
use crate::{Result, invalid};
use jsonc_parser::cst::{CstInputValue, CstRootNode};
use std::path::{Path, PathBuf};

pub fn source(agent: &str, binary: &Path, state: &Path) -> Result<Vec<u8>> {
    let adapter = match agent {
        "pi" => include_str!("../integrations/pi.js"),
        "opencode" => include_str!("../integrations/opencode.js"),
        _ => return Err(invalid("unknown runtime bridge")),
    };
    let config = serde_json::to_string(&serde_json::json!({"binary": binary, "state": state}))?;
    Ok(format!(
        "{}\n{adapter}",
        include_str!("../integrations/bridge.js").replace("__GHOSTBACK_CONFIG__", &config)
    )
    .into_bytes())
}

pub fn roots(home: &Path, use_environment: bool) -> (PathBuf, PathBuf) {
    let env = |key: &str| {
        use_environment
            .then(|| std::env::var_os(key))
            .flatten()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    };
    let pi = env("PI_CODING_AGENT_DIR").unwrap_or_else(|| home.join(".pi/agent"));
    let opencode = env("OPENCODE_CONFIG_DIR").unwrap_or_else(|| {
        env("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home.join(".config"))
            .join("opencode")
    });
    (pi, opencode)
}

pub fn file_url(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .filter(|_| path.is_absolute())
        .ok_or_else(|| invalid("plugin path must be absolute UTF-8"))?;
    let mut result = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            result.push(char::from(b));
        } else {
            result.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(result)
}

pub fn merge_plugin(input: &str, specifier: &str, remove: bool) -> Result<String> {
    let root = CstRootNode::parse(
        if input.trim().is_empty() {
            "{}\n"
        } else {
            input
        },
        &Default::default(),
    )
    .map_err(|e| invalid(e.to_string()))?;
    let object = root
        .object_value()
        .ok_or_else(|| invalid("OpenCode TUI configuration must be an object"))?;
    if object
        .properties()
        .iter()
        .filter(|p| p.name().and_then(|n| n.decoded_value().ok()).as_deref() == Some("plugin"))
        .count()
        > 1
    {
        return Err(invalid(
            "OpenCode TUI configuration has duplicate plugin keys",
        ));
    }
    if remove && object.get("plugin").is_none() {
        return Ok(input.to_owned());
    }
    let plugins = object
        .array_value_or_create("plugin")
        .ok_or_else(|| invalid("OpenCode TUI plugin setting must be an array"))?;
    let mut present = false;
    for entry in plugins.elements() {
        if entry
            .as_string_lit()
            .and_then(|v| v.decoded_value().ok())
            .as_deref()
            == Some(specifier)
        {
            present = true;
            if remove {
                entry.remove();
            }
        }
    }
    if !remove && !present {
        plugins.append(CstInputValue::String(specifier.into()));
    }
    Ok(root.to_string())
}
