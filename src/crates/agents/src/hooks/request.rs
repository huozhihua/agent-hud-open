use super::{display, hooks_by_id, hooks_of, session_id, Permission, Reply};
use crate::{iso, text, trimmed, validate_payload};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Cap retained Always rules so a long-lived HUD process cannot grow forever.
const WORKSPACE_ALWAYS_LIMIT: usize = 500;

type AlwaysPersister = Arc<dyn Fn(Vec<WorkspaceAlwaysRule>) -> Result<(), String> + Send + Sync>;

/// Workspace-scoped Always rules. Checked in the HUD process before holding a
/// ticket. Shell patterns are command prefixes (Cursor `terminalAllowlist`),
/// not a wildcard for every command.
static WORKSPACE_ALWAYS: Mutex<Vec<WorkspaceAlwaysRule>> = Mutex::new(Vec::new());
static ALWAYS_PERSISTER: Mutex<Option<AlwaysPersister>> = Mutex::new(None);

/// One HUD-side Always rule: a workspace root, a tool, and what that tool may
/// repeat (a shell prefix, a file path, or one `server:tool`).
///
/// `pattern` is never a silent whole-tool `*`. A missing pattern loads as
/// empty and matches nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAlwaysRule {
    pub workspace: String,
    pub tool_name: String,
    #[serde(default)]
    pub pattern: String,
}

/// Replace the in-memory Always table (used when loading from disk).
///
/// @param rules Rules restored from the host store
pub fn load_workspace_always(rules: Vec<WorkspaceAlwaysRule>) {
    if let Ok(mut slot) = WORKSPACE_ALWAYS.lock() {
        *slot = rules
            .into_iter()
            .filter(|rule| !rule.workspace.is_empty() && !rule.tool_name.is_empty())
            .collect();
        trim_workspace_always(&mut slot);
    }
}

/// Snapshot of the current Always table for host persistence.
///
/// @return Current rules in insertion order
pub fn workspace_always_snapshot() -> Vec<WorkspaceAlwaysRule> {
    WORKSPACE_ALWAYS
        .lock()
        .map(|rules| rules.clone())
        .unwrap_or_default()
}

/// Register a host callback invoked after Always rules change.
///
/// The callback must report write failures. A failed write does not update
/// the in-memory table, so a reload cannot forget a trust the card already
/// granted.
///
/// @param persister Writes the snapshot to durable storage; None clears it
pub fn set_workspace_always_persister(persister: Option<AlwaysPersister>) {
    if let Ok(mut slot) = ALWAYS_PERSISTER.lock() {
        *slot = persister;
    }
}

fn trim_workspace_always(rules: &mut Vec<WorkspaceAlwaysRule>) {
    if rules.len() > WORKSPACE_ALWAYS_LIMIT {
        let drop = rules.len() - WORKSPACE_ALWAYS_LIMIT;
        rules.drain(0..drop);
    }
}

fn persist_workspace_always(rules: &[WorkspaceAlwaysRule]) -> Result<(), String> {
    let slot = ALWAYS_PERSISTER
        .lock()
        .map_err(|_| "Always rules are unavailable".to_string())?;
    if let Some(persister) = slot.as_ref() {
        persister(rules.to_vec())
    } else {
        Ok(())
    }
}

/// Normalize a workspace or file path for Always keys (absolute, no trailing slash).
///
/// Canonicalizes when the path exists so a symlinked root matches across
/// sessions. A missing path keeps its absolute form.
///
/// @param raw Path from `workspace_roots`, `cwd`, or a file gate
/// @return Stable comparison key, or None when empty
fn normalize_workspace(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let resolved = absolute.canonicalize().unwrap_or(absolute);
    let mut key = resolved.to_string_lossy().into_owned();
    while key.len() > 1 && (key.ends_with('/') || key.ends_with('\\')) {
        key.pop();
    }
    (!key.is_empty()).then_some(key)
}

fn path_contains(root: &str, path: &str) -> bool {
    Path::new(path).starts_with(Path::new(root))
}

/// Roots a Cursor gate should be matched against, and the primary key shown
/// on the request.
///
/// Every `workspace_roots` entry counts, not only `[0]`. When `cwd` sits
/// inside one of them, the stored rule uses the longest containing root.
struct WorkspaceScope {
    primary: Option<String>,
    /// Roots an existing rule may hit: every listed root, or `cwd` when the
    /// payload has no roots.
    match_keys: Vec<String>,
}

fn workspace_scope(payload: &Value) -> WorkspaceScope {
    let mut roots = Vec::new();
    if let Some(list) = payload["workspace_roots"].as_array() {
        for root in list {
            if let Some(key) = text(root).and_then(normalize_workspace) {
                if !roots.contains(&key) {
                    roots.push(key);
                }
            }
        }
    }
    let cwd = text(&payload["cwd"]).and_then(normalize_workspace);
    let containing = cwd.as_ref().and_then(|cwd| {
        roots
            .iter()
            .filter(|root| path_contains(root, cwd))
            .max_by_key(|root| root.len())
            .cloned()
    });
    let mut match_keys = roots;
    if match_keys.is_empty() {
        if let Some(cwd) = cwd {
            match_keys.push(cwd);
        }
    }
    let primary = containing.or_else(|| match_keys.first().cloned());
    WorkspaceScope {
        primary,
        match_keys,
    }
}

/// Where a new rule is written. Prefer the longest root that contains `cwd`
/// so a multi-root window does not stamp the rule onto every folder. When
/// `cwd` picks nothing, keep every match key (there is no single repo).
fn store_keys(request: &Request) -> Vec<String> {
    let keys = if request.workspace_keys.is_empty() {
        request.workspace.clone().into_iter().collect::<Vec<_>>()
    } else {
        request.workspace_keys.clone()
    };
    if let Some(cwd) = request.cwd.as_deref().and_then(normalize_workspace) {
        if let Some(best) = keys
            .iter()
            .filter(|key| path_contains(key, &cwd))
            .max_by_key(|key| key.len())
            .cloned()
        {
            return vec![best];
        }
    }
    keys
}

