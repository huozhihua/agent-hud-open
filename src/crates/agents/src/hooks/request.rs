use super::{display, hooks_by_id, hooks_of, session_id, Permission, Reply};
use crate::{iso, text, trimmed, validate_payload};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::sync::Mutex;

/// Session-scoped Always rules for clients that cannot write permanent client
/// rules (Cursor). Checked in the HUD process before holding a ticket.
static SESSION_ALWAYS: Mutex<Vec<SessionAlways>> = Mutex::new(Vec::new());

#[derive(Clone)]
struct SessionAlways {
    session_id: String,
    tool_name: String,
    pattern: String,
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

/// Build a session Always suggestion when the client offers no `permission_suggestions`.
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

fn remember_session_always(request: &Request, suggestion: &Value) {
    let pattern = suggestion["rules"]
        .as_array()
        .and_then(|list| list.first())
        .and_then(|rule| text(&rule["ruleContent"]))
        .map(str::to_owned)
        .or_else(|| {
            suggestion["rules"]
                .as_array()
                .and_then(|list| list.first())
                .and_then(|rule| text(&rule["toolName"]))
                .map(|_| "*".into())
        });
    let Some(pattern) = pattern else {
        return;
    };
    let tool = request
        .tool_name
        .as_deref()
        .map(tool_kind)
        .unwrap_or("")
        .to_owned();
    let Ok(mut rules) = SESSION_ALWAYS.lock() else {
        return;
    };
    let entry = SessionAlways {
        session_id: request.session_id.clone(),
        tool_name: tool,
        pattern,
    };
    if !rules.iter().any(|rule| {
        rule.session_id == entry.session_id
            && rule.tool_name == entry.tool_name
            && rule.pattern == entry.pattern
    }) {
        rules.push(entry);
    }
}

fn session_always_matches(request: &Request) -> bool {
    let tool = request.tool_name.as_deref().map(tool_kind).unwrap_or("");
    let haystack = request
        .detail
        .as_deref()
        .or(request.path.as_deref())
        .unwrap_or("");
    let Ok(rules) = SESSION_ALWAYS.lock() else {
        return false;
    };
    rules.iter().any(|rule| {
        rule.session_id == request.session_id
            && (rule.tool_name == tool
                || rule.tool_name == request.tool_name.as_deref().unwrap_or(""))
            && (rule.pattern == "*"
                || rule.pattern == haystack
                || (!rule.pattern.is_empty() && haystack.contains(&rule.pattern)))
    })
}

/// If a HUD-side Always rule matches, the Cursor permission reply to write
/// without holding a ticket.
///
/// @param request Parsed permission request
/// @return Cursor `{"permission":"allow"}` when matched; otherwise None
pub fn auto_allow_response(request: &Request) -> Option<Value> {
    let permission = request.permission()?;
    if !matches!(permission.reply, Reply::CursorPermission) {
        return None;
    }
    session_always_matches(request).then(|| json!({"permission": "allow"}))
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
    let payload = if matches!(permission.reply, Reply::CursorPermission) {
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
    let path = if matches!(
        kind,
        Some("Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit")
    ) {
        file_path(input)
    } else {
        None
    };
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
    let mut suggestions = payload["permission_suggestions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if permission.rules && suggestions.is_empty() {
        if let Some(tool) = kind {
            suggestions.push(synthetic_always(tool, input));
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
        cwd: payload["cwd"].as_str().map(str::to_owned),
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
                remember_session_always(self, suggestion);
            }
        }
        if self
            .permission()
            .is_some_and(|permission| matches!(permission.reply, Reply::Decision))
        {
            return Ok(Some(
                json!({"decision":if output["behavior"]=="allow"{"approve"}else{"block"}}),
            ));
        }
        if self
            .permission()
            .is_some_and(|permission| matches!(permission.reply, Reply::CursorPermission))
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

    fn cursor_shell(command: &str) -> Value {
        json!({
            "conversation_id": "always-sess",
            "hook_event_name": "beforeShellExecution",
            "command": command,
            "cwd": "/tmp",
        })
    }

    #[test]
    fn cursor_shell_parses_and_replies_with_permission() {
        let request = parse_request("cursor", &cursor_shell("echo hi"))
            .unwrap()
            .unwrap();
        assert_eq!(request.session_id, "cursor:always-sess");
        assert_eq!(request.tool_name.as_deref(), Some("Shell"));
        assert!(request.always().is_some());
        let allow = request
            .response(&json!({"kind": "allow"}))
            .unwrap()
            .unwrap();
        assert_eq!(allow, json!({"permission": "allow"}));
        let deny = request.response(&json!({"kind": "deny"})).unwrap().unwrap();
        assert_eq!(
            deny,
            json!({"permission": "deny", "user_message": "Denied in Agent HUD"})
        );
    }

    #[test]
    fn cursor_always_auto_allows_a_matching_follow_up() {
        let first = parse_request("cursor", &cursor_shell("npm test"))
            .unwrap()
            .unwrap();
        first.response(&json!({"kind": "always"})).unwrap().unwrap();
        let second = parse_request("cursor", &cursor_shell("npm test"))
            .unwrap()
            .unwrap();
        assert_eq!(
            auto_allow_response(&second),
            Some(json!({"permission": "allow"}))
        );
        let other = parse_request("cursor", &cursor_shell("rm -rf /"))
            .unwrap()
            .unwrap();
        assert!(auto_allow_response(&other).is_none());
    }
}
