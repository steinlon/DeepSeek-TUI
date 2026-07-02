//! Mechanical enforcement of repo-law protected invariants.
//!
//! `.codewhale/constitution.json` invariants were previously advisory prose
//! rendered into the prompt. Entries that carry `paths` globs now also
//! compile into write holds evaluated in the engine's tool gate — the law
//! becomes mechanism, with a receipt naming the invariant.
//!
//! The contract mirrors the project-overlay rule ("overrides may only
//! tighten"):
//!
//! - Law can only ADD holds. There is no allow/widen shape in the schema, so
//!   a crafted constitution cannot grant authority.
//! - `ask` force-prompts in every mode, including YOLO — like the built-in
//!   safety floor, law is not bypassable by mode. `block` denies outright.
//! - Any failure (missing file, parse error, bad glob) degrades to fewer or
//!   zero rules — never a poisoned gate, never a hold on unprotected paths.
//! - Only the repo-local constitution participates. The user-global
//!   constitution stays advisory prose and never reaches this module.

use std::path::Path;

use serde_json::Value;

use crate::project_context::{RepoLawAction, RepoLawRule, load_repo_law_rules};

/// Tools whose inputs name filesystem write targets we can hold.
const WRITE_TOOLS: &[&str] = &["write_file", "edit_file", "apply_patch"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepoLawPlanDecision {
    /// Force an approval prompt naming the law, in every mode.
    ForcePrompt(String),
    /// Deny the call outright, naming the law.
    Block(String),
}

/// Evaluate the workspace's repo law against a proposed tool call. Returns
/// `None` for tools without write targets, workspaces without enforceable
/// law, and writes outside every protected glob.
pub(crate) fn repo_law_plan_decision(
    workspace: &Path,
    tool_name: &str,
    tool_input: &Value,
) -> Option<RepoLawPlanDecision> {
    if !WRITE_TOOLS.contains(&tool_name) {
        return None;
    }
    let targets = write_target_paths(workspace, tool_input);
    if targets.is_empty() {
        return None;
    }
    let rules = load_repo_law_rules(workspace);
    if rules.is_empty() {
        return None;
    }

    // Strongest action wins across all (rule, target) matches.
    let mut hold: Option<(&RepoLawRule, &str)> = None;
    for rule in &rules {
        for target in &targets {
            if rule.globs.is_match(target) {
                let stronger = matches!(rule.action, RepoLawAction::Block) || hold.is_none();
                let already_blocking = hold
                    .as_ref()
                    .is_some_and(|(held, _)| matches!(held.action, RepoLawAction::Block));
                if stronger && !already_blocking {
                    hold = Some((rule, target.as_str()));
                }
            }
        }
    }
    let (rule, target) = hold?;
    let reason = format!(
        "Repo law holds this write: \"{}\" protects {target} (.codewhale/constitution.json)",
        rule.text
    );
    Some(match rule.action {
        RepoLawAction::Ask => RepoLawPlanDecision::ForcePrompt(reason),
        RepoLawAction::Block => RepoLawPlanDecision::Block(reason),
    })
}

/// Extract workspace-relative write targets from a tool input. Follows the
/// same shapes the approval surfaces use: `path`/`target`/`destination`
/// params, `changes[].path`, and unified-diff `+++ b/` headers.
fn write_target_paths(workspace: &Path, input: &Value) -> Vec<String> {
    let mut targets = Vec::new();
    for key in ["path", "target", "destination", "file_path"] {
        if let Some(path) = input.get(key).and_then(Value::as_str) {
            push_normalized(&mut targets, workspace, path);
        }
    }
    if let Some(changes) = input.get("changes").and_then(Value::as_array) {
        for change in changes {
            if let Some(path) = change.get("path").and_then(Value::as_str) {
                push_normalized(&mut targets, workspace, path);
            }
        }
    }
    if let Some(patch) = input.get("patch").and_then(Value::as_str) {
        for line in patch.lines() {
            if let Some(rest) = line.strip_prefix("+++ b/") {
                push_normalized(&mut targets, workspace, rest.trim());
            } else if let Some(rest) = line.strip_prefix("*** Update File: ") {
                push_normalized(&mut targets, workspace, rest.trim());
            } else if let Some(rest) = line.strip_prefix("*** Add File: ") {
                push_normalized(&mut targets, workspace, rest.trim());
            } else if let Some(rest) = line.strip_prefix("*** Delete File: ") {
                push_normalized(&mut targets, workspace, rest.trim());
            }
        }
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Normalize to a forward-slash, workspace-relative string so globs written
/// as `crates/x/**` match regardless of how the tool spelled the path.
fn push_normalized(targets: &mut Vec<String>, workspace: &Path, raw: &str) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    let path = Path::new(trimmed);
    let relative = path.strip_prefix(workspace).unwrap_or(path);
    let mut normalized = relative
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string();
    if normalized.is_empty() {
        return;
    }
    // A path that escapes the workspace via `..` is normalized as-written;
    // globs are workspace-relative so it simply won't match, and the
    // ordinary approval/sandbox gates still govern it.
    if let Some(stripped) = normalized.strip_prefix('/') {
        // Absolute path outside the workspace: keep the tail so a law like
        // `**/secrets.toml` can still match; full-anchored globs won't.
        normalized = stripped.to_string();
    }
    targets.push(normalized);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn write_law(workspace: &Path, body: &str) {
        let dir = workspace.join(".codewhale");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("constitution.json"), body).unwrap();
    }

    const LAW: &str = r#"{
        "authority": ["AGENTS.md"],
        "protected_invariants": [
            "Keep DeepSeek support first-class.",
            { "text": "The wire format is frozen", "paths": ["crates/protocol/**"], "action": "block" },
            { "text": "Release notes need human review", "paths": ["CHANGELOG.md"] }
        ]
    }"#;

    #[test]
    fn advisory_only_law_never_holds() {
        let tmp = TempDir::new().unwrap();
        write_law(
            tmp.path(),
            r#"{"protected_invariants": ["Prose only, no paths."]}"#,
        );
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "write_file",
                &json!({"path": "src/main.rs", "content": "x"}),
            ),
            None
        );
    }

    #[test]
    fn block_action_denies_protected_write() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        let decision = repo_law_plan_decision(
            tmp.path(),
            "write_file",
            &json!({"path": "crates/protocol/wire.rs", "content": "x"}),
        );
        let Some(RepoLawPlanDecision::Block(reason)) = decision else {
            panic!("expected block, got {decision:?}");
        };
        assert!(reason.contains("The wire format is frozen"), "{reason}");
        assert!(reason.contains("crates/protocol/wire.rs"), "{reason}");
        assert!(reason.contains(".codewhale/constitution.json"), "{reason}");
    }

    #[test]
    fn ask_action_force_prompts_and_names_the_law() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        let decision = repo_law_plan_decision(
            tmp.path(),
            "edit_file",
            &json!({"path": "CHANGELOG.md", "old": "a", "new": "b"}),
        );
        let Some(RepoLawPlanDecision::ForcePrompt(reason)) = decision else {
            panic!("expected force prompt, got {decision:?}");
        };
        assert!(
            reason.contains("Release notes need human review"),
            "{reason}"
        );
    }

    #[test]
    fn unprotected_writes_and_non_write_tools_pass() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "write_file",
                &json!({"path": "src/main.rs", "content": "x"}),
            ),
            None
        );
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "read_file",
                &json!({"path": "crates/protocol/wire.rs"}),
            ),
            None
        );
    }

    #[test]
    fn apply_patch_targets_are_extracted_from_all_shapes() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        // changes[].path shape
        let decision = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({"changes": [{"path": "crates/protocol/msg.rs"}]}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
        // unified diff shape
        let decision = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({"patch": "--- a/crates/protocol/msg.rs\n+++ b/crates/protocol/msg.rs\n@@\n"}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
        // codex envelope shape
        let decision = repo_law_plan_decision(
            tmp.path(),
            "apply_patch",
            &json!({"patch": "*** Begin Patch\n*** Update File: crates/protocol/msg.rs\n*** End Patch\n"}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
    }

    #[test]
    fn block_outranks_ask_when_both_match() {
        let tmp = TempDir::new().unwrap();
        write_law(
            tmp.path(),
            r#"{"protected_invariants": [
                { "text": "ask first", "paths": ["docs/**"] },
                { "text": "never", "paths": ["docs/frozen/**"], "action": "block" }
            ]}"#,
        );
        let decision = repo_law_plan_decision(
            tmp.path(),
            "write_file",
            &json!({"path": "docs/frozen/spec.md", "content": "x"}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
    }

    #[test]
    fn absolute_and_dot_prefixed_paths_normalize_to_workspace_relative() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), LAW);
        let absolute = tmp.path().join("crates/protocol/wire.rs");
        let decision = repo_law_plan_decision(
            tmp.path(),
            "write_file",
            &json!({"path": absolute.to_string_lossy(), "content": "x"}),
        );
        assert!(matches!(decision, Some(RepoLawPlanDecision::Block(_))));
        let decision = repo_law_plan_decision(
            tmp.path(),
            "write_file",
            &json!({"path": "./CHANGELOG.md", "content": "x"}),
        );
        assert!(matches!(
            decision,
            Some(RepoLawPlanDecision::ForcePrompt(_))
        ));
    }

    #[test]
    fn malformed_law_and_bad_globs_degrade_to_no_holds() {
        let tmp = TempDir::new().unwrap();
        write_law(tmp.path(), "{ not json");
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "write_file",
                &json!({"path": "crates/protocol/wire.rs", "content": "x"}),
            ),
            None
        );
        write_law(
            tmp.path(),
            r#"{"protected_invariants": [
                { "text": "broken glob", "paths": ["crates/[invalid"] }
            ]}"#,
        );
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "write_file",
                &json!({"path": "crates/protocol/wire.rs", "content": "x"}),
            ),
            None
        );
    }

    #[test]
    fn no_law_file_means_no_holds() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            repo_law_plan_decision(
                tmp.path(),
                "write_file",
                &json!({"path": "anything.rs", "content": "x"}),
            ),
            None
        );
    }
}