fn normalize_scope_path(raw: &str, cwd: Option<&str>) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        return normalize_workspace(trimmed);
    }
    let base = cwd.and_then(normalize_workspace)?;
    normalize_workspace(&Path::new(&base).join(path).to_string_lossy())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub source: String,
    pub session_id: String,
    pub kind: String,
    pub turn_id: Option<String>,
    /// Codex's policy is insufficient evidence: the host must independently
    /// confirm this exact turn has approvals_reviewer == user before holding it.
    pub requires_user_reviewer: bool,
    tool_name: Option<String>,
    summary: String,
    detail: Option<String>,
    cwd: Option<String>,
    /// Workspace a new Always rule is keyed to (longest root that contains
    /// `cwd`, else the first listed root). None → Cursor Always unavailable.
    workspace: Option<String>,
    /// Every workspace root this gate listed, or `cwd` when it listed none.
    /// A rule matches when its workspace is any of these. Not part of the
    /// serde view: live requests keep it in memory.
    #[serde(default, skip)]
    workspace_keys: Vec<String>,
    path: Option<String>,
    removed: Option<String>,
    added: Option<String>,
    suggestions: Vec<Value>,
    questions: Vec<Question>,
    answer_input: Option<Value>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Question {
    id: Option<String>,
    question: String,
    header: Option<String>,
    options: Vec<QuestionOption>,
    multi_select: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct QuestionOption {
    label: String,
    description: Option<String>,
}

fn tool_kind(tool: &str) -> &str {
    match tool {
        "Shell" | "run_command" | "run_shell_command" | "exec" => "Bash",
        "edit" | "replace" => "Edit",
        "write_file" | "write" => "Write",
        "read_file" | "read" => "Read",
        "glob" => "Glob",
        "grep_search" | "search_file_content" | "grep" => "Grep",
        "web_fetch" | "webfetch" => "WebFetch",
        "web_search" => "WebSearch",
        "notebook_edit" => "NotebookEdit",
        "agent" => "Agent",
        "task" => "Task",
        _ => tool,
    }
}

/// Normalize Cursor gate payloads into the Claude-shaped fields `parse_request` reads.
fn normalize_cursor_payload(payload: &Value) -> Value {
    let mut fields = payload.as_object().cloned().unwrap_or_default();
    if let Some(id) = text(&payload["conversation_id"]) {
        fields.entry("session_id").or_insert_with(|| json!(id));
    }
    let event = payload["hook_event_name"].as_str().unwrap_or("");
    match event {
        "beforeShellExecution" => {
            fields.insert("tool_name".into(), json!("Shell"));
            let mut input = Map::new();
            if let Some(command) = text(&payload["command"]) {
                input.insert("command".into(), json!(command));
            }
            if let Some(description) = text(&payload["description"]) {
                input.insert("description".into(), json!(description));
            }
            fields.insert("tool_input".into(), Value::Object(input));
        }
        "beforeMCPExecution" => {
            let server = text(&payload["mcp_server_name"]).unwrap_or("mcp");
            let tool = text(&payload["tool_name"]).unwrap_or("tool");
            let name = if tool.starts_with("MCP:") {
                tool.to_owned()
            } else {
                format!("MCP:{server}/{tool}")
            };
            fields.insert("tool_name".into(), json!(name));
            if !fields.contains_key("tool_input") {
                fields.insert("tool_input".into(), json!({}));
            } else if let Some(raw) = fields.get("tool_input").and_then(Value::as_str) {
                fields.insert("tool_input".into(), json!({"arguments": raw}));
            }
        }
        "beforeReadFile" => {
            fields.insert("tool_name".into(), json!("Read"));
            let mut input = Map::new();
            if let Some(path) = text(&payload["file_path"]).or_else(|| text(&payload["path"])) {
                input.insert("file_path".into(), json!(path));
            }
            fields.insert("tool_input".into(), Value::Object(input));
        }
        "subagentStart" => {
            fields.insert("tool_name".into(), json!("Task"));
            let mut input = Map::new();
            if let Some(task) = text(&payload["task"]) {
                input.insert("description".into(), json!(task));
            }
            fields.insert("tool_input".into(), Value::Object(input));
        }
        _ => {}
    }
    Value::Object(fields)
}

/// Build an Always suggestion when a non-Cursor client offers no
/// `permission_suggestions`. Those clients still receive the suggestion in
/// `updatedPermissions`; `destination` stays `session` for them.
fn synthetic_always(tool: &str, input: &Value) -> Value {
    let content = match tool {
        "Bash" | "Shell" => trimmed(&input["command"]),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => file_path(input),
        "Task" | "Agent" => trimmed(&input["description"]),
        _ => None,
    }
    .unwrap_or_else(|| "*".into());
    json!({
        "type": "addRules",
        "behavior": "allow",
        "destination": "session",
        "rules": [{
            "toolName": tool,
            "ruleContent": content
        }]
    })
}

/// Cursor Always suggestion. `destination` is `workspace` because the rule is
/// saved in HUD's per-workspace store, not a session and not Cursor's
/// `terminalAllowlist` / `permissions.json`.
///
/// Shell content is a command prefix (`git`, `git status`), not `*`.
/// File tools use a normalized path. MCP uses `server:tool`. No safe scope
/// means no suggestion, so the card does not offer a silent whole-tool trust.
fn synthetic_cursor_always(tool: &str, input: &Value, cwd: Option<&str>) -> Option<Value> {
    let content = cursor_rule_content(tool, input, cwd)?;
    let tool_name = if tool.starts_with("MCP:") {
        "MCP"
    } else {
        tool
    };
    Some(json!({
        "type": "addRules",
        "behavior": "allow",
        "destination": "workspace",
        "rules": [{
            "toolName": tool_name,
            "ruleContent": content
        }]
    }))
}

fn cursor_rule_content(tool: &str, input: &Value, cwd: Option<&str>) -> Option<String> {
    let content = match tool {
        "Bash" | "Shell" => shell_prefix(trimmed(&input["command"])?.as_str())?,
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "Delete" => {
            normalize_scope_path(&scope_path(input)?, cwd)?
        }
        "Glob" | "Grep" => {
            let path = trimmed(&input["path"]).or_else(|| trimmed(&input["target_directory"]))?;
            normalize_scope_path(&path, cwd)?
        }
        "WebFetch" => trimmed(&input["url"])?,
        _ if tool.starts_with("MCP:") => mcp_pattern(tool)?,
        _ => return None,
    };
    (content != "*" && !content.is_empty()).then_some(content)
}

fn scope_path(input: &Value) -> Option<String> {
    file_path(input)
        .or_else(|| trimmed(&input["path"]))
        .or_else(|| trimmed(&input["target_file"]))
}

/// `MCP:server/tool` → Cursor's `server:tool`. Names that contain `:` cannot
/// be written without looking like a wildcard, so Always is not offered.
fn mcp_pattern(tool: &str) -> Option<String> {
    let rest = tool.strip_prefix("MCP:")?;
    let (server, name) = rest.split_once('/')?;
    if server.is_empty()
        || name.is_empty()
        || server.contains(':')
        || name.contains(':')
        || server.contains('*')
        || name.contains('*')
    {
        return None;
    }
    Some(format!("{server}:{name}"))
}

/// Command prefix stored for Shell Always.
///
/// Normalization (simple commands only):
/// 1. Reject unquoted shell control (`&&`, `||`, `|`, `;`, `&`, redirects,
///    newlines, backticks, `$()`). Those gates get no Always rule.
/// 2. Drop leading `NAME=value` assignments.
/// 3. Keep the executable token as written (`git`, `/usr/bin/git`).
/// 4. Append the next token when it is a subcommand word (`git status`,
///    `npm install`), not a flag (`-`) and not a path or glob.
/// 5. Leave later arguments off the prefix.
///
/// Matching is case-sensitive and stops at a token boundary, same as Cursor
/// `terminalAllowlist`: `git` matches `git status` but not `gitk`; `git status`
/// matches `git status --short`. A pattern `base:glob` such as `npm:install*`
/// matches that executable whose arguments match the glob. A pattern ending in
/// `:*` (Claude's `npm test:*`) is the boundary prefix of the stem.
/// Compound commands are not auto-allowed, which is stricter than Cursor's
/// raw string prefix (`git status` would otherwise allow `git status && rm`).
fn shell_prefix(command: &str) -> Option<String> {
    let tokens = simple_command_tokens(command)?;
    let rest = skip_env(&tokens);
    let binary = rest.first()?.as_str();
    if binary.is_empty() {
        return None;
    }
    if let Some(second) = rest.get(1) {
        if is_subcommand(second) {
            return Some(format!("{binary} {second}"));
        }
    }
    Some(binary.to_owned())
}

fn is_subcommand(token: &str) -> bool {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if matches!(first, '-' | '/' | '\\' | '.' | '~') {
        return false;
    }
    !token
        .chars()
        .any(|c| matches!(c, '/' | '\\' | '*' | '?' | '.' | '='))
}

fn skip_env(tokens: &[String]) -> &[String] {
    let mut rest = tokens;
    while rest.first().is_some_and(|token| is_env_assignment(token)) {
        rest = &rest[1..];
    }
    rest
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.is_empty()
}

/// Tokens of one simple command. None when the line is compound or the quotes
/// do not close — Always will not guess a prefix for those.
fn simple_command_tokens(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if q == '"' && c == '\\' {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            } else {
                current.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            '&' | '|' | ';' | '<' | '>' | '(' | ')' | '`' | '\n' => return None,
            '$' if chars.peek() == Some(&'(') => return None,
            _ => current.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn command_body(command: &str) -> Option<String> {
    let tokens = simple_command_tokens(command)?;
    let rest = skip_env(&tokens);
    (!rest.is_empty()).then(|| rest.join(" "))
}

fn shell_matches(command: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern == "*" {
        return false;
    }
    let Some(body) = command_body(command) else {
        return false;
    };
    if let Some(stem) = pattern.strip_suffix(":*") {
        return !stem.is_empty() && boundary_prefix(&body, stem.trim());
    }
    if let Some((base, glob)) = split_colon_glob(pattern) {
        return colon_matches(&body, base, glob);
    }
    boundary_prefix(&body, pattern)
}

fn boundary_prefix(command: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    if command == prefix {
        return true;
    }
    command.starts_with(prefix)
        && command[prefix.len()..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
}

fn split_colon_glob(pattern: &str) -> Option<(&str, &str)> {
    let (base, glob) = pattern.split_once(':')?;
    if base.is_empty() || base.chars().any(char::is_whitespace) {
        return None;
    }
    (glob.contains('*') || glob.contains('?')).then_some((base, glob))
}

fn colon_matches(body: &str, base: &str, glob: &str) -> bool {
    let mut parts = body.split_whitespace();
    let Some(command) = parts.next() else {
        return false;
    };
    if command != base {
        return false;
    }
    let args = parts.collect::<Vec<_>>().join(" ");
    glob_match(glob, &args)
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == b'?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            let next = st + 1;
            star = Some((sp, next));
            pi = sp + 1;
            ti = next;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn usable_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() || pattern == "*" || pattern.contains('\n') {
        return Err("Always needs a command prefix, file path, or one MCP tool".into());
    }
    Ok(())
}

fn pattern_from_suggestion(suggestion: &Value) -> Result<String, String> {
    let content = suggestion["rules"]
        .as_array()
        .and_then(|rules| rules.first())
        .and_then(|rule| text(&rule["ruleContent"]))
        .ok_or("Always needs a command prefix, file path, or one MCP tool")?;
    usable_pattern(content)?;
    Ok(content.to_owned())
}

fn remember_workspace_always(request: &Request, suggestion: &Value) -> Result<(), String> {
    let keys = store_keys(request);
    if keys.is_empty() {
        return Ok(());
    }
    let pattern = pattern_from_suggestion(suggestion)?;
    let tool = request
        .tool_name
        .as_deref()
        .map(tool_kind)
        .unwrap_or("")
        .to_owned();
    if tool.is_empty() {
        return Err("Always needs a tool name".into());
    }
    let mut rules = WORKSPACE_ALWAYS
        .lock()
        .map_err(|_| "Always rules are unavailable".to_string())?;
    let mut next = rules.clone();
    let before = next.len();
    for workspace in keys {
        let entry = WorkspaceAlwaysRule {
            workspace,
            tool_name: tool.clone(),
            pattern: pattern.clone(),
        };
        if !next.iter().any(|rule| {
            rule.workspace == entry.workspace
                && rule.tool_name == entry.tool_name
                && rule.pattern == entry.pattern
        }) {
            next.push(entry);
        }
    }
    if next.len() == before {
        return Ok(());
    }
    trim_workspace_always(&mut next);
    persist_workspace_always(&next)
        .map_err(|error| format!("Could not save the workspace Always rule: {error}"))?;
    *rules = next;
    Ok(())
}

fn workspace_always_matches(request: &Request) -> bool {
    let tool = request.tool_name.as_deref().map(tool_kind).unwrap_or("");
    if tool.is_empty() {
        return false;
    }
    let keys = if request.workspace_keys.is_empty() {
        request.workspace.clone().into_iter().collect::<Vec<_>>()
    } else {
        request.workspace_keys.clone()
    };
    if keys.is_empty() {
        return false;
    }
    let Ok(rules) = WORKSPACE_ALWAYS.lock() else {
        return false;
    };
    rules.iter().any(|rule| {
        keys.iter().any(|key| key == &rule.workspace)
            && (rule.tool_name == tool
                || rule.tool_name == request.tool_name.as_deref().unwrap_or(""))
            && subject_matches(request, tool, &rule.pattern)
    })
}

fn subject_matches(request: &Request, tool: &str, pattern: &str) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return false;
    }
    match tool {
        "Bash" => request
            .detail
            .as_deref()
            .is_some_and(|command| shell_matches(command, pattern)),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "Delete" | "Glob" | "Grep" => {
            request.path.as_deref().is_some_and(|path| {
                normalize_scope_path(path, request.cwd.as_deref()).as_deref() == Some(pattern)
            })
        }
        "WebFetch" => request.detail.as_deref().map(str::trim) == Some(pattern),
        _ if tool.starts_with("MCP:") => mcp_pattern(tool).as_deref() == Some(pattern),
        _ => false,
    }
}

/// If a HUD-side Always rule matches, the Cursor permission reply to write
/// without holding a ticket.
///
/// @param request Parsed permission request
/// @return Cursor `{"permission":"allow"}` when matched; otherwise None
pub fn auto_allow_response(request: &Request) -> Option<Value> {
    let permission = request.permission()?;
    if !matches!(&permission.reply, Reply::CursorPermission) {
        return None;
    }
    workspace_always_matches(request).then(|| json!({"permission": "allow"}))
}
fn questions(value: &Value) -> Option<Vec<Question>> {
    let list = value.as_array().filter(|v| !v.is_empty())?;
    list.iter()
        .map(|item| {
            let question = item["question"]
                .as_str()
                .filter(|s| !s.trim().is_empty())?
                .to_owned();
            let options = item["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| {
                    Some(QuestionOption {
                        label: text(&o["label"])?.into(),
                        description: text(&o["description"]).map(str::to_owned),
                    })
                })
                .collect::<Vec<_>>();
            if options.is_empty() {
                return None;
            }
            Some(Question {
                id: item["id"].as_str().map(str::to_owned),
                question,
                header: text(&item["header"]).map(str::to_owned),
                options,
                multi_select: item["multiSelect"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}
fn file_path(input: &Value) -> Option<String> {
    trimmed(&input["file_path"]).or_else(|| trimmed(&input["notebook_path"]))
}
fn lines(value: &Value) -> Option<String> {
    text(value).map(|s| s.split('\n').take(6).collect::<Vec<_>>().join("\n"))
}
fn summary(tool: Option<&str>, input: &Value) -> String {
    let Some(tool) = tool.filter(|s| !s.is_empty()) else {
        return "Tool call".into();
    };
    match tool {
        "Bash" | "BashOutput" | "KillShell" => trimmed(&input["description"])
            .or_else(|| trimmed(&input["command"]))
            .unwrap_or_else(|| tool.into()),
        "apply_patch" => {
            trimmed(&input["description"]).unwrap_or_else(|| "Apply file changes".into())
        }
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => file_path(input)
            .map(|p| p.rsplit('/').next().unwrap_or(&p).to_owned())
            .unwrap_or_else(|| tool.into()),
        "Glob" | "Grep" => trimmed(&input["pattern"]).unwrap_or_else(|| tool.into()),
        "WebFetch" => trimmed(&input["url"]).unwrap_or_else(|| tool.into()),
        "WebSearch" => trimmed(&input["query"]).unwrap_or_else(|| tool.into()),
        "Task" | "Agent" => trimmed(&input["description"]).unwrap_or_else(|| tool.into()),
        "ExitPlanMode" => input["plan"]
            .as_str()
            .and_then(|s| {
                s.split('\n')
                    .map(|s| s.trim_start_matches(['#', ' ']).trim())
                    .find(|s| !s.is_empty())
            })
            .map(|s| s.chars().take(2048).collect())
            .unwrap_or_else(|| "Plan".into()),
        _ => {
            if let Some(rest) = tool.strip_prefix("mcp__") {
                if let Some((server, name)) = rest.split_once("__") {
                    return format!("{server} · {name}");
                }
            }
            tool.into()
        }
    }
}
fn detail(tool: Option<&str>, input: &Value) -> Option<String> {
    if tool == Some("AskUserQuestion") {
        return None;
    }
    let subject = match tool {
        Some("Bash" | "apply_patch") => trimmed(&input["command"]),
        Some("Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit") => file_path(input),
        Some("WebFetch") => trimmed(&input["url"]),
        Some("ExitPlanMode") => trimmed(&input["plan"]),
        _ => None,
    };
    subject
        .or_else(|| {
            input.as_object().filter(|o| !o.is_empty()).map(|object| {
                let mut fields = object.iter().collect::<Vec<_>>();
                fields.sort_by(|a, b| a.0.cmp(b.0));
                fields
                    .into_iter()
                    .map(|(key, value)| {
                        format!(
                            "{key}: {}",
                            value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| value.to_string())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        })
        .map(|s| s.chars().take(2048).collect())
}
fn rules(value: &Value) -> Option<String> {
    let rules = value["rules"].as_array().filter(|v| !v.is_empty())?;
    rules
        .iter()
        .map(|rule| {
            let tool = text(&rule["toolName"])?;
            Some(if let Some(content) = text(&rule["ruleContent"]) {
                format!("{tool}({content})")
            } else {
                tool.into()
            })
        })
        .collect::<Option<Vec<_>>>()
        .map(|v| v.join(", "))
}

pub fn parse_request(source_name: &str, payload: &Value) -> Result<Option<Request>, String> {
    let (source, hooks) = hooks_of(source_name)?;
    validate_payload(payload)?;
    let Some(permission) = &hooks.permission else {
        return Err("This client does not use a permission hook".into());
    };
    let normalized;
    let payload = if matches!(&permission.reply, Reply::CursorPermission) {
        normalized = normalize_cursor_payload(payload);
        &normalized
    } else {
        payload
    };
    let Some(session) = text(&payload["session_id"]) else {
        return Ok(None);
    };
    let tool = payload["tool_name"].as_str();
    if permission.unanswerable.contains(&tool.unwrap_or("")) {
        return Ok(None);
    }
    let input = &payload["tool_input"];
    let kind = tool.map(tool_kind);
    let questions = if kind == Some("AskUserQuestion") {
        let Some(q) = questions(&input["questions"]) else {
            return Ok(None);
        };
        q
    } else {
        vec![]
    };
    let cursor = matches!(&permission.reply, Reply::CursorPermission);
    let mut path = if matches!(
        kind,
        Some("Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit")
    ) {
        file_path(input)
    } else {
        None
    };
    if path.is_none() && cursor && matches!(kind, Some("Delete" | "Glob" | "Grep")) {
        path = if matches!(kind, Some("Glob" | "Grep")) {
            trimmed(&input["path"]).or_else(|| trimmed(&input["target_directory"]))
        } else {
            scope_path(input)
        };
    }
    let change = if kind == Some("MultiEdit") {
        input["edits"]
            .as_array()
            .and_then(|v| v.first())
            .unwrap_or(&Value::Null)
    } else if path.is_some() {
        input
    } else {
        &Value::Null
    };
    let scope = if cursor {
        workspace_scope(payload)
    } else {
        WorkspaceScope {
            primary: None,
            match_keys: Vec::new(),
        }
    };
    let cwd = payload["cwd"].as_str().map(str::to_owned);
    let mut suggestions = payload["permission_suggestions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if permission.rules && suggestions.is_empty() {
        if let Some(tool) = kind {
            if cursor {
                if scope.primary.is_some() {
                    if let Some(suggestion) = synthetic_cursor_always(tool, input, cwd.as_deref()) {
                        suggestions.push(suggestion);
                    }
                }
            } else {
                suggestions.push(synthetic_always(tool, input));
            }
        }
    }
    Ok(Some(Request {
        source: source.into(),
        session_id: session_id(source, session),
        kind: if !questions.is_empty() {
            "question"
        } else if kind == Some("ExitPlanMode") {
            "plan"
        } else {
            "permission"
        }
        .into(),
        turn_id: payload["turn_id"].as_str().map(str::to_owned),
        requires_user_reviewer: permission.reviewer_gate,
        tool_name: tool.map(str::to_owned),
        summary: questions
            .first()
            .map(|q| q.question.clone())
            .unwrap_or_else(|| summary(kind, input)),
        detail: detail(kind, input),
        cwd,
        workspace: scope.primary,
        workspace_keys: scope.match_keys,
        path,
        removed: lines(&change["old_string"]),
        added: lines(&change["new_string"])
            .or_else(|| lines(&change["content"]))
            .or_else(|| lines(&change["new_source"])),
        suggestions,
        answer_input: (!questions.is_empty() || kind == Some("ExitPlanMode"))
            .then(|| input.clone()),
        questions,
    }))
}
impl Request {
    /// The client's permission hook, when its source is still known.
    fn permission(&self) -> Option<&'static Permission> {
        hooks_by_id(&self.source)?.permission.as_ref()
    }
    fn plans(&self) -> bool {
        self.permission().is_some_and(|permission| permission.plans)
    }
    fn always(&self) -> Option<&Value> {
        if !self.permission().is_some_and(|permission| permission.rules) {
            return None;
        }
        self.suggestions
            .iter()
            .find(|v| v["type"] == "addRules" && v["behavior"] == "allow" && rules(v).is_some())
    }
    fn plan(&self) -> Option<&str> {
        if self.kind != "plan" {
            return None;
        }
        self.answer_input.as_ref()?["plan"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
    pub fn view(&self, id: &str, created_at_ms: i64) -> Value {
        let mut questions = serde_json::to_value(&self.questions).unwrap_or_else(|_| json!([]));
        if let Some(list) = questions.as_array_mut() {
            for (item, question) in list.iter_mut().zip(&self.questions) {
                item["answerKey"] = json!(question.id.as_deref().unwrap_or(&question.question));
            }
        }
        json!({"id":id,"title":self.summary,"source":self.source,"vendor":display(&self.source),"toolName":self.tool_name.as_deref().unwrap_or(""),"input":self.detail.as_deref().unwrap_or(""),"detail":self.detail,"summary":self.summary,"sessionId":self.session_id,"createdAt":iso(created_at_ms),"allowsAlways":self.always().is_some(),"kind":self.kind,"cwd":self.cwd,"path":self.path,"removed":self.removed,"added":self.added,"suggestions":self.suggestions,"alwaysAllowRule":self.always().and_then(rules),"questions":questions,"plan":self.plan(),"answersQuestion":!self.questions.is_empty(),"answersPlan":self.plans()&&self.plan().is_some()})
    }
    /// None is an empty response: close the hook connection without writing JSON.
    pub fn response(&self, decision: &Value) -> Result<Option<Value>, String> {
        let kind = decision["kind"]
            .as_str()
            .ok_or("Decision kind is required")?;
        let mut output = Map::new();
        match kind {
            "leave" => return Ok(None),
            "allow" => {
                output.insert("behavior".into(), json!("allow"));
            }
            "deny" => {
                output.insert("behavior".into(), json!("deny"));
            }
            "always" => {
                let Some(default) = self.always() else {
                    return Ok(None);
                };
                let suggestion = decision.get("suggestion").unwrap_or(default);
                if !self.suggestions.contains(suggestion)
                    || suggestion["type"] != "addRules"
                    || suggestion["behavior"] != "allow"
                    || rules(suggestion).is_none()
                {
                    return Err("Permission rule was not offered by this client".into());
                }
                output.insert("behavior".into(), json!("allow"));
                output.insert("updatedPermissions".into(), json!([suggestion]));
            }
            "approve_plan" => {
                if !self.plans() || self.plan().is_none() {
                    return Ok(None);
                }
                output.insert("behavior".into(), json!("allow"));
                output.insert("updatedInput".into(), self.answer_input.clone().unwrap());
                let accept = decision
                    .get("acceptEdits")
                    .map(|v| v.as_bool().ok_or("acceptEdits must be a boolean"))
                    .transpose()?
                    .unwrap_or(false);
                if accept {
                    output.insert(
                        "updatedPermissions".into(),
                        json!([{"type":"setMode","mode":"acceptEdits","destination":"session"}]),
                    );
                }
            }
            "keep_planning" => {
                if !self.plans() || self.plan().is_none() {
                    return Ok(None);
                }
                let note = decision
                    .get("message")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_str().ok_or("message must be a string"))
                    .transpose()?
                    .unwrap_or("")
                    .trim();
                let reviewed = "The user reviewed this plan in Agent HUD and did not approve it.";
                let message = if note.is_empty() {
                    format!("{reviewed} They want to keep planning.")
                } else {
                    format!("{reviewed} Their feedback: {note}")
                };
                output.insert("behavior".into(), json!("deny"));
                output.insert("message".into(), json!(message));
            }
            "answer" => {
                if self.questions.is_empty() {
                    return Ok(None);
                }
                let Some(mut input) = self
                    .answer_input
                    .as_ref()
                    .and_then(Value::as_object)
                    .cloned()
                else {
                    return Ok(None);
                };
                let answers = decision["answers"]
                    .as_object()
                    .ok_or("answers must be a question-keyed object")?;
                let mut written = Map::new();
                for question in &self.questions {
                    let key = question.id.as_deref().unwrap_or(&question.question);
                    if let Some(answer) = answers.get(key) {
                        let object = answer.as_object().ok_or(
                            "Each answer must contain selected choices and optional custom text",
                        )?;
                        let selected = object
                            .get("selected")
                            .map(|v| v.as_array().ok_or("selected must be an array"))
                            .transpose()?;
                        let mut words = selected
                            .into_iter()
                            .flatten()
                            .map(|v| {
                                v.as_str()
                                    .map(str::to_owned)
                                    .ok_or("selected entries must be strings")
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        if let Some(custom) = object.get("custom").filter(|v| !v.is_null()) {
                            words.push(custom.as_str().ok_or("custom must be a string")?.into());
                        }
                        written.insert(question.question.clone(), json!(words.join(", ")));
                    }
                }
                input.insert("answers".into(), Value::Object(written));
                output.insert("behavior".into(), json!("allow"));
                output.insert("updatedInput".into(), Value::Object(input));
            }
            _ => return Err("Unsupported decision kind".into()),
        }
        if kind == "always" {
            if let Some(suggestion) = decision.get("suggestion").or_else(|| self.always()) {
                remember_workspace_always(self, suggestion)?;
            }
        }
        if self
            .permission()
            .is_some_and(|permission| matches!(&permission.reply, Reply::Decision))
        {
            return Ok(Some(
                json!({"decision":if output["behavior"]=="allow"{"approve"}else{"block"}}),
            ));
        }
        if self
            .permission()
            .is_some_and(|permission| matches!(&permission.reply, Reply::CursorPermission))
        {
            let allowed = output["behavior"] == "allow";
            let mut body = json!({"permission": if allowed { "allow" } else { "deny" }});
            if !allowed {
                body["user_message"] = json!("Denied in Agent HUD");
            }
            return Ok(Some(body));
        }
        Ok(Some(
            json!({"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":output}}),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn reset() {
        set_workspace_always_persister(None);
        load_workspace_always(vec![]);
    }

    fn cursor_shell(conversation: &str, command: &str, workspace: &str) -> Value {
        json!({
            "conversation_id": conversation,
            "hook_event_name": "beforeShellExecution",
            "command": command,
            "cwd": workspace,
            "workspace_roots": [workspace],
        })
    }

    fn cursor_shell_roots(command: &str, cwd: &str, roots: &[&str]) -> Value {
        json!({
            "conversation_id": "conv",
            "hook_event_name": "beforeShellExecution",
            "command": command,
            "cwd": cwd,
            "workspace_roots": roots,
        })
    }

    fn cursor_edit(conversation: &str, path: &str, workspace: &str) -> Value {
        json!({
            "conversation_id": conversation,
            "hook_event_name": "preToolUse",
            "tool_name": "Edit",
            "tool_input": {"file_path": path, "old_string": "a", "new_string": "b"},
            "cwd": workspace,
            "workspace_roots": [workspace],
        })
    }

    fn allow_prefix(request: &Request) -> String {
        request.always().and_then(rules).unwrap_or_default()
    }

    #[test]
    fn cursor_shell_parses_and_replies_with_permission() {
        let _guard = lock();
        reset();
        let request = parse_request("cursor", &cursor_shell("always-sess", "echo hi", "/tmp"))
            .unwrap()
            .unwrap();
        assert_eq!(request.session_id, "cursor:always-sess");
        assert_eq!(request.tool_name.as_deref(), Some("Shell"));
        assert!(request.workspace.is_some());
        assert_eq!(allow_prefix(&request), "Bash(echo hi)");
        assert_eq!(request.always().unwrap()["destination"], "workspace");
        let allow = request
            .response(&json!({"kind": "allow"}))
            .unwrap()
            .unwrap();
        assert_eq!(allow, json!({"permission": "allow"}));
        assert!(workspace_always_snapshot().is_empty());
        let deny = request.response(&json!({"kind": "deny"})).unwrap().unwrap();
        assert_eq!(
            deny,
            json!({"permission": "deny", "user_message": "Denied in Agent HUD"})
        );
        assert!(auto_allow_response(&request).is_none());
    }

    #[test]
    fn shell_always_is_a_command_prefix_not_the_whole_tool() {
        let _guard = lock();
        reset();
        let root = tempfile::tempdir().unwrap();
        let ws = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let first = parse_request("cursor", &cursor_shell("c1", "git status --short", &ws))
            .unwrap()
            .unwrap();
        assert_eq!(allow_prefix(&first), "Bash(git status)");
        let listed = parse_request("cursor", &cursor_shell("c0", "ls -la /tmp", &ws))
            .unwrap()
            .unwrap();
        assert_eq!(allow_prefix(&listed), "Bash(ls)");
        first.response(&json!({"kind": "always"})).unwrap().unwrap();
        let snap = workspace_always_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].tool_name, "Bash");
        assert_eq!(snap[0].pattern, "git status");
        assert_eq!(snap[0].workspace, ws);

        let allowed = ["git status", "git status -sb", "FOO=1 git status --short"];
        for command in allowed {
            let next = parse_request("cursor", &cursor_shell("c2", command, &ws))
                .unwrap()
                .unwrap();
            assert_eq!(
                auto_allow_response(&next),
                Some(json!({"permission": "allow"})),
                "{command}"
            );
        }
        for command in ["git diff", "gitk", "git statusfoo", "npm install", "ls"] {
            let next = parse_request("cursor", &cursor_shell("c3", command, &ws))
                .unwrap()
                .unwrap();
            assert!(auto_allow_response(&next).is_none(), "{command}");
        }
        let bare = parse_request("cursor", &cursor_shell("c4", "git", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&bare).is_none());
        bare.response(&json!({"kind": "always"})).unwrap().unwrap();
        let git = parse_request("cursor", &cursor_shell("c5", "git diff --stat", &ws))
            .unwrap()
            .unwrap();
        assert_eq!(
            auto_allow_response(&git),
            Some(json!({"permission": "allow"}))
        );
        let gitk = parse_request("cursor", &cursor_shell("c6", "gitk", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&gitk).is_none());
    }

    #[test]
    fn npm_install_prefix_matches_cursor_colon_glob_too() {
        let _guard = lock();
        reset();
        let root = tempfile::tempdir().unwrap();
        let ws = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let request = parse_request("cursor", &cursor_shell("c1", "npm install express", &ws))
            .unwrap()
            .unwrap();
        assert_eq!(allow_prefix(&request), "Bash(npm install)");
        request
            .response(&json!({"kind": "always"}))
            .unwrap()
            .unwrap();
        let again = parse_request("cursor", &cursor_shell("c2", "npm install left-pad", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&again).is_some());
        let test = parse_request("cursor", &cursor_shell("c3", "npm test", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&test).is_none());

        load_workspace_always(vec![WorkspaceAlwaysRule {
            workspace: ws.clone(),
            tool_name: "Bash".into(),
            pattern: "npm:install*".into(),
        }]);
        let globbed = parse_request("cursor", &cursor_shell("c4", "npm install express", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&globbed).is_some());
        let not_glob = parse_request("cursor", &cursor_shell("c5", "npm test", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&not_glob).is_none());
        load_workspace_always(vec![WorkspaceAlwaysRule {
            workspace: ws.clone(),
            tool_name: "Bash".into(),
            pattern: "*".into(),
        }]);
        let starred = parse_request("cursor", &cursor_shell("c6", "npm install", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&starred).is_none());
    }

    #[test]
    fn compound_shell_commands_do_not_offer_always() {
        let _guard = lock();
        reset();
        let root = tempfile::tempdir().unwrap();
        let ws = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let request = parse_request("cursor", &cursor_shell("c1", "git status && rm -rf /", &ws))
            .unwrap()
            .unwrap();
        assert!(request.always().is_none());
        assert_eq!(request.view("id", 0)["allowsAlways"], false);
        let stored = parse_request("cursor", &cursor_shell("c2", "git status", &ws))
            .unwrap()
            .unwrap();
        stored
            .response(&json!({"kind": "always"}))
            .unwrap()
            .unwrap();
        let compound = parse_request("cursor", &cursor_shell("c3", "git status && rm -rf /", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&compound).is_none());
    }

    #[test]
    fn edit_always_is_path_scoped_and_other_workspaces_still_ask() {
        let _guard = lock();
        reset();
        let root = tempfile::tempdir().unwrap();
        let ws = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let file = format!("{ws}/a.rs");
        let first = parse_request("cursor", &cursor_edit("conv-1", &file, &ws))
            .unwrap()
            .unwrap();
        assert_eq!(allow_prefix(&first), format!("Edit({file})"));
        assert_eq!(first.always().unwrap()["destination"], "workspace");
        first.response(&json!({"kind": "always"})).unwrap().unwrap();
        let same = parse_request("cursor", &cursor_edit("conv-2", &file, &ws))
            .unwrap()
            .unwrap();
        assert_eq!(
            auto_allow_response(&same),
            Some(json!({"permission": "allow"}))
        );
        let other_file =
            parse_request("cursor", &cursor_edit("conv-3", &format!("{ws}/b.rs"), &ws))
                .unwrap()
                .unwrap();
        assert!(auto_allow_response(&other_file).is_none());
        let other = tempfile::tempdir().unwrap();
        let other_ws = other
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let elsewhere = parse_request(
            "cursor",
            &cursor_edit("conv-4", &format!("{other_ws}/a.rs"), &other_ws),
        )
        .unwrap()
        .unwrap();
        assert!(auto_allow_response(&elsewhere).is_none());
        let shell = parse_request("cursor", &cursor_shell("conv-5", "ls", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&shell).is_none());
    }

    #[test]
    fn shell_always_follows_the_root_that_contains_cwd_not_only_the_first() {
        let _guard = lock();
        reset();
        let parent = tempfile::tempdir().unwrap();
        let a = parent.path().join("frontend");
        let b = parent.path().join("backend");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let a = a.canonicalize().unwrap().to_string_lossy().into_owned();
        let b = b.canonicalize().unwrap().to_string_lossy().into_owned();
        let first = parse_request("cursor", &cursor_shell_roots("cargo test", &b, &[&a, &b]))
            .unwrap()
            .unwrap();
        assert_eq!(first.workspace.as_deref(), Some(b.as_str()));
        assert_eq!(allow_prefix(&first), "Bash(cargo test)");
        first.response(&json!({"kind": "always"})).unwrap().unwrap();
        assert_eq!(workspace_always_snapshot()[0].workspace, b);

        let reordered = parse_request(
            "cursor",
            &cursor_shell_roots("cargo test --lib", &b, &[&b, &a]),
        )
        .unwrap()
        .unwrap();
        assert!(auto_allow_response(&reordered).is_some());

        let other_root = parse_request("cursor", &cursor_shell_roots("cargo test", &a, &[&a, &b]))
            .unwrap()
            .unwrap();
        assert!(
            auto_allow_response(&other_root).is_some(),
            "a listed root keeps the trust even when cwd is in the other root"
        );
        let only_frontend = parse_request("cursor", &cursor_shell_roots("cargo test", &a, &[&a]))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&only_frontend).is_none());
    }

    #[test]
    fn mcp_always_stays_on_one_tool() {
        let _guard = lock();
        reset();
        let root = tempfile::tempdir().unwrap();
        let ws = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let payload = |tool: &str| {
            json!({
                "conversation_id": "c",
                "hook_event_name": "beforeMCPExecution",
                "mcp_server_name": "github",
                "tool_name": tool,
                "cwd": ws,
                "workspace_roots": [ws],
            })
        };
        let first = parse_request("cursor", &payload("create_issue"))
            .unwrap()
            .unwrap();
        assert_eq!(allow_prefix(&first), "MCP(github:create_issue)");
        first.response(&json!({"kind": "always"})).unwrap().unwrap();
        let same = parse_request("cursor", &payload("create_issue"))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&same).is_some());
        let other = parse_request("cursor", &payload("list_issues"))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&other).is_none());
        assert_eq!(
            workspace_always_snapshot()[0].pattern,
            "github:create_issue"
        );
    }

    #[test]
    fn tools_without_a_safe_scope_do_not_offer_always() {
        let _guard = lock();
        reset();
        let root = tempfile::tempdir().unwrap();
        let ws = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let task = parse_request(
            "cursor",
            &json!({
                "conversation_id": "c",
                "hook_event_name": "subagentStart",
                "task": "Explore the auth flow",
                "cwd": ws,
                "workspace_roots": [ws],
            }),
        )
        .unwrap()
        .unwrap();
        assert!(task.always().is_none());
        let search = parse_request(
            "cursor",
            &json!({
                "conversation_id": "c",
                "hook_event_name": "preToolUse",
                "tool_name": "WebSearch",
                "tool_input": {"query": "serde"},
                "cwd": ws,
                "workspace_roots": [ws],
            }),
        )
        .unwrap()
        .unwrap();
        assert!(search.always().is_none());
        let grep = parse_request(
            "cursor",
            &json!({
                "conversation_id": "c",
                "hook_event_name": "preToolUse",
                "tool_name": "Grep",
                "tool_input": {"pattern": "TODO", "path": "src"},
                "cwd": ws,
                "workspace_roots": [ws],
            }),
        )
        .unwrap()
        .unwrap();
        assert!(grep.always().is_some());
        let saved = normalize_scope_path("src", Some(&ws)).unwrap();
        assert_eq!(allow_prefix(&grep), format!("Grep({saved})"));
        let fetch = |url: &str| {
            parse_request(
                "cursor",
                &json!({
                    "conversation_id": "c",
                    "hook_event_name": "preToolUse",
                    "tool_name": "WebFetch",
                    "tool_input": {"url": url},
                    "cwd": ws,
                    "workspace_roots": [ws],
                }),
            )
            .unwrap()
            .unwrap()
        };
        let page = fetch("https://example.com/docs");
        assert_eq!(allow_prefix(&page), "WebFetch(https://example.com/docs)");
        page.response(&json!({"kind": "always"})).unwrap().unwrap();
        assert!(auto_allow_response(&fetch("https://example.com/docs")).is_some());
        assert!(auto_allow_response(&fetch("https://example.com/other")).is_none());
    }

    #[test]
    fn a_failed_persister_does_not_keep_the_rule_in_memory() {
        let _guard = lock();
        reset();
        let root = tempfile::tempdir().unwrap();
        let ws = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        load_workspace_always(vec![WorkspaceAlwaysRule {
            workspace: ws.clone(),
            tool_name: "Read".into(),
            pattern: format!("{ws}/kept.rs"),
        }]);
        set_workspace_always_persister(Some(std::sync::Arc::new(|_| Err("disk full".into()))));
        let request = parse_request("cursor", &cursor_shell("c", "ls", &ws))
            .unwrap()
            .unwrap();
        let error = request.response(&json!({"kind": "always"})).unwrap_err();
        assert!(error.contains("disk full"), "{error}");
        let snap = workspace_always_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].tool_name, "Read");
        let again = parse_request("cursor", &cursor_shell("c2", "ls", &ws))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&again).is_none());
        reset();
    }
}
