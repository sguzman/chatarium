//! User-owned MCP bootstrap and policy drafts. These settings never grant
//! execution rights: the current MCP runner still requires explicit Allow+Run.
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const FILE_NAME: &str = "mcp-agent-defaults.json";
const SCHEMA: &str = "chatarium-mcp-agent-defaults";
const MAX_PROMPT: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDefaults {
    pub bootstrap: String,
    pub policy_rules: String,
    pub max_calls: u32,
    pub seed_new_conversations: bool,
}

impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            bootstrap: concat!(
                "Chatarium provides locally controlled MCP tools. When tool invocation ",
                "is enabled by the runtime, request exactly one tool with a standalone ",
                "JSON object of this shape:\n",
                "{\"chatarium_mcp_call\":{\"provider\":\"PROVIDER\",\"tool\":\"OPERATION\",",
                "\"arguments\":{}}}\n",
                "Use chatarium.builtin::tools.list to discover available tools. ",
                "Do not invent provider names or execute tools yourself. ",
                "Chatarium makes all approval decisions and returns exact tool results. ",
                "When the current runtime does not support automatic continuation, wait for explicit user review before assuming the tool has completed."
            ).to_owned(),
            policy_rules: "chatarium.builtin::tools.list = automatic\nchatarium.builtin::hello = automatic\nchatarium.demo::shell.command = ask".to_owned(),
            max_calls: 8,
            seed_new_conversations: false,
        }
    }
}

impl AgentDefaults {
    pub fn validate(&self) -> Result<(), String> {
        if self.bootstrap.len() > MAX_PROMPT
            || self.bootstrap.contains('\0')
            || !(1..=32).contains(&self.max_calls)
        {
            return Err("MCP bootstrap or call limit is invalid".to_owned());
        }
        let mut unique = BTreeMap::new();
        for line in self
            .policy_rules
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            if line.len() > 256 {
                return Err("MCP policy line exceeds 256 bytes".to_owned());
            }
            let (key, value) = line
                .split_once('=')
                .ok_or("policy syntax is provider::tool = automatic|ask|manual|disabled")?;
            let key = key.trim();
            let mode = value.trim();
            if !key.contains("::")
                || key.split("::").count() != 2
                || key
                    .chars()
                    .any(|c| !c.is_ascii_alphanumeric() && !"._:-".contains(c))
                || !matches!(mode, "automatic" | "ask" | "manual" | "disabled")
                || unique.insert(key.to_owned(), mode).is_some()
            {
                return Err("invalid or duplicated MCP policy rule".to_owned());
            }
        }
        if unique.len() > 128 {
            return Err("too many MCP policy rules".to_owned());
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path).map_err(|e| format!("cannot read MCP defaults: {e}"))?;
        if bytes.len() > MAX_PROMPT + 65536 {
            return Err("MCP defaults file exceeds allowed size".to_owned());
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid MCP defaults JSON: {e}"))?;
        if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || value.get("version").and_then(Value::as_u64) != Some(1)
        {
            return Err("unknown MCP defaults format".to_owned());
        }
        let bootstrap = value
            .get("bootstrap")
            .and_then(Value::as_str)
            .ok_or("MCP bootstrap missing")?
            .to_owned();
        let policy_rules = value
            .get("policy_rules")
            .and_then(Value::as_str)
            .ok_or("MCP policy rules missing")?
            .to_owned();
        let max_calls = u32::try_from(
            value
                .get("max_calls")
                .and_then(Value::as_u64)
                .ok_or("MCP call limit missing")?,
        )
        .map_err(|e| e.to_string())?;
        let settings = Self {
            bootstrap,
            policy_rules,
            max_calls,
            seed_new_conversations: value.get("seed_new_conversations").and_then(Value::as_bool).unwrap_or(false),
        };
        settings.validate()?;
        Ok(settings)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": SCHEMA, "version": 1,
            "bootstrap": self.bootstrap,
            "policy_rules": self.policy_rules,
            "max_calls": self.max_calls,
            "seed_new_conversations": self.seed_new_conversations
        }))
        .map_err(|e| e.to_string())?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelToolRequest {
    pub provider: String,
    pub tool: String,
    pub arguments: Value,
}

/// Read-only parsing. Never grant permission to a model-authored envelope.
pub fn parse_model_tool_request(text: &str) -> Result<Option<ModelToolRequest>, String> {
    if text.len() > 65536 || !text.trim_start().starts_with('{') {
        return Ok(None);
    }
    let parsed: Value = match serde_json::from_str(text.trim()) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(None),
    };
    let Some(top) = parsed.as_object() else {
        return Ok(None);
    };
    let Some(payload) = top.get("chatarium_mcp_call") else {
        return Ok(None);
    };
    if top.len() != 1 {
        return Err("MCP envelope has extra fields".to_owned());
    }
    let args = payload.as_object().ok_or("MCP call must be object")?;
    if args.len() != 3 {
        return Err("MCP call must have provider, tool, arguments".to_owned());
    }
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .ok_or("MCP provider must be text")?;
    let tool = args
        .get("tool")
        .and_then(Value::as_str)
        .ok_or("MCP tool must be text")?;
    for part in [provider, tool] {
        if part.is_empty()
            || part.len() > 128
            || part
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && !"._-".contains(c))
        {
            return Err("MCP identifier has invalid characters".to_owned());
        }
    }
    let arguments = args.get("arguments").ok_or("MCP arguments absent")?;
    if !arguments.is_object() {
        return Err("MCP arguments must be JSON object".to_owned());
    }
    Ok(Some(ModelToolRequest {
        provider: provider.to_owned(),
        tool: tool.to_owned(),
        arguments: arguments.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_model_envelope_cannot_grant_permissions() {
        let input = r#"{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"tools.list","arguments":{}}}"#;
        let call = parse_model_tool_request(input).unwrap().unwrap();
        assert_eq!(call.tool, "tools.list");
        assert_eq!(call.arguments, json!({}));
        assert!(
            parse_model_tool_request("prose and then a tool call")
                .unwrap()
                .is_none()
        );
        assert!(
            parse_model_tool_request(
                r#"{"chatarium_mcp_call":{"provider":"x","tool":"y","arguments":[]}}"#
            )
            .is_err()
        );
        assert!(parse_model_tool_request(r#"{"chatarium_mcp_call":{"provider":"x","tool":"y","arguments":{}},"approval":true}"#).is_err());
    }

    #[test]
    fn persisted_template_and_policy_are_validated() {
        let mut settings = AgentDefaults::default();
        settings.seed_new_conversations = true;
        assert!(settings.validate().is_ok());
        let mut bad = settings.clone();
        bad.policy_rules
            .push_str("\nchatarium.builtin::hello = disabled");
        assert!(bad.validate().is_err());
        let path = std::env::temp_dir().join(format!("chatarium-mcp-{}.json", std::process::id()));
        settings.save_atomic(&path).unwrap();
        assert_eq!(settings, AgentDefaults::load(&path).unwrap());
        let _ = fs::remove_file(path);
    }
}
