//! User-owned MCP bootstrap and policy settings. Automatic execution is
//! restricted to exactly persisted authorizations and audited one-shot routes.
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const FILE_NAME: &str = "mcp-agent-defaults.json";
const SCHEMA: &str = "chatarium-mcp-agent-defaults";
// Old saved files recorded the former default-off state without distinguishing
// explicit user intent. Migrate once; subsequent saves preserve a real opt-out.
const ACTIVATION_DEFAULT_VERSION: u64 = 2;
// Upgrade only the exact previously generated shell-approval default, once.
// Saved explicit 'ask', 'manual' and 'disabled' policies remain authoritative.
const HOST_SHELL_DEFAULT_VERSION: u64 = 2;
const COORDINATION_DEFAULT_VERSION: u64 = 2;
// Opt-in request for this personal Chatarium workspace: fixed built-in HTTP
// providers run by default. Stamp once; preserve later per-tool user choices.
const LOCAL_HTTP_DEFAULT_VERSION: u64 = 2;
const MAX_PROMPT: usize = 32 * 1024;
const PRIOR_DEFAULT_POLICY: &str = "chatarium.builtin::tools.list = automatic\nchatarium.builtin::hello = automatic\nchatarium.demo::shell.command = ask";
const READ_ONLY_DEFAULT_POLICY: &str = "chatarium.builtin::tools.list = automatic\nchatarium.builtin::hello = automatic\nchatarium.demo::shell.command = ask\nchatarium.builtin::time.now = automatic";

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
                "When a tool is permitted and executed, Chatarium appends the exact audited result ",
                "to this same conversation turn and resumes you automatically. ",
                "Never claim to have called a tool unless its actual result appears in context. ",
                "If execution requires approval, wait for the user to approve it."
            ).to_owned(),
            policy_rules: append_missing_local_http_defaults(&format!(
                "{READ_ONLY_DEFAULT_POLICY}\nchatarium.builtin::shell.command = automatic\n                 chatarium.builtin::coordination.identity = automatic\n                 chatarium.builtin::coordination.send = automatic"
            )),
            max_calls: 8,
            seed_new_conversations: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    Automatic,
    Ask,
    Manual,
    Disabled,
}
impl ToolPolicy {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Ask => "ask",
            Self::Manual => "manual",
            Self::Disabled => "disabled",
        }
    }
}
/// Append explicit persisted automatic rules only for the two fixed,
/// code-owned localhost providers. Existing exact user rules take priority.
/// Never add a wildcard or authorize an unrecognized external stdio provider.
fn append_missing_local_http_defaults(rules: &str) -> String {
    let mut updated = rules.to_owned();
    for (provider, operation, _) in crate::local_http_mcp::registered_tools() {
        let target = format!("{provider}::{operation}");
        let already_set = rules
            .lines()
            .filter_map(|line| line.split_once('='))
            .any(|(name, _)| name.trim() == target);
        if !already_set {
            if !updated.is_empty() && !updated.ends_with('\n') {
                updated.push('\n');
            }
            updated.push_str(&format!("{target} = automatic"));
        }
    }
    updated
}

/// Policy is resolved by exact provider + operation names. Wildcards and
/// inferred permissions are forbidden; no matching rule means Ask.
pub fn tool_policy(rules: &str, provider: &str, operation: &str) -> Result<ToolPolicy, String> {
    let target = format!("{provider}::{operation}");
    let mut matching = None;
    for line in rules.lines().filter(|line| !line.trim().is_empty()) {
        let (key, value) = line.split_once('=').ok_or("invalid MCP policy line")?;
        if key.trim() != target {
            continue;
        }
        if matching.is_some() {
            return Err("duplicate MCP tool rule".to_owned());
        }
        matching = Some(match value.trim() {
            "automatic" => ToolPolicy::Automatic,
            "ask" => ToolPolicy::Ask,
            "manual" => ToolPolicy::Manual,
            "disabled" => ToolPolicy::Disabled,
            _ => return Err("invalid MCP execution mode".to_owned()),
        });
    }
    Ok(matching.unwrap_or(ToolPolicy::Ask))
}

