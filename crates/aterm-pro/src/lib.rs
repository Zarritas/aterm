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

/// A tab in a saved profile (serde mirror of `TabSnapshot`, which the
/// dependency-light contract crate doesn't derive serde for).
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct ProfileTab {
    argv: Vec<String>,
    cwd: Option<String>,
    key: Option<String>,
    name: Option<String>,
}

/// A saved workspace profile: a named set of tabs to reopen.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Profile {
    name: String,
    tabs: Vec<ProfileTab>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ProfileStore {
    profiles: Vec<Profile>,
}

#[derive(Default)]
struct ProImpl {
    parallel: Option<ParallelDialog>,
    cleanup: Option<CleanupDialog>,
    /// Pro features hub window.
    hub_open: bool,
    /// Workspace-profiles window + draft name for "save current as".
    profiles_open: bool,
    profile_name: String,
    /// Dashboard window.
    dashboard_open: bool,
    /// Export-to-HTML session picker window.
    export_open: bool,
    /// Port-session window + chosen target provider id.
    port_open: bool,
    port_target: String,
    /// One-shot flags set from the hub, run (with host) in `ui`.
    run_memory: bool,
    run_mcp: bool,
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

    fn open_features(&mut self, host: &mut dyn ProHost) {
        if !Self::gate(host, "Funciones Pro") {
            return;
        }
        self.hub_open = true;
    }

