//! Private Pro features for aterm. Native port of `agent-sessions-pro/pro/index.ts`.
//!
//! Fase 1: **comparativa paralela con git worktrees** — lanza N agentes, cada
//! uno en su propio `git worktree`/rama, e inyecta un prompt común. Más
//! `compare` (diff/commits por worktree) y `cleanup`.
//!
//! Diferencia con la extensión: VS Code usa diálogos `async`; aquí egui es
//! modo-inmediato, así que el estado de cada diálogo vive en [`ProImpl`] y se
//! redibuja cada frame desde [`ProModule::ui`].

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aterm_pro_api::{ProHost, ProModule, ProviderLite};

/// Construye el módulo Pro real (lo llama el core con `--features pro`).
pub fn module() -> Box<dyn ProModule> {
    Box::new(ProImpl::default())
}

/// Un worktree de comparativa detectado (rama bajo `agents/`).
#[derive(Clone)]
struct Worktree {
    path: PathBuf,
    branch: String,
}

/// Diálogo "lanzar comparativa": elegir agentes + prompt común.
struct ParallelDialog {
    repo_root: PathBuf,
    /// (proveedor, ¿seleccionado?).
    picks: Vec<(ProviderLite, bool)>,
    prompt: String,
}

/// Diálogo "limpiar worktrees": elegir cuáles eliminar.
struct CleanupDialog {
    repo_root: PathBuf,
    /// (worktree, ¿marcado para borrar?).
    picks: Vec<(Worktree, bool)>,
}

#[derive(Default)]
struct ProImpl {
    parallel: Option<ParallelDialog>,
    cleanup: Option<CleanupDialog>,
}

/// Timestamp compacto en base36 (paridad con `Date.now().toString(36)`).
fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    to_base36(secs)
}

fn to_base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

/// Parsea `git worktree list --porcelain`, quedándose con las ramas `agents/*`
/// (las que crea la comparativa). Paridad con `parseWorktrees` de la extensión.
fn parse_worktrees(raw: &str) -> Vec<Worktree> {
    let mut trees = Vec::new();
    let mut cur_path: Option<String> = None;
    let mut cur_branch: Option<String> = None;
    let flush =
        |path: &mut Option<String>, branch: &mut Option<String>, trees: &mut Vec<Worktree>| {
            if let (Some(p), Some(b)) = (path.take(), branch.take()) {
                if b.starts_with("agents/") {
                    trees.push(Worktree {
                        path: PathBuf::from(p),
                        branch: b,
                    });
                }
            }
        };
    for line in raw.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            flush(&mut cur_path, &mut cur_branch, &mut trees);
            cur_path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            cur_branch = Some(b.trim_start_matches("refs/heads/").to_string());
        } else if line.trim().is_empty() {
            flush(&mut cur_path, &mut cur_branch, &mut trees);
        }
    }
    flush(&mut cur_path, &mut cur_branch, &mut trees);
    trees
}

/// Lista los worktrees de comparativa del repo.
fn list_worktrees(host: &dyn ProHost, repo_root: &Path) -> Result<Vec<Worktree>, String> {
    let raw = host.exec_git(&["worktree", "list", "--porcelain"], repo_root)?;
    Ok(parse_worktrees(&raw))
}

impl ProImpl {
    /// Gate Pro: true si está desbloqueado; si no, avisa + abre la compra.
    fn gate(host: &mut dyn ProHost, feature: &str) -> bool {
        if host.is_pro() {
            return true;
        }
        host.notify(format!(
            "«{feature}» requiere Aterm Pro. Abriendo la página…"
        ));
        host.open_buy();
        false
    }
}

impl ProModule for ProImpl {
    fn open_parallel(&mut self, host: &mut dyn ProHost) {
        if !Self::gate(host, "Comparativa paralela") {
            return;
        }
        let Some(repo_root) = host.repo_root() else {
            host.notify(
                "Abre una pestaña dentro de un repo git para lanzar una comparativa.".to_string(),
            );
            return;
        };
        let usable: Vec<ProviderLite> = host
            .providers()
            .into_iter()
            .filter(|p| p.available && !p.new_session_argv.is_empty())
            .collect();
        if usable.len() < 2 {
            host.notify(
                "Necesitas al menos 2 agentes en el PATH para una comparativa.".to_string(),
            );
            return;
        }
        self.parallel = Some(ParallelDialog {
            repo_root,
            picks: usable.into_iter().map(|p| (p, true)).collect(),
            prompt: String::new(),
        });
    }