/// Edit exactly one named policy without affecting unrelated permissions.
/// The result remains a draft until the user explicitly saves MCP defaults.
pub fn set_tool_policy_rule(
    rules: &str,
    provider: &str,
    operation: &str,
    selected: ToolPolicy,
) -> String {
    let target = format!("{provider}::{operation}");
    let replacement = format!("{target} = {}", selected.name());
    let mut found = false;
    let mut lines = Vec::new();
    for line in rules.lines() {
        if line
            .split_once('=')
            .is_some_and(|(name, _)| name.trim() == target)
        {
            if !found {
                lines.push(replacement.clone());
                found = true;
            }
        } else {
            lines.push(line.to_owned());
        }
    }
    if !found {
        lines.push(replacement);
    }
    lines.join("\n")
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
            // A fresh installation must have the exact built-in permissions on
            // disk before the persistence worker can authorize any automatic
            // MCP call. Never synthesize a grant for an existing settings file.
            let defaults = Self::default();
            defaults.save_atomic(path)?;
            return Ok(defaults);
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
        let mut settings = Self {
            bootstrap,
            policy_rules,
            max_calls,
            seed_new_conversations: value
                .get("seed_new_conversations")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        settings.validate()?;
        // On an existing installation, a generated shell-approval default was
        // the wrong choice for the requested automatic Fish shell. Upgrade
        // that exact old generated policy once. Never rewrite a customized
        // policy, including an explicitly selected Ask/Manual/Disabled.
        if value
            .get("host_shell_default_version")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            < HOST_SHELL_DEFAULT_VERSION
        {
            let old_shell_ask_default =
                format!("{READ_ONLY_DEFAULT_POLICY}\nchatarium.builtin::shell.command = ask");
            if settings.policy_rules == old_shell_ask_default {
                settings.policy_rules = Self::default().policy_rules;
            } else if settings.policy_rules == PRIOR_DEFAULT_POLICY
                || settings.policy_rules == READ_ONLY_DEFAULT_POLICY
            {
                // Older profiles with no host-shell permission must not gain
                // unrestricted execution merely from a software upgrade.
                settings.policy_rules = old_shell_ask_default;
            }
            settings.save_atomic(path)?;
        }
        // Only the exact previously generated shell default receives the new
        // coordination tool grants. Customized policies, including explicit
        // denials, are never silently escalated.
        if value
            .get("coordination_default_version")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            < COORDINATION_DEFAULT_VERSION
        {
            let previous_generated_default =
                format!("{READ_ONLY_DEFAULT_POLICY}\nchatarium.builtin::shell.command = automatic");
            if settings.policy_rules == previous_generated_default {
                settings.policy_rules = Self::default().policy_rules;
            }
            settings.save_atomic(path)?;
        }
        // Upgrade existing saved installations once, without overriding
        // any explicit Ask, Manual, Disabled or Automatic rule. The updated
        // policy is written to disk *before* an agent can run with it.
        if value
            .get("local_http_default_version")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            < LOCAL_HTTP_DEFAULT_VERSION
        {
            settings.policy_rules = append_missing_local_http_defaults(&settings.policy_rules);
            settings.save_atomic(path)?;
        }
        if value
            .get("activation_default_version")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            < ACTIVATION_DEFAULT_VERSION
        {
            // Version 1 saved the old default "off" even when nobody had
            // chosen an opt-out. Migrate that implicit state to a usable MCP
            // setup. Later explicit off choices survive via the version stamp.
            settings.seed_new_conversations = true;
            if settings
                .bootstrap
                .contains("When the current runtime does not support automatic continuation")
            {
                settings.bootstrap = Self::default().bootstrap;
            }
            settings.save_atomic(path)?;
        }
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
            "seed_new_conversations": self.seed_new_conversations,
            "activation_default_version": ACTIVATION_DEFAULT_VERSION,
            "host_shell_default_version": HOST_SHELL_DEFAULT_VERSION,
            "coordination_default_version": COORDINATION_DEFAULT_VERSION,
            "local_http_default_version": LOCAL_HTTP_DEFAULT_VERSION
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
    if text.len() > 65536 {
        return Ok(None);
    }
    // Prefer the documented JSON-first envelope. Some models instead emit
    // a sentence and append the tool JSON, sometimes with no whitespace.
    // Accept only one *terminal* envelope in that case; refuse examples,
    // code fences, extra JSON and fragments embedded in ordinary prose.
    let trimmed = text.trim_start();
    let candidate = if trimmed.starts_with('{') {
        trimmed
    } else {
        let marker = "\"chatarium_mcp_call\"";
        let mut occurrences = text.match_indices(marker);
        let Some((key_at, _)) = occurrences.next() else {
            return Ok(None);
        };
        if occurrences.next().is_some() {
            return Err("MCP response may request only one tool".to_owned());
        }
        let Some(object_at) = text[..key_at].rfind('{') else {
            return Ok(None);
        };
        // The MCP key must be the first field of the top-level envelope.
        if !text[object_at + 1..key_at].trim().is_empty() {
            return Ok(None);
        }
        let introduction = text[..object_at].trim_end();
        let lower = introduction.to_ascii_lowercase();
        let explanatory_example = [
            "example",
            "sample",
            "hypothetical",
            "illustrat",
            "e.g.",
            "such as",
        ]
        .iter()
        .any(|word| lower.contains(word));
        if introduction.is_empty()
            || !introduction
                .chars()
                .last()
                .is_some_and(|ch| matches!(ch, '.' | '!' | '?'))
            || introduction.chars().any(|ch| matches!(ch, '{' | '}' | '`'))
            || explanatory_example
        {
            return Ok(None);
        }
        let trailing = text[object_at..].trim();
        let mut first = serde_json::Deserializer::from_str(trailing).into_iter::<Value>();
        if !matches!(first.next(), Some(Ok(_))) {
            return Ok(None);
        }
        let remainder = &trailing[first.byte_offset()..];
        if !remainder.trim().is_empty() {
            // Observed live regression: the agent put a single tool envelope
            // between a brief intention and a *post-call waiting report*.
            // Do not treat arbitrary prose after embedded JSON as authority.
            // Accept only an explicit, newline-delimited "no result yet"
            // status, with no second JSON, braces, or fenced examples.
            let status = remainder.trim();
            let lower_status = status.to_ascii_lowercase();
            let waiting_for_result =
                (lower_status.contains("no tool result") && lower_status.contains("available"))
                    || (lower_status.contains("waiting for")
                        && lower_status.contains("tool result"));
            if !(remainder.starts_with('\n') || remainder.starts_with("\r\n"))
                || status.len() > 4096
                || status.contains(['{', '}'])
                || status.contains("```")
                || !waiting_for_result
            {
                return Ok(None);
            }
        }
        trailing
    };
    // Existing JSON-first behavior still supports a short explanation after
    // the tool. Prose-first requests must end with their single JSON object.
    let mut values = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
    let parsed = match values.next() {
        Some(Ok(parsed)) => parsed,
        _ => return Ok(None),
    };
    let suffix = &candidate[values.byte_offset()..];
    if !suffix.trim().is_empty() {
        let after_whitespace = suffix.trim_start();
        if after_whitespace.starts_with('{') {
            // A model can repeat the exact same envelope back-to-back (even
            // with no separator). One identical duplicate is *one* proposal,
            // never two executions. Reject differing or additional requests.
            // Prose-first responses still require one terminal envelope.
            if !trimmed.starts_with('{') || candidate != trimmed {
                return Err("MCP response may request only one tool".to_owned());
            }
            let mut duplicate_values =
                serde_json::Deserializer::from_str(after_whitespace).into_iter::<Value>();
            let duplicate = match duplicate_values.next() {
                Some(Ok(value)) => value,
                _ => return Err("MCP response contains malformed second tool JSON".to_owned()),
            };
            if !after_whitespace[duplicate_values.byte_offset()..]
                .trim()
                .is_empty()
            {
                return Err("MCP response may request only one tool".to_owned());
            }
            if duplicate != parsed {
                return Err("MCP response contains different tool requests".to_owned());
            }
        } else {
            // Continue accepting a plain explanation after JSON-first output.
            // An extra array or a second tool envelope is never interpreted
            // as permission to perform an additional operation.
            if after_whitespace.starts_with('[') {
                return Err("MCP response may request only one tool".to_owned());
            }
            let separated_by_newline = suffix.starts_with('\n') || suffix.starts_with("\r\n");
            let immediately_followed_by_prose =
                suffix.chars().next().is_some_and(char::is_alphabetic);
            // The remote model may append the literal " yield?" after a
            // complete MCP JSON envelope. This is a non-executable handoff
            // marker, not a second tool call. Accept only the exact token:
            // arbitrary prose after a space is still rejected.
            let explicit_yield_marker = after_whitespace.trim_end() == "yield?";
            if !separated_by_newline && !immediately_followed_by_prose && !explicit_yield_marker {
                return Ok(None);
            }
        }
    }
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
    fn leading_mcp_envelope_with_following_explanation_is_a_tool_request() {
        let source = concat!(
            "{\"chatarium_mcp_call\":{\"provider\":\"chatarium.builtin\",",
            "\"tool\":\"tools.list\",\"arguments\":{}}}\n",
            "I have requested tools.list and am waiting for its actual result."
        );
        let proposal = parse_model_tool_request(source).unwrap().unwrap();
        assert_eq!(proposal.provider, "chatarium.builtin");
        assert_eq!(proposal.tool, "tools.list");
        assert_eq!(proposal.arguments, json!({}));
        // Explanatory examples are not invocations, and inline garbage
        // cannot silently grant a tool proposal.
        assert!(
            parse_model_tool_request(&format!("Example: {source}"))
                .unwrap()
                .is_none()
        );
        assert!(parse_model_tool_request(
            "{\"chatarium_mcp_call\":{\"provider\":\"chatarium.builtin\",\"tool\":\"hello\",\"arguments\":{}}} extra"
        ).unwrap().is_none());
    }

    #[test]
    fn contiguous_prose_after_tool_envelope_still_invokes_one_tool() {
        // Regression: the agent once emitted this exact shape during a
        // git-status request. The caller displayed it as prose and never
        // journaled a tools.list call.
        let tool = r#"{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"tools.list","arguments":{}}}"#;
        let response =
            format!("{tool}I've requested the available tools so I can check Git status.");
        let call = parse_model_tool_request(&response).unwrap().unwrap();
        assert_eq!(call.provider, "chatarium.builtin");
        assert_eq!(call.tool, "tools.list");
        assert_eq!(call.arguments, json!({}));
        assert!(
            parse_model_tool_request(&format!("{tool} extra"))
                .unwrap()
                .is_none()
        );
        let duplicate = parse_model_tool_request(&format!("{tool}{tool}"))
            .unwrap()
            .unwrap();
        assert_eq!(duplicate.tool, "tools.list");
    }

    #[test]
    fn mcp_json_with_spaced_yield_marker_still_invokes_one_tool() {
        // Screenshot regression (2026-10-17): the model emitted the correct
        // tools.list JSON followed by " yield?". No purple MCP call appeared.
        let tool = r#"{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"tools.list","arguments":{}}}"#;
        for response in [
            format!("{tool} yield?"),
            format!("{tool} yield?   "),
            format!("{tool}\n yield?"),
        ] {
            let call = parse_model_tool_request(&response).unwrap().unwrap();
            assert_eq!(call.provider, "chatarium.builtin");
            assert_eq!(call.tool, "tools.list");
            assert_eq!(call.arguments, json!({}));
        }
        // Whitespace-prefixed arbitrary prose and extra tool instructions
        // remain non-executable.
        for response in [
            format!("{tool} extra"),
            format!("{tool} yield? and also execute code"),
            format!("{tool} `yield?`"),
        ] {
            assert!(parse_model_tool_request(&response).unwrap().is_none());
        }
    }

    #[test]
    fn sentence_before_terminal_tool_envelope_invokes_one_tool() {
        // Reproduce the user-observed output: the model concatenated ordinary
        // narration directly with the tool JSON, producing no tool event.
        let tool = r#"{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"tools.list","arguments":{}}}"#;
        let sentence = "I'll discover the available tools, then test creating a Mathematica session, evaluating code, running time and finance operations, listing sessions, and closing the session.";
        for response in [
            format!("{sentence}{tool}"),
            format!("{sentence}\n{tool}"),
            format!("{sentence}\n  {tool}\n"),
        ] {
            let call = parse_model_tool_request(&response).unwrap().unwrap();
            assert_eq!(call.provider, "chatarium.builtin");
            assert_eq!(call.tool, "tools.list");
            assert_eq!(call.arguments, json!({}));
        }
    }

    #[test]
    fn manifoldr_retest_request_embedded_between_intention_and_waiting_report() {
        // Live screenshot, Oct 17: the agent *said* it had issued discovery,
        // but no purple tool event appeared because prose followed the JSON.
        let tool = r#"{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"tools.list","arguments":{}}}"#;
        let intention = "I'll discover the tools and schemas, then run each read-only call in sequence. Chatarium controls permissions; if it requires approval, I'll wait rather than bypass it.";
        let waiting = "The discovery request has been issued, but no tool result is available in this conversation. I cannot verify schemas or proceed with dependent calls without that result.\n| Step | Status |\n| Discover Manifoldr tools | Pending tool result |\n| list_categories | Not run |";
        let answer = format!("{intention}\n{tool}\n{waiting}");
        let call = parse_model_tool_request(&answer).unwrap().unwrap();
        assert_eq!(call.provider, "chatarium.builtin");
        assert_eq!(call.tool, "tools.list");
        assert_eq!(call.arguments, json!({}));
    }

    #[test]
    fn embedded_tool_envelope_never_authorizes_general_prose_examples_or_second_payload() {
        let tool = r#"{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"tools.list","arguments":{}}}"#;
        let waiting = "The discovery request was issued, but no tool result is available.";
        for response in [
            format!("For example.\n{tool}\n{waiting}"),
            format!("I will discover the tools.\n```json\n{tool}\n```\n{waiting}"),
            format!("I will discover the tools.\n{tool}\nOperation completed successfully."),
            format!("I will discover the tools.\n{tool} {waiting}"),
            format!("I will discover the tools.\n{tool}\n{waiting} {{}}"),
            format!("I will discover the tools.\n{tool}\n{waiting}\n```"),
            format!("I will discover the tools.\n{tool}\n{waiting}\n{tool}"),
        ] {
            assert!(
                parse_model_tool_request(&response)
                    .map(|call| call.is_none())
                    .unwrap_or(true),
                "unsafe embedded tool invocation should not be dispatched: {response}"
            );
        }
    }

    #[test]
    fn trailing_tool_parser_refuses_examples_fences_and_multiple_calls() {
        let tool = r#"{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"tools.list","arguments":{}}}"#;
        for prefix in [
            "For example.",
            "This is a sample.",
            "A hypothetical call.",
            "Here is a code fence. ```json\n",
            "Quoted `chatarium_mcp_call` instruction.",
            "We have not decided to call it:",
        ] {
            assert!(
                parse_model_tool_request(&format!("{prefix}{tool}"))
                    .unwrap()
                    .is_none(),
                "should not dispatch an illustrated/quoted envelope: {prefix}"
            );
        }
        assert!(parse_model_tool_request(&format!("I will call the tool.{tool}{tool}")).is_err());
        assert!(
            parse_model_tool_request(&format!("I will call the tool.{tool} extra"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn identical_repeated_mcp_envelopes_are_deduplicated_as_one_proposal() {
        // Screenshot regression: the model accidentally emitted two copies
        // with no whitespace, so Chatarium previously produced no purple call.
        let one = r#"{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"tools.list","arguments":{}}}"#;
        for response in [
            format!("{one}{one}"),
            format!("{one}\n{one}"),
            format!("{one}\n  {one}\n"),
        ] {
            let request = parse_model_tool_request(&response).unwrap().unwrap();
            assert_eq!(request.provider, "chatarium.builtin");
            assert_eq!(request.tool, "tools.list");
            assert_eq!(request.arguments, json!({}));
        }
    }

    #[test]
    fn conflicting_or_additional_mcp_envelopes_are_never_queued() {
        let one = r#"{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"tools.list","arguments":{}}}"#;
        let different = r#"{"chatarium_mcp_call":{"provider":"chatarium.local.mathematica","tool":"mathematica_create_session","arguments":{}}}"#;
        assert!(parse_model_tool_request(&format!("{one}{different}")).is_err());
        assert!(parse_model_tool_request(&format!("{one}{one}{one}")).is_err());
        assert!(parse_model_tool_request(&format!("{one}{one} extra")).is_err());
        assert!(parse_model_tool_request(&format!("{one}{{")).is_err());
        assert!(parse_model_tool_request(&format!("{one}[1]")).is_err());
    }

    #[test]
    fn generated_shell_default_gains_coordination_without_escalating_custom_policy() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-coordination-permissions-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let defaults = AgentDefaults::default();
        let old =
            format!("{READ_ONLY_DEFAULT_POLICY}\nchatarium.builtin::shell.command = automatic");
        let file = json!({
            "schema": SCHEMA,
            "version": 1,
            "bootstrap": defaults.bootstrap,
            "policy_rules": old,
            "max_calls": 8,
            "seed_new_conversations": true,
            "activation_default_version": ACTIVATION_DEFAULT_VERSION,
            "host_shell_default_version": HOST_SHELL_DEFAULT_VERSION
        });
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let upgraded = AgentDefaults::load(&path).unwrap();
        assert_eq!(
            tool_policy(
                &upgraded.policy_rules,
                "chatarium.builtin",
                "coordination.send"
            ),
            Ok(ToolPolicy::Automatic)
        );
        let mut disabled = upgraded.clone();
        disabled.policy_rules = disabled.policy_rules.replace(
            "chatarium.builtin::coordination.send = automatic",
            "chatarium.builtin::coordination.send = disabled",
        );
        disabled.save_atomic(&path).unwrap();
        let reloaded = AgentDefaults::load(&path).unwrap();
        assert_eq!(reloaded.policy_rules, disabled.policy_rules);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn first_load_persists_exact_default_permissions_for_worker_replay() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-default-mcp-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        assert!(!path.exists());
        let loaded = AgentDefaults::load(&path).unwrap();
        assert!(loaded.seed_new_conversations);
        assert_eq!(
            tool_policy(&loaded.policy_rules, "chatarium.builtin", "tools.list"),
            Ok(ToolPolicy::Automatic)
        );
        assert_eq!(
            tool_policy(&loaded.policy_rules, "chatarium.builtin", "time.now"),
            Ok(ToolPolicy::Automatic)
        );
        assert_eq!(
            tool_policy(&loaded.policy_rules, "chatarium.builtin", "shell.command"),
            Ok(ToolPolicy::Automatic)
        );
        assert!(path.is_file());
        assert_eq!(loaded, AgentDefaults::load(&path).unwrap());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_saved_default_off_is_upgraded_and_user_opt_out_after_upgrade_survives() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-legacy-mcp-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let old = json!({
            "schema": SCHEMA,
            "version": 1,
            "bootstrap": "Use chatarium.builtin::tools.list. When the current runtime does not support automatic continuation, wait for explicit user review before assuming the tool has completed.",
            "policy_rules": AgentDefaults::default().policy_rules,
            "max_calls": 8,
            "seed_new_conversations": false,
        });
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let migrated = AgentDefaults::load(&path).unwrap();
        assert!(migrated.seed_new_conversations);
        assert!(migrated.bootstrap.contains("resumes you automatically"));
        assert_eq!(
            tool_policy(&migrated.policy_rules, "chatarium.builtin", "tools.list"),
            Ok(ToolPolicy::Automatic)
        );
        let on_disk: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk["activation_default_version"], 2);

        let mut opted_out = migrated.clone();
        opted_out.seed_new_conversations = false;
        opted_out.save_atomic(&path).unwrap();
        assert!(!AgentDefaults::load(&path).unwrap().seed_new_conversations);
        let _ = fs::remove_file(path);
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
    #[test]
    fn clock_policy_migrates_only_the_exact_previous_default() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-clock-migration-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let mut current = AgentDefaults::default();
        current.policy_rules = PRIOR_DEFAULT_POLICY.to_owned();
        // Emulate a real pre-migration file: the current save function
        // correctly stamps new writes as already migrated.
        let legacy = json!({
            "schema": SCHEMA,
            "version": 1,
            "bootstrap": current.bootstrap,
            "policy_rules": current.policy_rules,
            "max_calls": current.max_calls,
            "seed_new_conversations": current.seed_new_conversations,
            "activation_default_version": ACTIVATION_DEFAULT_VERSION,
        });
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let migrated = AgentDefaults::load(&path).unwrap();
        assert_eq!(
            tool_policy(&migrated.policy_rules, "chatarium.builtin", "time.now"),
            Ok(ToolPolicy::Automatic)
        );
        current
            .policy_rules
            .push_str("\nchatarium.demo::other = ask");
        current.save_atomic(&path).unwrap();
        let unchanged = AgentDefaults::load(&path).unwrap();
        assert_eq!(unchanged.policy_rules, current.policy_rules);
        assert_eq!(
            tool_policy(&unchanged.policy_rules, "chatarium.builtin", "time.now"),
            Ok(ToolPolicy::Ask)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn previous_generated_shell_ask_upgrades_once_but_opt_out_persists() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-host-shell-migration-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let old_policy =
            format!("{READ_ONLY_DEFAULT_POLICY}\nchatarium.builtin::shell.command = ask");
        let old = json!({
            "schema": SCHEMA,
            "version": 1,
            "bootstrap": AgentDefaults::default().bootstrap,
            "policy_rules": old_policy,
            "max_calls": 8,
            "seed_new_conversations": true,
            "activation_default_version": ACTIVATION_DEFAULT_VERSION,
        });
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

        let migrated = AgentDefaults::load(&path).unwrap();
        assert_eq!(
            tool_policy(&migrated.policy_rules, "chatarium.builtin", "shell.command"),
            Ok(ToolPolicy::Automatic)
        );
        let serialized: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            serialized["host_shell_default_version"],
            HOST_SHELL_DEFAULT_VERSION
        );
        assert_eq!(migrated, AgentDefaults::load(&path).unwrap());

        let mut explicitly_asking = migrated.clone();
        explicitly_asking.policy_rules = old_policy;
        explicitly_asking.save_atomic(&path).unwrap();
        let reloaded = AgentDefaults::load(&path).unwrap();
        assert_eq!(reloaded.policy_rules, explicitly_asking.policy_rules);
        assert_eq!(
            tool_policy(&reloaded.policy_rules, "chatarium.builtin", "shell.command"),
            Ok(ToolPolicy::Ask)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn customized_legacy_shell_policy_is_never_escalated() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-host-shell-custom-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let policy = format!("{READ_ONLY_DEFAULT_POLICY}\nchatarium.builtin::shell.command = ask")
            .replace(
                "chatarium.demo::shell.command = ask",
                "chatarium.demo::shell.command = disabled",
            );
        let old = json!({
            "schema": SCHEMA,
            "version": 1,
            "bootstrap": AgentDefaults::default().bootstrap,
            "policy_rules": policy,
            "max_calls": 8,
            "seed_new_conversations": true,
            "activation_default_version": ACTIVATION_DEFAULT_VERSION,
        });
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let migrated = AgentDefaults::load(&path).unwrap();
        assert!(migrated.policy_rules.starts_with(&policy));
        assert_eq!(
            tool_policy(&migrated.policy_rules, "chatarium.builtin", "shell.command"),
            Ok(ToolPolicy::Ask)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn fixed_http_tools_are_automatically_authorized_by_fresh_persisted_defaults() {
        let defaults = AgentDefaults::default();
        defaults.validate().unwrap();
        for (provider, operation, _) in crate::local_http_mcp::registered_tools() {
            assert_eq!(
                tool_policy(&defaults.policy_rules, provider, operation),
                Ok(ToolPolicy::Automatic),
                "{provider}::{operation} must be explicitly saved as automatic"
            );
        }
        assert_eq!(
            tool_policy(&defaults.policy_rules, "external.unknown", "execute"),
            Ok(ToolPolicy::Ask)
        );
        assert_eq!(
            tool_policy(
                &defaults.policy_rules,
                crate::local_http_mcp::MATHEMATICA,
                "unregistered_operation"
            ),
            Ok(ToolPolicy::Ask)
        );
    }

    #[test]
    fn policy_dropdown_changes_only_selected_named_tool() {
        let original = AgentDefaults::default().policy_rules;
        let updated = set_tool_policy_rule(
            &original,
            crate::local_http_mcp::MATHEMATICA,
            "mathematica_execute_code",
            ToolPolicy::Ask,
        );
        assert_eq!(
            tool_policy(
                &updated,
                crate::local_http_mcp::MATHEMATICA,
                "mathematica_execute_code"
            ),
            Ok(ToolPolicy::Ask)
        );
        assert_eq!(
            tool_policy(
                &updated,
                crate::local_http_mcp::MATHEMATICA,
                "mathematica_close_session"
            ),
            Ok(ToolPolicy::Automatic)
        );
        assert_eq!(
            tool_policy(&updated, crate::local_http_mcp::MANIFOLDR, "search_markets"),
            Ok(ToolPolicy::Automatic)
        );
        let restored = set_tool_policy_rule(
            &updated,
            crate::local_http_mcp::MATHEMATICA,
            "mathematica_execute_code",
            ToolPolicy::Automatic,
        );
        assert_eq!(restored, original);
        let mut settings = AgentDefaults::default();
        settings.policy_rules = updated;
        settings.validate().unwrap();
    }

    #[test]
    fn fixed_http_policy_migration_preserves_existing_choices_and_new_opt_out() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-http-auto-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let old_rules = format!(
            "{READ_ONLY_DEFAULT_POLICY}\n\
             chatarium.local.mathematica::mathematica_execute_code = ask\n\
             chatarium.local.mathematica::mathematica_close_session = disabled\n\
             chatarium.local.manifoldr::get_market = manual\n\
             third.party::operation = ask"
        );
        let legacy = json!({
            "schema": SCHEMA,
            "version": 1,
            "bootstrap": AgentDefaults::default().bootstrap,
            "policy_rules": old_rules,
            "max_calls": 8,
            "seed_new_conversations": true,
            "activation_default_version": ACTIVATION_DEFAULT_VERSION,
            "host_shell_default_version": HOST_SHELL_DEFAULT_VERSION,
            "coordination_default_version": COORDINATION_DEFAULT_VERSION
        });
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let upgraded = AgentDefaults::load(&path).unwrap();
        assert_eq!(
            tool_policy(
                &upgraded.policy_rules,
                crate::local_http_mcp::MATHEMATICA,
                "mathematica_execute_code"
            ),
            Ok(ToolPolicy::Ask)
        );
        assert_eq!(
            tool_policy(
                &upgraded.policy_rules,
                crate::local_http_mcp::MATHEMATICA,
                "mathematica_close_session"
            ),
            Ok(ToolPolicy::Disabled)
        );
        assert_eq!(
            tool_policy(
                &upgraded.policy_rules,
                crate::local_http_mcp::MANIFOLDR,
                "get_market"
            ),
            Ok(ToolPolicy::Manual)
        );
        assert_eq!(
            tool_policy(&upgraded.policy_rules, "third.party", "operation"),
            Ok(ToolPolicy::Ask)
        );
        assert_eq!(
            tool_policy(
                &upgraded.policy_rules,
                crate::local_http_mcp::MATHEMATICA,
                "mathematica_create_session"
            ),
            Ok(ToolPolicy::Automatic)
        );
        assert_eq!(upgraded, AgentDefaults::load(&path).unwrap());
        let disk: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            disk["local_http_default_version"],
            LOCAL_HTTP_DEFAULT_VERSION
        );

        // User changes after migration persist across every later restart.
        let mut opt_out = upgraded.clone();
        opt_out.policy_rules = opt_out.policy_rules.replace(
            "chatarium.local.mathematica::mathematica_create_session = automatic",
            "chatarium.local.mathematica::mathematica_create_session = disabled",
        );
        opt_out.save_atomic(&path).unwrap();
        let reloaded = AgentDefaults::load(&path).unwrap();
        assert_eq!(reloaded, opt_out);
        assert_eq!(
            tool_policy(
                &reloaded.policy_rules,
                crate::local_http_mcp::MATHEMATICA,
                "mathematica_create_session"
            ),
            Ok(ToolPolicy::Disabled)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn explicit_policies_fail_closed_without_wildcards() {
        let rules = "demo::hello = automatic\ndemo::shell.command = ask\ndemo::blocked = disabled";
        assert_eq!(
            tool_policy(rules, "demo", "hello"),
            Ok(ToolPolicy::Automatic)
        );
        assert_eq!(
            tool_policy(rules, "demo", "shell.command"),
            Ok(ToolPolicy::Ask)
        );
        assert_eq!(
            tool_policy(rules, "demo", "blocked"),
            Ok(ToolPolicy::Disabled)
        );
        assert_eq!(tool_policy(rules, "other", "hello"), Ok(ToolPolicy::Ask));
        assert_eq!(tool_policy(rules, "demo", "hello2"), Ok(ToolPolicy::Ask));
        assert!(
            tool_policy(
                "demo::hello = automatic\ndemo::hello = disabled",
                "demo",
                "hello"
            )
            .is_err()
        );
    }
}
