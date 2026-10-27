#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use agent_hud_host::HostOptions;
use std::path::{Component, Path, PathBuf};

fn main() {
    agent_hud_host::run_with_options(
        tauri::generate_context!(),
        None,
        HostOptions {
            canonical_executable: approved_hook_owner(),
        },
    );
}

/// Installed `.app` bundles may own client hooks; target/debug and disk-image
/// copies must not rewrite Cursor or other client settings.
fn approved_hook_owner() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.canonicalize().unwrap_or(exe);
    if !path.is_file() || !is_approved_hook_owner_path(&path) {
        return None;
    }
    Some(path)
}

fn is_approved_hook_owner_path(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let text = path.to_string_lossy();
    if text.starts_with("/Volumes/")
        || text.contains("/AppTranslocation/")
        || path
            .components()
            .any(|part| matches!(part, Component::Normal(name) if name == "target"))
    {
        return false;
    }
    text.contains(".app/Contents/MacOS/")
}

#[cfg(test)]
mod tests {
    use super::is_approved_hook_owner_path;
    use std::path::Path;

    #[test]
    fn installed_app_binaries_may_own_hooks() {
        assert!(is_approved_hook_owner_path(Path::new(
            "/Applications/Agent HUD Next.app/Contents/MacOS/agent-hud-next"
        )));
    }

    #[test]
    fn target_and_volume_paths_must_not_own_hooks() {
        assert!(!is_approved_hook_owner_path(Path::new(
            "/Users/me/agent-hud-open/target/release/agent-hud-next"
        )));
        assert!(!is_approved_hook_owner_path(Path::new(
            "/Volumes/Agent HUD Next/Agent HUD Next.app/Contents/MacOS/agent-hud-next"
        )));
    }
}