    fn ui(&mut self, ctx: &egui::Context, host: &mut dyn ProHost) {
        self.draw_parallel(ctx, host);
        self.draw_cleanup(ctx, host);
        self.draw_hub(ctx);
        self.draw_profiles(ctx, host);
        self.draw_dashboard(ctx, host);
        self.draw_export(ctx, host);
        self.draw_port(ctx, host);
        if std::mem::take(&mut self.run_memory) {
            self.run_memory_graph(host);
        }
        if std::mem::take(&mut self.run_mcp) {
            self.run_mcp_config(host);
        }
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

    // ── Fase 4: Pro features hub ─────────────────────────────────────────

    fn draw_hub(&mut self, ctx: &egui::Context) {
        if !self.hub_open {
            return;
        }
        let mut open = true;
        egui::Window::new("✦ Funciones Pro")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_width(260.0);
                // Full-width, evenly-sized action buttons.
                let item = |ui: &mut egui::Ui, label: &str| -> bool {
                    ui.add_sized([ui.available_width(), 28.0], egui::Button::new(label))
                        .clicked()
                };
                if item(ui, "📁 Perfiles de espacio de trabajo") {
                    self.profiles_open = true;
                }
                if item(ui, "📊 Dashboard") {
                    self.dashboard_open = true;
                }
                if item(ui, "🖺 Exportar conversación a HTML") {
                    self.export_open = true;
                }
                if item(ui, "⇄ Portar sesión a otro proveedor") {
                    self.port_open = true;
                }
                if item(ui, "🕸 Memory graph (CLAUDE.md)") {
                    self.run_memory = true;
                }
                if item(ui, "🔌 Configurar MCP") {
                    self.run_mcp = true;
                }
            });
        self.hub_open = open;
    }

    fn profiles_path(host: &dyn ProHost) -> std::path::PathBuf {
        host.config_dir().join("profiles.json")
    }

    fn load_profiles(host: &dyn ProHost) -> ProfileStore {
        std::fs::read_to_string(Self::profiles_path(host))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save_profiles(host: &mut dyn ProHost, store: &ProfileStore) {
        let json = serde_json::to_string_pretty(store).unwrap_or_default();
        if let Err(e) = host.write_file(&Self::profiles_path(host), &json) {
            host.notify(format!("No se pudo guardar perfiles: {e}"));
        }
    }

    fn draw_profiles(&mut self, ctx: &egui::Context, host: &mut dyn ProHost) {
        if !self.profiles_open {
            return;
        }
        let mut store = Self::load_profiles(host);
        let mut open = true;
        let mut to_open: Option<usize> = None;
        let mut to_delete: Option<usize> = None;
        let mut save_current = false;
        egui::Window::new("📁 Perfiles de espacio de trabajo")
            .open(&mut open)
            .resizable(true)
            .default_size([420.0, 320.0])
            .show(ctx, |ui| {
                if store.profiles.is_empty() {
                    ui.weak("Sin perfiles. Guarda las pestañas abiertas como un perfil.");
                }
                for (i, p) in store.profiles.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.button("▶").on_hover_text("Abrir perfil").clicked() {
                            to_open = Some(i);
                        }
                        if ui.small_button("✕").clicked() {
                            to_delete = Some(i);
                        }
                        ui.label(egui::RichText::new(&p.name).strong());
                        ui.weak(format!("{} pestañas", p.tabs.len()));
                    });
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.profile_name)
                            .hint_text("nombre del perfil")
                            .desired_width(180.0),
                    );
                    if ui
                        .add_enabled(
                            !self.profile_name.trim().is_empty(),
                            egui::Button::new("Guardar pestañas actuales"),
                        )
                        .clicked()
                    {
                        save_current = true;
                    }
                });
            });

        if save_current {
            let tabs: Vec<ProfileTab> = host
                .current_tabs()
                .into_iter()
                .map(|t| ProfileTab {
                    argv: t.argv,
                    cwd: t.cwd,
                    key: t.key,
                    name: t.name,
                })
                .collect();
            store.profiles.push(Profile {
                name: self.profile_name.trim().to_string(),
                tabs,
            });
            Self::save_profiles(host, &store);
            self.profile_name.clear();
            host.notify("Perfil guardado".to_string());
        }
        if let Some(i) = to_delete {
            if i < store.profiles.len() {
                store.profiles.remove(i);
                Self::save_profiles(host, &store);
            }
        }
        if let Some(i) = to_open {
            if let Some(p) = store.profiles.get(i) {
                for t in &p.tabs {
                    let cwd = t.cwd.clone().map(std::path::PathBuf::from);
                    host.open_agent(t.argv.clone(), cwd.unwrap_or_else(|| ".".into()));
                }
                host.notify(format!("Perfil «{}» abierto", p.name));
            }
        }
        self.profiles_open = open;
    }

    fn draw_dashboard(&mut self, ctx: &egui::Context, host: &mut dyn ProHost) {
        if !self.dashboard_open {
            return;
        }
        let sessions = host.sessions();
        let mut by_provider: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut total_msgs: u64 = 0;
        for s in &sessions {
            *by_provider.entry(s.provider.clone()).or_default() += 1;
            total_msgs += s.message_count.unwrap_or(0);
        }
        let mut open = true;
        let mut export_csv = false;
        egui::Window::new("📊 Dashboard")
            .open(&mut open)
            .resizable(true)
            .default_size([420.0, 360.0])
            .show(ctx, |ui| {
                ui.heading(format!("{} sesiones", sessions.len()));
                ui.label(format!("{total_msgs} mensajes en total"));
                ui.separator();
                ui.label("Por proveedor:");
                for (p, n) in &by_provider {
                    ui.label(format!("  {p}: {n}"));
                }
                ui.separator();
                if ui.button("⇩ Exportar CSV").clicked() {
                    export_csv = true;
                }
            });
        if export_csv {
            let mut csv = String::from("provider,id,title,cwd,model,messages,last_activity\n");
            for s in &sessions {
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{}\n",
                    s.provider,
                    s.id,
                    csv_field(s.title.as_deref().unwrap_or("")),
                    csv_field(s.cwd.as_deref().unwrap_or("")),
                    s.model.as_deref().unwrap_or(""),
                    s.message_count.unwrap_or(0),
                    s.last_activity,
                ));
            }
            let dest = host.config_dir().join("exports").join("dashboard.csv");
            match host.write_file(&dest, &csv) {
                Ok(()) => {
                    host.notify(format!("CSV exportado → {}", dest.display()));
                    host.open_path(&dest.to_string_lossy());
                }
                Err(e) => host.notify(format!("Export CSV falló: {e}")),
            }
        }
        self.dashboard_open = open;
    }

    fn draw_export(&mut self, ctx: &egui::Context, host: &mut dyn ProHost) {
        if !self.export_open {
            return;
        }
        let sessions = host.sessions();
        let mut open = true;
        let mut pick: Option<(String, String, String)> = None;
        egui::Window::new("🖺 Exportar conversación a HTML")
            .open(&mut open)
            .resizable(true)
            .default_size([460.0, 360.0])
            .show(ctx, |ui| {
                ui.weak("Elige una sesión para exportarla a HTML:");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for s in &sessions {
                        let title = s.title.clone().unwrap_or_else(|| s.id.clone());
                        if ui
                            .button(format!("[{}] {}", s.provider, truncate(&title, 48)))
                            .clicked()
                        {
                            pick = Some((s.provider.clone(), s.id.clone(), title));
                        }
                    }
                });
            });
        if let Some((provider, id, title)) = pick {
            match host.transcript(&provider, &id) {
                Some(turns) => {
                    let html = render_html(&title, &turns);
                    let dest = host
                        .config_dir()
                        .join("exports")
                        .join(format!("{provider}-{}.html", safe_name(&id)));
                    match host.write_file(&dest, &html) {
                        Ok(()) => {
                            host.notify(format!("HTML exportado → {}", dest.display()));
                            host.open_path(&dest.to_string_lossy());
                        }
                        Err(e) => host.notify(format!("Export HTML falló: {e}")),
                    }
                }
                None => host.notify("No se pudo leer la conversación".to_string()),
            }
            self.export_open = false;
        } else {
            self.export_open = open;
        }
    }

    fn draw_port(&mut self, ctx: &egui::Context, host: &mut dyn ProHost) {
        if !self.port_open {
            return;
        }
        let sessions = host.sessions();
        let providers: Vec<ProviderLite> = host
            .providers()
            .into_iter()
            .filter(|p| p.available && !p.new_session_argv.is_empty())
            .collect();
        if self.port_target.is_empty() {
            if let Some(p) = providers.first() {
                self.port_target = p.id.clone();
            }
        }
        let mut open = true;
        let mut pick: Option<(String, String, Option<String>)> = None;
        egui::Window::new("⇄ Portar sesión a otro proveedor")
            .open(&mut open)
            .resizable(true)
            .default_size([480.0, 380.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Destino:");
                    egui::ComboBox::from_id_salt("port-target")
                        .selected_text(
                            providers
                                .iter()
                                .find(|p| p.id == self.port_target)
                                .map(|p| p.display_name.as_str())
                                .unwrap_or("—"),
                        )
                        .show_ui(ui, |ui| {
                            for p in &providers {
                                ui.selectable_value(
                                    &mut self.port_target,
                                    p.id.clone(),
                                    &p.display_name,
                                );
                            }
                        });
                });
                ui.weak("La conversación se inyecta como contexto en una sesión nueva.");
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for s in &sessions {
                        let title = s.title.clone().unwrap_or_else(|| s.id.clone());
                        if ui
                            .button(format!("[{}] {}", s.provider, truncate(&title, 44)))
                            .clicked()
                        {
                            pick = Some((s.provider.clone(), s.id.clone(), s.cwd.clone()));
                        }
                    }
                });
            });
        if let Some((provider, id, cwd)) = pick {
            let argv = providers
                .iter()
                .find(|p| p.id == self.port_target)
                .map(|p| p.new_session_argv.clone());
            match (host.transcript(&provider, &id), argv) {
                (Some(turns), Some(argv)) => {
                    let context = port_context(&turns);
                    let cwd = cwd
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| ".".into());
                    if let Some(tab_id) = host.open_agent(argv, cwd) {
                        host.inject_prompt(tab_id, context, 2500);
                        host.notify(format!("Portada a {}", self.port_target));
                    }
                }
                _ => host.notify("No se pudo portar la sesión".to_string()),
            }
            self.port_open = false;
        } else {
            self.port_open = open;
        }
    }

    fn run_memory_graph(&mut self, host: &mut dyn ProHost) {
        let Some(root) = host.repo_root() else {
            host.notify("Abre una pestaña dentro de un repo para el memory graph.".to_string());
            return;
        };
        let md = std::fs::read_to_string(root.join("CLAUDE.md")).unwrap_or_default();
        if md.is_empty() {
            host.notify("No hay CLAUDE.md en este repo.".to_string());
            return;
        }
        let mut report = format!("# Memory graph — {}\n\n", root.display());
        let imports: Vec<&str> = md
            .lines()
            .filter_map(|l| l.trim().strip_prefix('@'))
            .collect();
        let links: Vec<String> = md
            .match_indices("[[")
            .filter_map(|(i, _)| {
                md[i + 2..]
                    .find("]]")
                    .map(|j| md[i + 2..i + 2 + j].to_string())
            })
            .collect();
        report.push_str(&format!("Líneas: {}\n\n", md.lines().count()));
        report.push_str("## Imports (@)\n\n");
        if imports.is_empty() {
            report.push_str("_(ninguno)_\n");
        } else {
            for im in &imports {
                report.push_str(&format!("- `{im}`\n"));
            }
        }
        report.push_str("\n## Enlaces [[…]]\n\n");
        if links.is_empty() {
            report.push_str("_(ninguno)_\n");
        } else {
            for l in &links {
                report.push_str(&format!("- [[{l}]]\n"));
            }
        }
        host.show_report("Memory graph".to_string(), report);
    }

    fn run_mcp_config(&mut self, host: &mut dyn ProHost) {
        let snippet = "{\n  \"mcpServers\": {\n    \"agent-sessions\": {\n      \
            \"command\": \"agent-sessions-cli\",\n      \"args\": [\"serve\"]\n    }\n  }\n}\n";
        if let Some(root) = host.repo_root() {
            let dest = root.join(".mcp.json");
            match host.write_file(&dest, snippet) {
                Ok(()) => host.notify(format!("MCP escrito → {}", dest.display())),
                Err(e) => host.notify(format!("MCP falló: {e}")),
            }
        }
        host.show_report(
            "Configurar MCP".to_string(),
            format!(
                "Servidor MCP de Agent Sessions (busca en tu propio historial).\n\n\
                 Escrito `.mcp.json` en el repo si había uno abierto. Snippet:\n\n```json\n{snippet}```\n"
            ),
        );
    }
}

