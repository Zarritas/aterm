//! Project commands: a per-project catalogue gathered from the repo —
//! the agent's slash-commands (`.claude/commands/**`), and the project's own
//! scripts (package.json, Makefile, justfile, Cargo). The panel offers them in
//! a picker so you can launch a command in a terminal rooted at the project.

use std::path::Path;

/// How a discovered command is launched.
#[derive(Clone, Debug, PartialEq)]
pub enum RunSpec {
    /// Run a shell command line in the project cwd.
    Shell(String),
    /// Launch the agent (its `new_session_argv`) and send a `/slash` command.
    Slash(String),
}

/// One launchable project command.
#[derive(Clone, Debug)]
pub struct ProjectCommand {
    pub label: String,
    pub detail: String,
    pub run: RunSpec,
}

/// Discover every command available for `cwd` (best-effort; missing files are
/// simply skipped). Ordered: slash-commands, then npm/make/just/cargo scripts.
pub fn discover(cwd: &Path) -> Vec<ProjectCommand> {
    let mut out = Vec::new();
    discover_slash(cwd, &mut out);

    if let Ok(text) = std::fs::read_to_string(cwd.join("package.json")) {
        let pm = detect_pm(cwd);
        for name in parse_package_scripts(&text) {
            out.push(ProjectCommand {
                label: format!("{pm} run {name}"),
                detail: "package.json".to_string(),
                run: RunSpec::Shell(format!("{pm} run {name}")),
            });
        }
    }
    if let Ok(text) = std::fs::read_to_string(cwd.join("Makefile")) {
        for target in parse_makefile_targets(&text) {
            out.push(ProjectCommand {
                label: format!("make {target}"),
                detail: "Makefile".to_string(),
                run: RunSpec::Shell(format!("make {target}")),
            });
        }
    }
    for jf in ["justfile", "Justfile", ".justfile"] {
        if let Ok(text) = std::fs::read_to_string(cwd.join(jf)) {
            for recipe in parse_justfile_recipes(&text) {
                out.push(ProjectCommand {
                    label: format!("just {recipe}"),
                    detail: "justfile".to_string(),
                    run: RunSpec::Shell(format!("just {recipe}")),
                });
            }
            break;
        }
    }
    if cwd.join("Cargo.toml").is_file() {
        for sub in ["check", "build", "test", "run"] {
            out.push(ProjectCommand {
                label: format!("cargo {sub}"),
                detail: "Cargo".to_string(),
                run: RunSpec::Shell(format!("cargo {sub}")),
            });
        }
    }
    out
}

/// `.claude/commands/**/*.md` → namespaced slash commands (subdir as `sub:name`),
/// description pulled from the YAML frontmatter when present.
fn discover_slash(cwd: &Path, out: &mut Vec<ProjectCommand>) {
    let root = cwd.join(".claude/commands");
    if !root.is_dir() {
        return;
    }
    let mut stack = vec![(root.clone(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let next = if prefix.is_empty() {
                    format!("{name}:")
                } else {
                    format!("{prefix}{name}:")
                };
                stack.push((path, next));
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                let name = format!("{prefix}{stem}");
                let desc = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| frontmatter_description(&t))
                    .unwrap_or_else(|| "slash-command".to_string());
                out.push(ProjectCommand {
                    label: format!("/{name}"),
                    detail: desc,
                    run: RunSpec::Slash(format!("/{name}")),
                });
            }
        }
    }
}

/// Pick the package manager from the lockfile present (npm by default).
fn detect_pm(cwd: &Path) -> &'static str {
    if cwd.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if cwd.join("yarn.lock").is_file() {
        "yarn"
    } else if cwd.join("bun.lockb").is_file() {
        "bun"
    } else {
        "npm"
    }
}

/// Keys of the `scripts` object in a `package.json`, in file order.
pub fn parse_package_scripts(json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    value
        .get("scripts")
        .and_then(|s| s.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Phony-ish target names from a Makefile (`name:` at column 0, skip patterns,
/// special targets and variable assignments).
pub fn parse_makefile_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t', '#', '.']) {
            continue;
        }
        let Some((head, _)) = line.split_once(':') else {
            continue;
        };
        let name = head.trim();
        if name.is_empty()
            || name.contains('=')
            || name.contains('%')
            || name.contains(' ')
            || name.contains('$')
        {
            continue;
        }
        if !out.iter().any(|t| t == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// Recipe names from a justfile (`name:` or `name args:` at column 0).
pub fn parse_justfile_recipes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        // Skip indented bodies, comments, attributes and `name := value`
        // settings/assignments (the `:=` is not a recipe colon).
        if line.starts_with([' ', '\t', '#', '@']) || !line.contains(':') || line.contains(":=") {
            continue;
        }
        let head = line.split(':').next().unwrap_or("").trim();
        let name = head.split_whitespace().next().unwrap_or("");
        if name.is_empty() || name.contains('=') {
            continue;
        }
        if !out.iter().any(|t| t == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// The `description:` value from a markdown YAML frontmatter block, if any.
pub fn frontmatter_description(md: &str) -> Option<String> {
    let body = md.strip_prefix("---")?;
    let end = body.find("\n---")?;
    for line in body[..end].lines() {
        if let Some(rest) = line.trim().strip_prefix("description:") {
            let v = rest.trim().trim_matches(['"', '\'']).trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_scripts_in_order() {
        let json = r#"{"scripts":{"build":"x","test":"y"},"name":"p"}"#;
        assert_eq!(parse_package_scripts(json), vec!["build", "test"]);
        assert!(parse_package_scripts("not json").is_empty());
        assert!(parse_package_scripts("{}").is_empty());
    }

    #[test]
    fn makefile_targets_skip_noise() {
        let mk = "VAR = 1\n.PHONY: all\nall: build\n\tcmd\nbuild:\n\tcmd\n%.o: %.c\n";
        let t = parse_makefile_targets(mk);
        assert_eq!(t, vec!["all", "build"]);
    }

    #[test]
    fn justfile_recipes() {
        let jf = "set shell := [\"bash\"]\nbuild:\n    cargo build\ntest args:\n    cargo test\n";
        let r = parse_justfile_recipes(jf);
        assert_eq!(r, vec!["build", "test"]);
    }

    #[test]
    fn frontmatter_desc() {
        let md = "---\ndescription: Hace algo\nmodel: x\n---\n# cuerpo\n";
        assert_eq!(frontmatter_description(md).as_deref(), Some("Hace algo"));
        assert!(frontmatter_description("sin frontmatter").is_none());
    }
}