    fn run_compare(&mut self, host: &mut dyn ProHost) {
        if !Self::gate(host, "Comparar worktrees") {
            return;
        }
        let Some(repo_root) = host.repo_root() else {
            host.notify("Abre primero una pestaña dentro del repo.".to_string());
            return;
        };
        let trees = match list_worktrees(host, &repo_root) {
            Ok(t) => t,
            Err(e) => {
                host.notify(format!("git: {e}"));
                return;
            }
        };
        if trees.is_empty() {
            host.notify("No hay worktrees de comparativa que comparar.".to_string());
            return;
        }
        let base_sha = host
            .exec_git(&["rev-parse", "HEAD"], &repo_root)
            .unwrap_or_default()
            .trim()
            .to_string();

        let mut md = String::from("# Comparativa de agentes\n\n");
        md.push_str(&format!(
            "Repo: `{}` · base: `HEAD` · {} agente(s)\n\n",
            repo_root.display(),
            trees.len()
        ));
        for t in &trees {
            md.push_str(&format!(
                "---\n\n## {}\n\n`{}`\n",
                t.branch,
                t.path.display()
            ));
            if let Ok(stat) = host.exec_git(&["diff", "--stat", "HEAD"], &t.path) {
                let stat = stat.trim();
                if !stat.is_empty() {
                    md.push_str(&format!("\n### Cambios sin commit\n\n```\n{stat}\n```\n"));
                }
            }
            if !base_sha.is_empty() {
                let range = format!("{base_sha}..HEAD");
                if let Ok(log) = host.exec_git(&["log", "--oneline", &range], &t.path) {
                    let log = log.trim();
                    if !log.is_empty() {
                        md.push_str(&format!("\n### Commits sobre HEAD\n\n```\n{log}\n```\n"));
                    }
                }
            }
        }
        host.show_report("Comparativa de agentes".to_string(), md);
    }

    fn open_cleanup(&mut self, host: &mut dyn ProHost) {
        if !Self::gate(host, "Limpiar worktrees") {
            return;
        }
        let Some(repo_root) = host.repo_root() else {
            host.notify("Abre primero una pestaña dentro del repo.".to_string());
            return;
        };
        let trees = match list_worktrees(host, &repo_root) {
            Ok(t) => t,
            Err(e) => {
                host.notify(format!("git: {e}"));
                return;
            }
        };
        if trees.is_empty() {
            host.notify("No hay worktrees de comparativa que limpiar.".to_string());
            return;
        }
        self.cleanup = Some(CleanupDialog {
            repo_root,
            picks: trees.into_iter().map(|t| (t, true)).collect(),
        });
    }

    fn ui(&mut self, ctx: &egui::Context, host: &mut dyn ProHost) {
        self.draw_parallel(ctx, host);
        self.draw_cleanup(ctx, host);
    }

    fn edition(&self) -> &'static str {
        "Pro"
    }
}

