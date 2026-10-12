//! Coverage rules for a bridged tool's own tests: clap and the overlay must stay in step.

use super::generate::{find_path, leaf_paths, plan};
use super::overlay::Overlay;
use clap::Command;

/// Every legacy command name and visible or hidden alias, at any depth (clap's `help` excluded).
fn legacy_names(cmd: &Command, out: &mut Vec<String>) {
    for sub in cmd.get_subcommands().filter(|c| c.get_name() != "help") {
        out.push(sub.get_name().to_string());
        out.extend(sub.get_all_aliases().map(str::to_string));
        legacy_names(sub, out);
    }
}

/// Problems that make the overlay unfit for `command` (empty when it is fine):
///
/// - the overlay does not parse or breaks an overlay rule (a missing `access`, unknown keys,
///   a non-snake_case or duplicate op name, a dangerous read op, ...);
/// - an op names no clap command, names a command group, or is excluded;
/// - a published op name equals a legacy subcommand name or alias at any level (otherwise
///   `tool <name> ...` would change meaning when the bridge is added);
/// - a clap leaf command is neither published by an op nor `excluded`;
/// - an `excluded` entry names no clap command (stale);
/// - the generated document fails the lmapi publish check.
pub fn coverage_problems(command: &Command, overlay_toml: &str) -> Vec<String> {
    let overlay: Overlay = match toml::from_str(overlay_toml) {
        Ok(overlay) => overlay,
        Err(e) => return vec![format!("invalid lmapi overlay: {e}")],
    };
    let mut out = overlay.problems();
    let mut root = command.clone();
    root.build();
    let mut legacy = Vec::new();
    legacy_names(&root, &mut legacy);
    for op in &overlay.ops {
        if legacy.contains(&op.name) {
            out.push(format!("op name {} equals a legacy subcommand name", op.name));
        }
    }
    for path in leaf_paths(&root) {
        let joined = path.join(" ");
        let first = path.first().cloned().unwrap_or_default();
        let excluded = overlay.excluded.iter().any(|e| e == &joined || e == &first);
        if !excluded
            && !overlay
                .ops
                .iter()
                .any(|op| op.command.split_whitespace().eq(path.iter().map(String::as_str)))
        {
            out.push(format!("clap command `{joined}` is neither published nor excluded"));
        }
    }
    for entry in &overlay.excluded {
        let words: Vec<String> = entry.split_whitespace().map(str::to_string).collect();
        if words.is_empty() || find_path(&root, &words).is_none() {
            out.push(format!("excluded command `{entry}` is not a clap command"));
        }
    }
    let tool = overlay.tool.clone().unwrap_or_else(|| command.get_name().to_string());
    for op in &overlay.ops {
        if let Err(e) = plan(&root, &overlay, op, &tool, None) {
            out.push(e);
        }
    }
    if !out.is_empty() {
        return out;
    }
    let bridge = match super::Bridge::new(command.clone(), overlay_toml) {
        Ok(bridge) => bridge,
        Err(e) => return vec![e],
    };
    match bridge.schema() {
        Err(e) => out.push(e),
        Ok(doc) => {
            let value = serde_json::to_value(&doc).unwrap_or_default();
            if let Err(issues) = crate::publish::parse_published_schema(&value) {
                for issue in issues {
                    out.push(format!("published document: {}", issue.message));
                }
            }
        }
    }
    out
}

/// Panic with every [`coverage_problems`] entry. Call it from the tool's tests:
///
/// ```ignore
/// #[test]
/// fn lmapi_overlay_covers_the_cli() {
///     lmrust::clap_bridge::assert_coverage(&cli::command(), include_str!("../lmapi/overlay.toml"));
/// }
/// ```
pub fn assert_coverage(command: &Command, overlay_toml: &str) {
    let problems = coverage_problems(command, overlay_toml);
    assert!(
        problems.is_empty(),
        "lmapi overlay coverage:\n  {}",
        problems.join("\n  ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subcommand_aliases_count_as_legacy_names() {
        let cmd = Command::new("t")
            .subcommand(Command::new("list").visible_alias("ls"))
            .subcommand(Command::new("show").alias("cat"));
        for name in ["ls", "cat", "show"] {
            let toml =
                format!("excluded = [\"show\"]\n[[op]]\nname = \"{name}\"\ncommand = \"list\"\naccess = \"read\"\n");
            let problems = coverage_problems(&cmd, &toml).join("\n");
            assert!(
                problems.contains(&format!("op name {name} equals a legacy subcommand name")),
                "{problems}"
            );
        }
        assert!(coverage_problems(
            &cmd,
            "excluded = [\"show\"]\n[[op]]\nname = \"list_all\"\ncommand = \"list\"\naccess = \"read\"\n"
        )
        .is_empty());
    }
}
