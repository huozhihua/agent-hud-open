//! Durable HUD-side Always rules for clients without client-native permanent
//! rules (Cursor). The match table lives in `agent-hud-agents`. This file
//! only loads and writes `hooks/workspace-always.json`. It does not write
//! Cursor's `permissions.json` or command allowlist.
use agent_hud_agents::WorkspaceAlwaysRule;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

const FILE_NAME: &str = "workspace-always.json";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StoreFile {
    version: u32,
    rules: Vec<WorkspaceAlwaysRule>,
}

/// Path of the Always store under the Next data directory.
///
/// @param data Agent HUD Next data directory
/// @return `hooks/workspace-always.json`
pub fn store_path(data: &Path) -> PathBuf {
    data.join("hooks").join(FILE_NAME)
}

/// Load Always rules from disk into the agents in-memory table and register a
/// writer that keeps the file in sync. A failed write is logged and returned
/// so the in-memory table is not left ahead of disk.
///
/// @param data Agent HUD Next data directory
pub fn install(data: &Path) {
    let path = store_path(data);
    let rules = read_rules(&path);
    agent_hud_agents::load_workspace_always(rules);
    let path_for_write = path;
    agent_hud_agents::set_workspace_always_persister(Some(Arc::new(move |rules| {
        write_rules(&path_for_write, &rules).map_err(|error| {
            let message = format!(
                "workspace Always rules could not be saved to {}: {error}",
                path_for_write.display()
            );
            eprintln!("Agent HUD: {message}");
            message
        })
    })));
}

/// Drop the persister so tests or shutdown do not keep a stale path.
pub fn uninstall() {
    agent_hud_agents::set_workspace_always_persister(None);
}

fn read_rules(path: &Path) -> Vec<WorkspaceAlwaysRule> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            eprintln!(
                "Agent HUD could not read workspace Always rules from {}: {error}",
                path.display()
            );
            return Vec::new();
        }
    };
    match serde_json::from_slice::<StoreFile>(&bytes) {
        Ok(file) if file.version == VERSION => file.rules,
        Ok(_) => {
            eprintln!(
                "Agent HUD ignored workspace Always rules at {} (unsupported version)",
                path.display()
            );
            Vec::new()
        }
        Err(error) => {
            eprintln!(
                "Agent HUD ignored unreadable workspace Always rules at {}: {error}",
                path.display()
            );
            Vec::new()
        }
    }
}

fn write_rules(path: &Path, rules: &[WorkspaceAlwaysRule]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let body = json!({
        "version": VERSION,
        "rules": rules,
    });
    let bytes = serde_json::to_vec_pretty(&body).map_err(|error| error.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).map_err(|error| error.to_string())?;
    fs::rename(&tmp, path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    /// Global Always table/persister are process-wide; serialize these tests.
    fn always_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn round_trip_persists_rules_across_install() {
        let _guard = always_lock();
        let dir = tempdir().unwrap();
        let data = dir.path();
        uninstall();
        agent_hud_agents::load_workspace_always(vec![]);
        let rule = WorkspaceAlwaysRule {
            workspace: "/Users/me/project".into(),
            tool_name: "Edit".into(),
            pattern: "/Users/me/project/src/a.rs".into(),
        };
        write_rules(&store_path(data), std::slice::from_ref(&rule)).unwrap();
        install(data);
        let snap = agent_hud_agents::workspace_always_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].workspace, "/Users/me/project");
        assert_eq!(snap[0].tool_name, "Edit");
        assert_eq!(snap[0].pattern, "/Users/me/project/src/a.rs");
        uninstall();
        agent_hud_agents::load_workspace_always(vec![]);
    }

    #[test]
    fn remember_always_writes_a_shell_prefix_through_persister() {
        let _guard = always_lock();
        let dir = tempdir().unwrap();
        let data = dir.path();
        let ws = dir.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        let ws_key = ws.canonicalize().unwrap().to_string_lossy().into_owned();
        uninstall();
        agent_hud_agents::load_workspace_always(vec![]);
        install(data);
        let payload = json!({
            "conversation_id": "c1",
            "hook_event_name": "beforeShellExecution",
            "command": "ls",
            "cwd": ws_key,
            "workspace_roots": [ws_key],
        });
        let request = agent_hud_agents::parse_request("cursor", &payload)
            .unwrap()
            .unwrap();
        request
            .response(&json!({"kind": "always"}))
            .unwrap()
            .unwrap();
        assert!(store_path(data).is_file());
        uninstall();
        agent_hud_agents::load_workspace_always(vec![]);
        install(data);
        let snap = agent_hud_agents::workspace_always_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].tool_name, "Bash");
        assert_eq!(snap[0].pattern, "ls");
        assert_ne!(snap[0].pattern, "*");
        uninstall();
        agent_hud_agents::load_workspace_always(vec![]);
    }

    #[test]
    fn a_failed_write_is_surfaced_and_not_trusted_in_memory() {
        let _guard = always_lock();
        let dir = tempdir().unwrap();
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"x").unwrap();
        uninstall();
        agent_hud_agents::load_workspace_always(vec![]);
        install(&blocker);
        let ws = dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let payload = json!({
            "conversation_id": "c1",
            "hook_event_name": "beforeShellExecution",
            "command": "ls",
            "cwd": ws,
            "workspace_roots": [ws],
        });
        let request = agent_hud_agents::parse_request("cursor", &payload)
            .unwrap()
            .unwrap();
        let error = request.response(&json!({"kind": "always"})).unwrap_err();
        assert!(error.contains("Could not save"), "{error}");
        assert!(agent_hud_agents::workspace_always_snapshot().is_empty());
        let again = agent_hud_agents::parse_request("cursor", &payload)
            .unwrap()
            .unwrap();
        assert!(agent_hud_agents::auto_allow_response(&again).is_none());
        uninstall();
        agent_hud_agents::load_workspace_always(vec![]);
    }
}