impl ProImpl {
    fn draw_parallel(&mut self, ctx: &egui::Context, host: &mut dyn ProHost) {
        let Some(dlg) = &mut self.parallel else {
            return;
        };
        let mut open = true;
        let mut launch = false;
        let mut cancel = false;
        egui::Window::new("⚡ Comparativa paralela")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("Repo: {}", dlg.repo_root.display()));
                ui.label("Un worktree + rama por agente; el prompt se pega tras 2,5 s.");
                ui.separator();
                ui.label("Agentes:");
                for (p, sel) in &mut dlg.picks {
                    ui.checkbox(sel, &p.display_name);
                }
                ui.separator();
                ui.label("Prompt inicial (opcional):");
                ui.add(
                    egui::TextEdit::multiline(&mut dlg.prompt)
                        .desired_rows(3)
                        .desired_width(360.0)
                        .hint_text("p. ej. Refactoriza term/mod.rs para extraer la selección"),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    let n = dlg.picks.iter().filter(|(_, s)| *s).count();
                    if ui
                        .add_enabled(n >= 2, egui::Button::new(format!("Lanzar {n} agentes")))
                        .clicked()
                    {
                        launch = true;
                    }
                    if ui.button("Cancelar").clicked() {
                        cancel = true;
                    }
                });
            });

        if launch {
            self.launch_parallel(host);
            self.parallel = None;
        } else if cancel || !open {
            self.parallel = None;
        }
    }

    fn launch_parallel(&mut self, host: &mut dyn ProHost) {
        let Some(dlg) = &self.parallel else { return };
        let repo_root = dlg.repo_root.clone();
        let prompt = dlg.prompt.trim().to_string();
        let stamp = stamp();
        let parent = repo_root.parent().unwrap_or(&repo_root).to_path_buf();
        let repo_name = repo_root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());

        let mut launched: Vec<String> = Vec::new();
        for (p, _) in dlg.picks.iter().filter(|(_, s)| *s) {
            let id = &p.id;
            let worktree_path = parent.join(format!("{repo_name}-{id}-{stamp}"));
            let branch = format!("agents/{id}-{stamp}");
            let wt_str = worktree_path.to_string_lossy().to_string();
            // git worktree add -B agents/<id>-<stamp> <path> HEAD
            if let Err(e) = host.exec_git(
                &["worktree", "add", "-B", &branch, &wt_str, "HEAD"],
                &repo_root,
            ) {
                host.notify(format!(
                    "No se pudo crear worktree para {}: {e}",
                    p.display_name
                ));
                continue;
            }
            if let Some(tab_id) = host.open_agent(p.new_session_argv.clone(), worktree_path) {
                if !prompt.is_empty() {
                    host.inject_prompt(tab_id, prompt.clone(), 2500);
                }
            }
            launched.push(branch);
        }
        if launched.is_empty() {
            return;
        }
        host.notify(format!(
            "Lanzados {} agentes en worktrees bajo {}. Ramas: {}.",
            launched.len(),
            parent.display(),
            launched.join(", ")
        ));
    }

    fn draw_cleanup(&mut self, ctx: &egui::Context, host: &mut dyn ProHost) {
        let Some(dlg) = &mut self.cleanup else { return };
        let mut open = true;
        let mut do_remove = false;
        let mut cancel = false;
        egui::Window::new("🗑 Limpiar worktrees")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Worktrees de comparativa a eliminar:");
                for (t, sel) in &mut dlg.picks {
                    ui.checkbox(sel, format!("{}  ·  {}", t.branch, t.path.display()));
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let n = dlg.picks.iter().filter(|(_, s)| *s).count();
                    if ui
                        .add_enabled(n > 0, egui::Button::new(format!("Eliminar {n}")))
                        .clicked()
                    {
                        do_remove = true;
                    }
                    if ui.button("Cancelar").clicked() {
                        cancel = true;
                    }
                });
            });

        if do_remove {
            self.run_cleanup(host);
            self.cleanup = None;
        } else if cancel || !open {
            self.cleanup = None;
        }
    }

    fn run_cleanup(&mut self, host: &mut dyn ProHost) {
        let Some(dlg) = &self.cleanup else { return };
        let repo_root = dlg.repo_root.clone();
        let mut removed = 0usize;
        for (t, _) in dlg.picks.iter().filter(|(_, s)| *s) {
            let path = t.path.to_string_lossy().to_string();
            // git worktree remove --force <path> ; git branch -D <branch>
            if let Err(e) = host.exec_git(&["worktree", "remove", "--force", &path], &repo_root) {
                host.notify(format!("No se pudo eliminar {}: {e}", t.branch));
                continue;
            }
            let _ = host.exec_git(&["branch", "-D", &t.branch], &repo_root);
            removed += 1;
        }
        host.notify(format!("Limpiados {removed} worktree(s)."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base36_matches_js_semantics() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
    }

    #[test]
    fn parse_worktrees_keeps_only_agent_branches() {
        let raw = "\
worktree /home/u/proj
HEAD aaaa
branch refs/heads/main

worktree /home/u/proj-claude-1a2b
HEAD bbbb
branch refs/heads/agents/claude-1a2b

worktree /home/u/proj-codex-1a2b
HEAD cccc
branch refs/heads/agents/codex-1a2b
";
        let trees = parse_worktrees(raw);
        assert_eq!(trees.len(), 2);
        assert_eq!(trees[0].branch, "agents/claude-1a2b");
        assert_eq!(trees[0].path, PathBuf::from("/home/u/proj-claude-1a2b"));
        assert_eq!(trees[1].branch, "agents/codex-1a2b");
    }
}