/// Truncate with an ellipsis for compact labels.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// Quote a CSV field if it contains a comma, quote or newline.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// A filesystem-safe version of an id (for export filenames).
fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Minimal HTML escaping.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render a conversation as a self-contained HTML document.
fn render_html(title: &str, turns: &[aterm_pro_api::Turn]) -> String {
    let mut body = String::new();
    for t in turns {
        body.push_str(&format!(
            "<div class=\"turn {}\"><div class=\"role\">{}</div><pre>{}</pre></div>\n",
            html_escape(&t.role),
            html_escape(&t.role),
            html_escape(&t.text),
        ));
    }
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:820px;margin:2rem auto;\
         padding:0 1rem;background:#1e1e2e;color:#cdd6f4}}.turn{{margin:1rem 0;border-radius:8px;\
         padding:.6rem .9rem;background:#313244}}.role{{font-weight:600;color:#89b4fa;\
         text-transform:uppercase;font-size:.75rem;margin-bottom:.3rem}}\
         pre{{white-space:pre-wrap;word-wrap:break-word;margin:0;font-family:ui-monospace,monospace}}\
         </style></head><body><h1>{}</h1>{}</body></html>",
        html_escape(title),
        html_escape(title),
        body
    )
}

/// Build a context prompt from a transcript for porting to another provider
/// (truncated so it stays a reasonable paste).
fn port_context(turns: &[aterm_pro_api::Turn]) -> String {
    let mut out = String::from(
        "Contexto de una conversación previa que estoy migrando. Continúa desde aquí.\n\n",
    );
    for t in turns.iter().rev().take(20).rev() {
        out.push_str(&format!("[{}] {}\n\n", t.role, truncate(&t.text, 1200)));
    }
    out
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
