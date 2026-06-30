//! The agent-session panel: scan providers, list sessions with rich metadata,
//! filter, preview conversations, rename/tag/colour, export/import and delete.
//!
//! All the heavy lifting lives in the `agent-sessions` crate (read-only
//! discovery + `MetadataStore` + `transfer`); this module is UI wiring.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_sessions::{
    all_providers, encode_cwd, export_sessions, import_archive, import_archive_routed, parse_tags,
    transfer::move_session,
    types::{AgentSession, DeleteError, PreviewTurn, ProviderQuota},
    AgentProvider, ExportItem, MetadataStore,
};
use eframe::egui;

/// How sessions are bucketed in the list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupMode {
    /// One section per provider (Claude/Codex/…).
    Provider,
    /// One section per working directory, across providers.
    Project,
    /// Two levels: provider → project (working directory).
    Cascade,
}

/// User-assigned display names for project directories, keyed by absolute path.
/// Persisted separately from the vendored `MetadataStore` so the export/import
/// manifest stays byte-compatible.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ProjectNames {
    names: std::collections::HashMap<String, String>,
    /// Per-project accent colour as `#rrggbb` (optional).
    #[serde(default)]
    colors: std::collections::HashMap<String, String>,
}

impl ProjectNames {
    fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    fn get(&self, path: &str) -> Option<&str> {
        self.names.get(path).map(String::as_str)
    }

    fn set(&mut self, path: &str, name: String) {
        if name.trim().is_empty() {
            self.names.remove(path);
        } else {
            self.names.insert(path.to_string(), name.trim().to_string());
        }
    }

    fn color(&self, path: &str) -> Option<egui::Color32> {
        self.colors.get(path).and_then(|c| parse_hex(c))
    }

    fn set_color(&mut self, path: &str, hex: Option<String>) {
        match hex {
            Some(h) => {
                self.colors.insert(path.to_string(), h);
            }
            None => {
                self.colors.remove(path);
            }
        }
    }
}

/// What the panel asks the host app to do (open a PTY tab).
pub enum PanelAction {
    Open {
        argv: Vec<String>,
        cwd: Option<PathBuf>,
        /// Stable `provider:id` identity for *resume* opens, so the host can
        /// focus an existing tab instead of resuming the same session twice
        /// (two agents writing one transcript would corrupt it). `None` for
        /// fresh shells / new sessions, which may be opened repeatedly.
        key: Option<String>,
    },
    /// Launch a saved template: open `argv` in `cwd`, then inject `prompt`
    /// (after a short delay, no Enter — the user reviews it) when non-empty.
    OpenTemplate {
        argv: Vec<String>,
        cwd: Option<PathBuf>,
        prompt: Option<String>,
    },
}

/// One provider's scan result plus the live trait object (for preview/delete/
/// quota) and its account quota snapshot.
struct ProviderGroup {
    provider: Box<dyn AgentProvider>,
    display_name: String,
    sessions: Vec<AgentSession>,
    quota: Option<ProviderQuota>,
    status: Option<agent_sessions::types::ServiceStatus>,
    error: Option<String>,
}

/// In-flight rename/tags/colour/notes/favourite edit for one `(provider, id)`.
struct EditState {
    provider: String,
    id: String,
    name: String,
    tags: String,
    color: String,
    notes: String,
    favorite: bool,
}

/// Loaded conversation preview for the inspector window.
struct PreviewState {
    title: String,
    turns: Result<Vec<PreviewTurn>, String>,
}

/// In-flight "move a Claude session to another project" dialog.
struct MoveState {
    id: String,
    source_cwd: String,
    is_live: bool,
    /// Destination path draft (free text + autocomplete).
    dest: String,
}

/// Result of a background full-text search: the query it ran for, plus the set
/// of `(provider, id)` whose transcript matched.
type FtsResult = (String, std::collections::HashSet<(String, String)>);

pub struct SessionPanel {
    groups: Vec<ProviderGroup>,
    /// Set once a scan has populated `groups` at least once.
    scanned: bool,
    /// Channel carrying the result of an in-flight background scan, if any.
    scan_rx: Option<Receiver<Vec<ProviderGroup>>>,
    /// When the last scan finished, to drive periodic auto-refresh.
    last_scan_at: Option<std::time::Instant>,
    filter: String,
    /// Active full-text query (Some once a content search has run for `filter`).
    fts_query: Option<String>,
    /// `(provider, id)` of sessions whose transcript matched `fts_query`.
    fts_matches: std::collections::HashSet<(String, String)>,
    /// In-flight content search.
    fts_rx: Option<Receiver<FtsResult>>,
    group_mode: GroupMode,
    /// Active "folder": when set, only sessions carrying this tag are shown.
    tag_filter: Option<String>,
    /// Show only sessions the provider reports as live.
    only_active: bool,
    /// One-frame override to force every header open (`Some(true)`) or closed
    /// (`Some(false)`); cleared after applying.
    force_open: Option<bool>,
    metadata: MetadataStore,
    metadata_path: PathBuf,
    projects: ProjectNames,
    projects_path: PathBuf,
    /// In-flight project rename: (absolute path, draft name).
    project_edit: Option<(String, String)>,
    edit: Option<EditState>,
    preview: Option<PreviewState>,
    move_select: Option<MoveState>,
    import_path: String,
    /// Import destination provider id (only "claude" is wired today).
    import_provider: String,
    /// Import destination project: `None` routes each session to its recorded
    /// cwd; `Some(path)` forces every imported session into that project.
    import_project: Option<String>,
    /// Draft path for "new session in another directory" (shared across the
    /// per-provider new-session menus; transient).
    new_session_path: String,
    status: Option<String>,
    /// Saved launch templates (shared file with the sidecar/extension).
    templates: crate::templates::TemplateStore,
    /// Whether the templates manager window is open.
    templates_open: bool,
    /// In-flight "save new template" form (when `Some`, the form is shown).
    template_form: Option<TemplateForm>,
}

/// Draft fields for the "save a launch template" form.
#[derive(Default)]
struct TemplateForm {
    name: String,
    provider: String,
    prompt: String,
    cwd: String,
    tags: String,
}

impl Default for SessionPanel {
    fn default() -> Self {
        let metadata_path = metadata_path();
        let metadata = MetadataStore::load(&metadata_path);
        let projects_path = config_dir().join("project-names.json");
        let projects = ProjectNames::load(&projects_path);
        Self {
            groups: Vec::new(),
            scanned: false,
            scan_rx: None,
            last_scan_at: None,
            filter: String::new(),
            fts_query: None,
            fts_matches: std::collections::HashSet::new(),
            fts_rx: None,
            group_mode: GroupMode::Provider,
            tag_filter: None,
            only_active: false,
            force_open: None,
            metadata,
            metadata_path,
            projects,
            projects_path,
            project_edit: None,
            edit: None,
            preview: None,
            move_select: None,
            import_path: String::new(),
            import_provider: "claude".to_string(),
            import_project: None,
            new_session_path: String::new(),
            status: None,
            templates: crate::templates::TemplateStore::load(),
            templates_open: false,
            template_form: None,
        }
    }
}

impl SessionPanel {
    /// Kick off a provider scan on a background thread. `list_sessions` shells
    /// out (opencode takes ~2-3s) and `quota` reads files, so doing it on the
    /// UI thread freezes the window — the OS then shows a "not responding"
    /// dialog. The thread repaints the context when it finishes.
    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.scan_rx.is_some() {
            return; // a scan is already running
        }
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let groups = scan_all_providers();
            let _ = tx.send(groups);
            ctx.request_repaint();
        });
    }

    /// Adopt a finished scan's results, if the background thread has delivered.
    fn poll_scan(&mut self) {
        if let Some(rx) = &self.scan_rx {
            if let Ok(groups) = rx.try_recv() {
                self.groups = groups;
                self.scanned = true;
                self.scan_rx = None;
                self.last_scan_at = Some(std::time::Instant::now());
            }
        }
    }

    /// Auto-refresh interval from settings (kept sane, never under 15s).
    fn refresh_every() -> std::time::Duration {
        std::time::Duration::from_secs(crate::settings::get().refresh_secs.max(15))
    }

    fn maybe_auto_refresh(&mut self, ctx: &egui::Context) {
        if self.scan_rx.is_some() {
            return;
        }
        let stale = self
            .last_scan_at
            .is_some_and(|t| t.elapsed() >= Self::refresh_every());
        if stale {
            self.start_scan(ctx);
        }
    }

    /// Force a re-scan on the next frame (used after settings change).
    pub fn request_rescan(&mut self) {
        self.scanned = false;
    }

    /// Launch a full-text search of session transcripts for the current filter,
    /// off-thread (reading each `.jsonl` is slow). Repaints when done.
    fn start_fts(&mut self, ctx: &egui::Context) {
        let query = self.filter.trim().to_lowercase();
        if query.is_empty() {
            self.clear_fts();
            return;
        }
        let ids: Vec<(String, String)> = self
            .groups
            .iter()
            .flat_map(|g| {
                let pid = g.provider.id().to_string();
                g.sessions.iter().map(move |s| (pid.clone(), s.id.clone()))
            })
            .collect();
        let (tx, rx) = mpsc::channel();
        self.fts_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let providers = all_providers();
            let mut matches = std::collections::HashSet::new();
            for (pid, sid) in ids {
                if let Some(p) = providers.iter().find(|p| p.id() == pid) {
                    if let Some(content) = p.fts_content(&sid) {
                        if content.to_lowercase().contains(&query) {
                            matches.insert((pid, sid));
                        }
                    }
                }
            }
            let _ = tx.send((query, matches));
            ctx.request_repaint();
        });
    }

    fn clear_fts(&mut self) {
        self.fts_query = None;
        self.fts_matches.clear();
        self.fts_rx = None;
    }

    fn poll_fts(&mut self) {
        if let Some(rx) = &self.fts_rx {
            if let Ok((query, matches)) = rx.try_recv() {
                self.fts_query = Some(query);
                self.fts_matches = matches;
                self.fts_rx = None;
            }
        }
        // A changed filter invalidates a prior content search.
        if let Some(q) = &self.fts_query {
            if *q != self.filter.trim().to_lowercase() {
                self.clear_fts();
            }
        }
    }

    fn save_metadata(&mut self) {
        if let Err(e) = self.metadata.save(&self.metadata_path) {
            self.status = Some(format!("No se pudo guardar metadata: {e}"));
        }
    }

    /// Render the panel into `ui`; returns an action when the user resumes a
    /// session or starts a new one.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<PanelAction> {
        // First paint (and every re-scan) launches the scan off-thread so the
        // window never blocks on `list_sessions`.
        if !self.scanned && self.scan_rx.is_none() {
            self.start_scan(ui.ctx());
        }
        self.poll_scan();
        self.poll_fts();
        self.maybe_auto_refresh(ui.ctx());
        // Wake periodically so the auto-refresh fires even when idle.
        ui.ctx().request_repaint_after(Self::refresh_every());
        let scanning = self.scan_rx.is_some();
        let mut action = None;

        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new("Agent sessions").color(c_lavender()));
            let rescan = ui.add_enabled(!scanning, egui::Button::new("⟳"));
            if rescan.on_hover_text("Re-escanear").clicked() {
                self.start_scan(ui.ctx());
            }
            if ui
                .small_button("▾")
                .on_hover_text("Expandir todo")
                .clicked()
            {
                self.force_open = Some(true);
            }
            if ui
                .small_button("▸")
                .on_hover_text("Colapsar todo")
                .clicked()
            {
                self.force_open = Some(false);
            }
            if scanning {
                ui.spinner();
            }
        });

        ui.horizontal(|ui| {
            ui.label("Buscar:");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("filtrar…")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.horizontal(|ui| {
            let searching = self.fts_rx.is_some();
            if ui
                .add_enabled(!searching, egui::Button::new("⌕ en contenido"))
                .on_hover_text("Buscar el texto también dentro de las conversaciones")
                .clicked()
            {
                self.start_fts(ui.ctx());
            }
            if searching {
                ui.spinner();
            } else if self.fts_query.is_some() {
                ui.colored_label(
                    c_teal(),
                    format!("{} coincidencias en contenido", self.fts_matches.len()),
                );
                if ui
                    .small_button("✕")
                    .on_hover_text("Quitar búsqueda de contenido")
                    .clicked()
                {
                    self.clear_fts();
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("Agrupar:");
            ui.selectable_value(&mut self.group_mode, GroupMode::Provider, "Proveedor");
            ui.selectable_value(&mut self.group_mode, GroupMode::Project, "Proyecto");
            ui.selectable_value(
                &mut self.group_mode,
                GroupMode::Cascade,
                "Proveedor › Proyecto",
            );
        });

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.only_active, "● Solo activas");
            if ui
                .button("📋 Plantillas")
                .on_hover_text("Guardar / lanzar plantillas de sesión")
                .clicked()
            {
                self.templates_open = true;
            }
        });

        // Tag "folders": a row of chips that filter to one tag at a time.
        let tags = self.metadata.all_tags();
        if !tags.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Carpetas:");
                if ui
                    .selectable_label(self.tag_filter.is_none(), "Todas")
                    .clicked()
                {
                    self.tag_filter = None;
                }
                for tag in &tags {
                    let active = self.tag_filter.as_deref() == Some(tag.as_str());
                    if ui.selectable_label(active, format!("#{tag}")).clicked() {
                        self.tag_filter = if active { None } else { Some(tag.clone()) };
                    }
                }
            });
        }

        // Owned snapshots so the mutable import widgets below don't clash with
        // borrows of `self.groups` / `self.projects`. Reused by the scroll area.
        let all_projects: Vec<String> = {
            let mut v: Vec<String> = self
                .groups
                .iter()
                .flat_map(|g| g.sessions.iter().filter_map(|s| s.cwd.clone()))
                .collect();
            v.sort();
            v.dedup();
            v
        };
        let provider_list: Vec<(String, String)> = self
            .groups
            .iter()
            .map(|g| (g.provider.id().to_string(), g.display_name.clone()))
            .collect();
        let project_options: Vec<(Option<String>, String)> = {
            let mut v = vec![(None, "Auto (cwd original)".to_string())];
            for p in &all_projects {
                let label = self
                    .projects
                    .get(p)
                    .map(str::to_string)
                    .unwrap_or_else(|| display_path(p));
                v.push((Some(p.clone()), label));
            }
            v
        };

        egui::CollapsingHeader::new("Importar sesiones")
            .id_salt("import")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Archivo:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.import_path)
                            .hint_text("ruta .zip")
                            .desired_width(220.0),
                    );
                });
                // Filesystem autocomplete: list matching dirs / .zip files.
                let trimmed = self.import_path.trim().to_string();
                if !trimmed.is_empty() && !std::path::Path::new(&trimmed).is_file() {
                    let candidates = path_candidates(&trimmed);
                    if !candidates.is_empty() {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            for c in candidates {
                                if ui.selectable_label(false, completion_label(&c)).clicked() {
                                    self.import_path = c;
                                }
                            }
                        });
                    }
                }
                ui.horizontal(|ui| {
                    ui.label("Proveedor:");
                    egui::ComboBox::from_id_salt("imp-prov")
                        .selected_text(self.import_provider.clone())
                        .show_ui(ui, |ui| {
                            for (id, name) in &provider_list {
                                ui.add_enabled_ui(id == "claude", |ui| {
                                    ui.selectable_value(
                                        &mut self.import_provider,
                                        id.clone(),
                                        name,
                                    )
                                    .on_disabled_hover_text("Import solo soportado para Claude");
                                });
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("Proyecto:");
                    let current = project_options
                        .iter()
                        .find(|(o, _)| *o == self.import_project)
                        .map(|(_, l)| l.clone())
                        .unwrap_or_else(|| "Auto (cwd original)".to_string());
                    egui::ComboBox::from_id_salt("imp-proj")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (opt, label) in &project_options {
                                ui.selectable_value(&mut self.import_project, opt.clone(), label);
                            }
                        });
                });
                if ui.button("Importar").clicked() {
                    self.do_import();
                }
            });

        if let Some(status) = &self.status {
            ui.colored_label(egui::Color32::LIGHT_BLUE, status);
        }
        ui.separator();

        let filter = self.filter.to_lowercase();
        let tag_filter = self.tag_filter.clone();
        let only_active = self.only_active;
        // Snapshot metadata for read during the closure; mutations are deferred.
        let mut to_edit: Option<(String, String)> = None;
        let mut to_preview: Option<(String, String, String)> = None;
        let mut to_export: Option<(usize, usize)> = None;
        let mut to_delete: Option<(usize, usize, bool)> = None;
        let mut to_move: Option<(String, String, bool)> = None;
        let mut to_compact: Option<(usize, usize)> = None;
        let mut to_rename_project: Option<String> = None;
        // Take the new-session path draft out of `self` so the scroll closure
        // can mutate it without clashing with the immutable `self` borrows
        // below; written back after the closure.
        let mut new_session_path = std::mem::take(&mut self.new_session_path);

        // When a content search is active, text matching uses its result set
        // (transcript hits) instead of the title/metadata filter.
        let fts_on = self.fts_query.is_some();
        let fts = &self.fts_matches;
        let passes = |gi: usize, si: usize| -> bool {
            let g = &self.groups[gi];
            let s = &g.sessions[si];
            let text_ok = if fts_on {
                fts.contains(&(g.provider.id().to_string(), s.id.clone()))
            } else {
                matches_filter(s, &self.metadata, g.provider.id(), &filter)
            };
            (!only_active || s.is_active)
                && text_ok
                && tag_passes(
                    &self.metadata,
                    g.provider.id(),
                    &s.id,
                    tag_filter.as_deref(),
                )
        };
        let projects = &self.projects;
        let metadata = &self.metadata;
        let groups = &self.groups;
        let force_open = self.force_open;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.group_mode {
                GroupMode::Provider => {
                    for (gi, group) in groups.iter().enumerate() {
                        let provider_id = group.provider.id();
                        let mut visible: Vec<usize> = (0..group.sessions.len())
                            .filter(|si| passes(gi, *si))
                            .collect();
                        // Favourites pinned to the top (stable: keeps the
                        // last-activity order within each band).
                        visible.sort_by_key(|si| {
                            !metadata
                                .get(provider_id, &group.sessions[*si].id)
                                .is_some_and(|m| m.favorite)
                        });
                        let counts = count_states(visible.iter().map(|si| &group.sessions[*si]));
                        let title = match &group.error {
                            Some(err) => {
                                egui::RichText::new(format!("{} — {err}", group.display_name))
                            }
                            None => egui::RichText::new(format!(
                                "{} ({})",
                                group.display_name,
                                visible.len()
                            )),
                        }
                        .color(provider_color(provider_id))
                        .strong();
                        section(
                            ui,
                            ("provider", gi),
                            title,
                            counts,
                            force_open,
                            !visible.is_empty(),
                            |ui| {
                                if let Some(s) = &group.status {
                                    status_badge(ui, s);
                                }
                                if let Some(q) = &group.quota {
                                    quota_badges(ui, q);
                                }
                                new_session_pick_project(
                                    ui,
                                    group,
                                    projects,
                                    &mut new_session_path,
                                    &mut action,
                                );
                                for si in visible {
                                    let s = &group.sessions[si];
                                    row_ui(
                                        ui,
                                        s,
                                        metadata.get(provider_id, &s.id),
                                        provider_id,
                                        gi,
                                        si,
                                        false,
                                        &mut action,
                                        &mut to_edit,
                                        &mut to_preview,
                                        &mut to_export,
                                        &mut to_delete,
                                        &mut to_move,
                                        &mut to_compact,
                                    );
                                }
                            },
                        );
                    }
                }
                GroupMode::Project => {
                    // Bucket every visible session by working directory, across
                    // providers. BTreeMap keeps the projects sorted.
                    let mut buckets: std::collections::BTreeMap<String, Vec<(usize, usize)>> =
                        std::collections::BTreeMap::new();
                    for (gi, group) in groups.iter().enumerate() {
                        for si in 0..group.sessions.len() {
                            if passes(gi, si) {
                                buckets
                                    .entry(project_key(&group.sessions[si]))
                                    .or_default()
                                    .push((gi, si));
                            }
                        }
                    }
                    for (bi, (project, items)) in buckets.iter().enumerate() {
                        let counts =
                            count_states(items.iter().map(|(gi, si)| &groups[*gi].sessions[*si]));
                        let title = project_header(projects, project, items.len());
                        section(ui, ("project", bi), title, counts, force_open, true, |ui| {
                            project_rename_row(ui, project, &mut to_rename_project);
                            let proj = (project.as_str() != NO_PROJECT).then_some(project.as_str());
                            new_session_pick_provider(ui, groups, proj, &mut action);
                            for (gi, si) in items {
                                let group = &groups[*gi];
                                let s = &group.sessions[*si];
                                row_ui(
                                    ui,
                                    s,
                                    metadata.get(group.provider.id(), &s.id),
                                    group.provider.id(),
                                    *gi,
                                    *si,
                                    true,
                                    &mut action,
                                    &mut to_edit,
                                    &mut to_preview,
                                    &mut to_export,
                                    &mut to_delete,
                                    &mut to_move,
                                    &mut to_compact,
                                );
                            }
                        });
                    }
                }
                GroupMode::Cascade => {
                    for (gi, group) in groups.iter().enumerate() {
                        let provider_id = group.provider.id();
                        let mut visible: Vec<usize> = (0..group.sessions.len())
                            .filter(|si| passes(gi, *si))
                            .collect();
                        // Favourites pinned to the top (stable: keeps the
                        // last-activity order within each band).
                        visible.sort_by_key(|si| {
                            !metadata
                                .get(provider_id, &group.sessions[*si].id)
                                .is_some_and(|m| m.favorite)
                        });
                        let counts = count_states(visible.iter().map(|si| &group.sessions[*si]));
                        let title = match &group.error {
                            Some(err) => {
                                egui::RichText::new(format!("{} — {err}", group.display_name))
                            }
                            None => egui::RichText::new(format!(
                                "{} ({})",
                                group.display_name,
                                visible.len()
                            )),
                        }
                        .color(provider_color(provider_id))
                        .strong();
                        section(
                            ui,
                            ("casc-prov", gi),
                            title,
                            counts,
                            force_open,
                            !visible.is_empty(),
                            |ui| {
                                if let Some(s) = &group.status {
                                    status_badge(ui, s);
                                }
                                if let Some(q) = &group.quota {
                                    quota_badges(ui, q);
                                }
                                // Sub-bucket this provider's sessions by project.
                                let mut subs: std::collections::BTreeMap<String, Vec<usize>> =
                                    std::collections::BTreeMap::new();
                                for si in &visible {
                                    subs.entry(project_key(&group.sessions[*si]))
                                        .or_default()
                                        .push(*si);
                                }
                                for (pi, (project, sis)) in subs.iter().enumerate() {
                                    let counts =
                                        count_states(sis.iter().map(|si| &group.sessions[*si]));
                                    let title = project_header(projects, project, sis.len());
                                    section(
                                        ui,
                                        ("casc-proj", gi, pi),
                                        title,
                                        counts,
                                        force_open,
                                        true,
                                        |ui| {
                                            project_rename_row(ui, project, &mut to_rename_project);
                                            // Provider and project both fixed here: open directly.
                                            if ui.button("+ Nueva sesión").clicked() {
                                                let argv = group.provider.new_session_argv();
                                                if !argv.is_empty() {
                                                    let cwd = (project.as_str() != NO_PROJECT)
                                                        .then(|| PathBuf::from(project));
                                                    action = Some(PanelAction::Open {
                                                        argv,
                                                        cwd,
                                                        key: None,
                                                    });
                                                }
                                            }
                                            for si in sis {
                                                let s = &group.sessions[*si];
                                                row_ui(
                                                    ui,
                                                    s,
                                                    metadata.get(provider_id, &s.id),
                                                    provider_id,
                                                    gi,
                                                    *si,
                                                    false,
                                                    &mut action,
                                                    &mut to_edit,
                                                    &mut to_preview,
                                                    &mut to_export,
                                                    &mut to_delete,
                                                    &mut to_move,
                                                    &mut to_compact,
                                                );
                                            }
                                        },
                                    );
                                }
                            },
                        );
                    }
                }
            });

        // Apply deferred mutations outside the borrow of `self.groups`.
        if let Some((provider, id)) = to_edit {
            self.open_editor(&provider, &id);
        }
        if let Some((provider, id, title)) = to_preview {
            self.load_preview(&provider, &id, title);
        }
        if let Some((gi, si)) = to_export {
            self.do_export(gi, si);
        }
        if let Some((gi, si, force)) = to_delete {
            self.do_delete(gi, si, force);
        }
        if let Some((id, source_cwd, is_live)) = to_move {
            self.move_select = Some(MoveState {
                id,
                source_cwd: source_cwd.clone(),
                is_live,
                dest: source_cwd,
            });
        }
        if let Some((gi, si)) = to_compact {
            let g = &self.groups[gi];
            let session_id = g.sessions[si].id.clone();
            if let Some(argv) = g.provider.compact_argv(&session_id) {
                let cwd = g.sessions[si].cwd.as_ref().map(PathBuf::from);
                // Opens a one-off terminal running `/compact`; not a resume.
                action = Some(PanelAction::Open {
                    argv,
                    cwd,
                    key: None,
                });
            }
        }
        if let Some(path) = to_rename_project {
            let draft = self.projects.get(&path).unwrap_or("").to_string();
            self.project_edit = Some((path, draft));
        }
        self.new_session_path = new_session_path;
        self.force_open = None; // one-shot: applied this frame, then released

        self.editor_window(ui.ctx());
        self.project_window(ui.ctx());
        self.move_window(ui.ctx());
        self.preview_window(ui.ctx());
        if let Some(a) = self.templates_window(ui.ctx()) {
            action = Some(a);
        }

        action
    }

    fn open_editor(&mut self, provider: &str, id: &str) {
        let meta = self.metadata.get(provider, id).cloned().unwrap_or_default();
        self.edit = Some(EditState {
            provider: provider.to_string(),
            id: id.to_string(),
            name: meta.name.unwrap_or_default(),
            tags: meta.tags.join(", "),
            color: meta.color.unwrap_or_default(),
            notes: meta.notes.unwrap_or_default(),
            favorite: meta.favorite,
        });
    }

    fn load_preview(&mut self, provider: &str, id: &str, title: String) {
        let turns = self
            .groups
            .iter()
            .find(|g| g.provider.id() == provider)
            .map(|g| g.provider.preview(id))
            .unwrap_or_else(|| Err("proveedor no encontrado".to_string()));
        self.preview = Some(PreviewState { title, turns });
    }

    fn do_export(&mut self, gi: usize, si: usize) {
        let group = &self.groups[gi];
        let session = group.sessions[si].clone();
        let meta = self.metadata.get(group.provider.id(), &session.id);
        let item = ExportItem {
            session_id: session.id.clone(),
            display_name: meta.and_then(|m| m.name.clone()),
            tags: meta.map(|m| m.tags.clone()).unwrap_or_default(),
        };
        let dest = home_dir().join(format!(
            "aterm-export-{}-{}.zip",
            group.provider.id(),
            now_secs()
        ));
        let provider = &group.provider;
        match export_sessions(&[(session, item)], |id| provider.locate(id), &dest) {
            Ok(0) => self.status = Some("Nada que exportar (sesión no localizada)".into()),
            Ok(n) => self.status = Some(format!("Exportadas {n} → {}", dest.display())),
            Err(e) => self.status = Some(format!("Export falló: {e}")),
        }
    }

    fn do_import(&mut self) {
        let zip = PathBuf::from(self.import_path.trim());
        if self.import_path.trim().is_empty() {
            self.status = Some("Indica una ruta .zip".into());
            return;
        }
        if self.import_provider != "claude" {
            // Only Claude's on-disk layout is wired in `transfer`.
            self.status = Some(format!(
                "Importar a {} aún no está soportado (solo Claude)",
                self.import_provider
            ));
            return;
        }
        let projects = home_dir().join(".claude/projects");
        let result = match &self.import_project {
            // Auto: route each session to its recorded cwd (interop default).
            None => {
                let fallback = projects.join("aterm-imported");
                import_archive_routed(&zip, &projects, &fallback, encode_cwd)
            }
            // Forced: drop every session into the chosen project's directory.
            Some(path) => import_archive(&zip, &projects.join(encode_cwd(path))),
        };
        match result {
            Ok(o) => {
                self.status = Some(format!(
                    "Importadas {} (omitidas {} existentes, {} sin datos)",
                    o.imported.len(),
                    o.skipped_existing.len(),
                    o.skipped_missing.len()
                ));
                self.scanned = false;
            }
            Err(e) => self.status = Some(format!("Import falló: {e}")),
        }
    }

    fn do_delete(&mut self, gi: usize, si: usize, force: bool) {
        let (provider_id, session_id) = {
            let g = &self.groups[gi];
            (g.provider.id().to_string(), g.sessions[si].id.clone())
        };
        let result = self.groups[gi].provider.delete_session(&session_id, force);
        match result {
            Ok(()) => {
                self.status = Some(format!("Eliminada {session_id}"));
                self.metadata
                    .update(&provider_id, &session_id, |m| *m = Default::default());
                self.save_metadata();
                self.groups[gi].sessions.remove(si);
            }
            Err(DeleteError::Active) => {
                self.status =
                    Some("Sesión activa: vuelve a pulsar ✖ para forzar el borrado".into());
            }
            Err(e) => self.status = Some(e.to_user_string()),
        }
    }

    fn editor_window(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Editar sesión")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("edit-grid").num_columns(2).show(ui, |ui| {
                    ui.label("Nombre");
                    ui.text_edit_singleline(&mut edit.name);
                    ui.end_row();
                    ui.label("Tags");
                    ui.text_edit_singleline(&mut edit.tags);
                    ui.end_row();
                    ui.label("Color");
                    ui.text_edit_singleline(&mut edit.color);
                    ui.end_row();
                    ui.label("Notas");
                    ui.add(
                        egui::TextEdit::multiline(&mut edit.notes)
                            .desired_rows(3)
                            .desired_width(260.0)
                            .hint_text("Notas libres sobre la sesión"),
                    );
                    ui.end_row();
                    ui.label("Favorito");
                    ui.checkbox(&mut edit.favorite, "Fijar arriba (★)");
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    if ui.button("Guardar").clicked() {
                        save = true;
                    }
                    if ui.button("Cancelar").clicked() {
                        cancel = true;
                    }
                });
            });

        if save {
            let (provider, id) = (edit.provider.clone(), edit.id.clone());
            let name = edit.name.trim().to_string();
            let tags = parse_tags(&edit.tags);
            let color = edit.color.trim().to_string();
            let notes = edit.notes.trim().to_string();
            let favorite = edit.favorite;
            self.metadata.update(&provider, &id, |m| {
                m.name = (!name.is_empty()).then(|| name.clone());
                m.tags = tags.clone();
                m.color = (!color.is_empty()).then(|| color.clone());
                m.notes = (!notes.is_empty()).then(|| notes.clone());
                m.favorite = favorite;
            });
            self.save_metadata();
            self.edit = None;
        } else if cancel || !open {
            self.edit = None;
        }
    }

    /// The templates manager: list saved recipes (launch/delete) and a form to
    /// save a new one. Returns a launch action when the user fires a template.
    fn templates_window(&mut self, ctx: &egui::Context) -> Option<PanelAction> {
        if !self.templates_open {
            return None;
        }
        // Provider catalogue for the dropdown + argv lookup (fresh, cheap).
        let providers: Vec<(String, String, Vec<String>)> = all_providers()
            .iter()
            .map(|p| {
                (
                    p.id().to_string(),
                    p.display_name().to_string(),
                    p.new_session_argv(),
                )
            })
            .collect();

        let mut open = true;
        let mut to_launch: Option<String> = None;
        let mut to_delete: Option<String> = None;
        let mut save_form = false;
        let mut cancel_form = false;

        egui::Window::new("📋 Plantillas de sesión")
            .open(&mut open)
            .resizable(true)
            .default_size([420.0, 360.0])
            .show(ctx, |ui| {
                if self.templates.templates.is_empty() {
                    ui.weak("Sin plantillas todavía. Crea una abajo.");
                }
                for t in &self.templates.templates {
                    ui.horizontal(|ui| {
                        if ui.button("▶").on_hover_text("Lanzar").clicked() {
                            to_launch = Some(t.id.clone());
                        }
                        if ui.small_button("✕").on_hover_text("Eliminar").clicked() {
                            to_delete = Some(t.id.clone());
                        }
                        ui.label(egui::RichText::new(&t.name).strong());
                        ui.weak(format!("[{}]", t.provider));
                        for tag in &t.tags {
                            ui.colored_label(c_teal(), format!("#{tag}"));
                        }
                    });
                }
                ui.separator();
                if self.template_form.is_none() {
                    if ui.button("＋ Nueva plantilla").clicked() {
                        let mut f = TemplateForm::default();
                        // Default the provider to the first available one.
                        if let Some((id, _, _)) = providers.first() {
                            f.provider = id.clone();
                        }
                        self.template_form = Some(f);
                    }
                } else if let Some(f) = self.template_form.as_mut() {
                    egui::Grid::new("tpl-form").num_columns(2).show(ui, |ui| {
                        ui.label("Nombre");
                        ui.text_edit_singleline(&mut f.name);
                        ui.end_row();
                        ui.label("Proveedor");
                        egui::ComboBox::from_id_salt("tpl-provider")
                            .selected_text(
                                providers
                                    .iter()
                                    .find(|(id, _, _)| id == &f.provider)
                                    .map(|(_, dn, _)| dn.as_str())
                                    .unwrap_or("—"),
                            )
                            .show_ui(ui, |ui| {
                                for (id, dn, _) in &providers {
                                    ui.selectable_value(&mut f.provider, id.clone(), dn);
                                }
                            });
                        ui.end_row();
                        ui.label("Prompt");
                        ui.add(
                            egui::TextEdit::multiline(&mut f.prompt)
                                .desired_rows(2)
                                .desired_width(240.0)
                                .hint_text("Opcional: se pega al lanzar"),
                        );
                        ui.end_row();
                        ui.label("Directorio");
                        ui.text_edit_singleline(&mut f.cwd);
                        ui.end_row();
                        ui.label("Tags");
                        ui.text_edit_singleline(&mut f.tags);
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        let ok = !f.name.trim().is_empty() && !f.provider.trim().is_empty();
                        if ui.add_enabled(ok, egui::Button::new("Guardar")).clicked() {
                            save_form = true;
                        }
                        if ui.button("Cancelar").clicked() {
                            cancel_form = true;
                        }
                    });
                }
            });

        if cancel_form {
            self.template_form = None;
        }
        if save_form {
            if let Some(f) = self.template_form.take() {
                let prompt = f.prompt.trim().to_string();
                let cwd = f.cwd.trim().to_string();
                let t = crate::templates::LaunchTemplate {
                    id: crate::templates::slug(&f.name, now_secs()),
                    name: f.name.trim().to_string(),
                    provider: f.provider.clone(),
                    prompt: (!prompt.is_empty()).then_some(prompt),
                    cwd: (!cwd.is_empty()).then_some(cwd),
                    tags: parse_tags(&f.tags),
                };
                match self.templates.upsert(t) {
                    Ok(()) => self.status = Some("Plantilla guardada".into()),
                    Err(e) => self.status = Some(format!("No se pudo guardar: {e}")),
                }
            }
        }
        if let Some(id) = to_delete {
            if let Err(e) = self.templates.delete(&id) {
                self.status = Some(format!("No se pudo borrar: {e}"));
            }
        }
        let mut action = None;
        if let Some(id) = to_launch {
            if let Some(t) = self.templates.templates.iter().find(|t| t.id == id) {
                let argv = providers
                    .iter()
                    .find(|(pid, _, _)| pid == &t.provider)
                    .map(|(_, _, argv)| argv.clone())
                    .unwrap_or_default();
                if argv.is_empty() {
                    self.status = Some(format!("Proveedor «{}» no disponible", t.provider));
                } else {
                    action = Some(PanelAction::OpenTemplate {
                        argv,
                        cwd: t.cwd.as_ref().map(PathBuf::from),
                        prompt: t.prompt.clone(),
                    });
                }
            }
        }
        if !open {
            self.templates_open = false;
            self.template_form = None;
        }
        action
    }

    fn project_window(&mut self, ctx: &egui::Context) {
        let Some((path, draft)) = self.project_edit.as_mut() else {
            return;
        };
        let path_label = display_path(path);
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let mut set_color: Option<Option<String>> = None;
        egui::Window::new("Proyecto")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.weak(path_label);
                ui.label("Nombre:");
                ui.text_edit_singleline(draft);
                ui.separator();
                ui.label("Color:");
                ui.horizontal_wrapped(|ui| {
                    let p = crate::theme::pal();
                    let swatches = [
                        ("Lavanda", p.lavender),
                        ("Verde", p.green),
                        ("Amarillo", p.yellow),
                        ("Melocotón", p.peach),
                        ("Rojo", p.red),
                        ("Malva", p.mauve),
                        ("Turquesa", p.teal),
                        ("Zafiro", p.sapphire),
                    ];
                    for (name, c) in swatches {
                        if ui
                            .add(
                                egui::Button::new("  ")
                                    .fill(c)
                                    .min_size(egui::vec2(22.0, 18.0)),
                            )
                            .on_hover_text(name)
                            .clicked()
                        {
                            set_color = Some(Some(hex_of(c)));
                        }
                    }
                    if ui.button("Sin color").clicked() {
                        set_color = Some(None);
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Guardar").clicked() {
                        save = true;
                    }
                    if ui.button("Cancelar").clicked() {
                        cancel = true;
                    }
                });
            });

        // Colour swatches apply immediately (the window stays open).
        if let Some(hex) = set_color {
            if let Some(p) = self.project_edit.as_ref().map(|(p, _)| p.clone()) {
                self.projects.set_color(&p, hex);
                let _ = self.projects.save(&self.projects_path);
            }
        }
        if save {
            if let Some((path, draft)) = self.project_edit.take() {
                self.projects.set(&path, draft);
                if let Err(e) = self.projects.save(&self.projects_path) {
                    self.status = Some(format!("No se pudieron guardar los nombres: {e}"));
                }
            }
        } else if cancel || !open {
            self.project_edit = None;
        }
    }

    fn move_window(&mut self, ctx: &egui::Context) {
        let Some(mv) = self.move_select.as_mut() else {
            return;
        };
        // Known destination projects (Claude sessions' distinct cwds).
        let mut projects: Vec<String> = self
            .groups
            .iter()
            .find(|g| g.provider.id() == "claude")
            .map(|g| {
                let mut v: Vec<String> = g.sessions.iter().filter_map(|s| s.cwd.clone()).collect();
                v.sort();
                v.dedup();
                v
            })
            .unwrap_or_default();
        projects.retain(|p| p != &mv.source_cwd);

        let mut open = true;
        let mut go = false;
        let mut cancel = false;
        egui::Window::new("Mover sesión a proyecto")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.weak(format!("Desde: {}", display_path(&mv.source_cwd)));
                ui.label("Destino:");
                ui.add(
                    egui::TextEdit::singleline(&mut mv.dest)
                        .hint_text("/ruta/al/proyecto")
                        .desired_width(260.0),
                );
                // Autocomplete + known projects as quick picks.
                let trimmed = mv.dest.trim().to_string();
                if !trimmed.is_empty() && !std::path::Path::new(&trimmed).is_dir() {
                    for c in path_candidates(&trimmed) {
                        if c != trimmed
                            && ui.selectable_label(false, completion_label(&c)).clicked()
                        {
                            mv.dest = c;
                        }
                    }
                }
                for p in &projects {
                    if ui.selectable_label(false, display_path(p)).clicked() {
                        mv.dest = p.clone();
                    }
                }
                ui.horizontal(|ui| {
                    let ok = !mv.dest.trim().is_empty() && mv.dest.trim() != mv.source_cwd;
                    if ui.add_enabled(ok, egui::Button::new("Mover")).clicked() {
                        go = true;
                    }
                    if ui.button("Cancelar").clicked() {
                        cancel = true;
                    }
                });
            });

        if go {
            if let Some(mv) = self.move_select.take() {
                self.do_move(&mv);
            }
        } else if cancel || !open {
            self.move_select = None;
        }
    }

    fn do_move(&mut self, mv: &MoveState) {
        let projects = home_dir().join(".claude/projects");
        let src = encode_cwd(&mv.source_cwd);
        let dst = encode_cwd(mv.dest.trim());
        match move_session(&projects, &mv.id, &src, &dst, |_| mv.is_live) {
            Ok(()) => {
                self.status = Some(format!("Movida a {}", display_path(mv.dest.trim())));
                self.scanned = false; // re-scan to reflect the new project
            }
            Err(e) if e == "ACTIVE" => {
                self.status = Some("No se puede mover una sesión activa".into());
            }
            Err(e) => self.status = Some(format!("Mover falló: {e}")),
        }
    }

    fn preview_window(&mut self, ctx: &egui::Context) {
        let Some(preview) = self.preview.as_ref() else {
            return;
        };
        let mut open = true;
        egui::Window::new(format!("Preview — {}", preview.title))
            .open(&mut open)
            .default_size([520.0, 420.0])
            .vscroll(true)
            .show(ctx, |ui| match &preview.turns {
                Ok(turns) if turns.is_empty() => {
                    ui.label("(sin contenido)");
                }
                Ok(turns) => {
                    for turn in turns {
                        let color = if turn.role == "user" {
                            egui::Color32::LIGHT_GREEN
                        } else {
                            egui::Color32::LIGHT_BLUE
                        };
                        ui.colored_label(color, format!("▍{}", turn.role));
                        ui.label(&turn.text);
                        ui.separator();
                    }
                }
                Err(e) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, e);
                }
            });
        if !open {
            self.preview = None;
        }
    }
}

/// One session row: colour dot, name/title, resume + actions, metadata line.
#[allow(clippy::too_many_arguments)]
fn row_ui(
    ui: &mut egui::Ui,
    s: &AgentSession,
    meta: Option<&agent_sessions::SessionMetadata>,
    provider_id: &str,
    gi: usize,
    si: usize,
    show_provider: bool,
    action: &mut Option<PanelAction>,
    to_edit: &mut Option<(String, String)>,
    to_preview: &mut Option<(String, String, String)>,
    to_export: &mut Option<(usize, usize)>,
    to_delete: &mut Option<(usize, usize, bool)>,
    to_move: &mut Option<(String, String, bool)>,
    to_compact: &mut Option<(usize, usize)>,
) {
    let name = meta
        .and_then(|m| m.name.clone())
        .or_else(|| s.title.clone())
        .unwrap_or_else(|| "(sin título)".to_string());

    egui::Frame::none()
        .fill(c_card())
        .rounding(8.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            // Title line: live dot, resume, optional [provider], name.
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!s.resume_argv.is_empty(), egui::Button::new("▶"))
                    .on_hover_text("Reanudar")
                    .clicked()
                {
                    *action = Some(PanelAction::Open {
                        argv: s.resume_argv.clone(),
                        cwd: s.cwd.as_ref().map(PathBuf::from),
                        key: Some(format!("{provider_id}:{}", s.id)),
                    });
                }
                if let Some(dot) = meta
                    .and_then(|m| m.color.as_ref())
                    .and_then(|c| parse_hex(c))
                {
                    ui.colored_label(dot, "●");
                }
                if s.is_active {
                    let (color, tip) = live_state(s.live_status.as_deref());
                    ui.colored_label(color, "●").on_hover_text(tip);
                }
                if show_provider {
                    ui.colored_label(provider_color(provider_id), format!("[{provider_id}]"));
                }
                if meta.is_some_and(|m| m.favorite) {
                    ui.colored_label(egui::Color32::from_rgb(0xf9, 0xe2, 0xaf), "★")
                        .on_hover_text("Favorito");
                }
                ui.label(egui::RichText::new(&name).strong());
            });

            // Metadata line: model · branch · context% · msgs · relative time.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let mut sep = false;
                let dot = |ui: &mut egui::Ui, sep: &mut bool| {
                    if *sep {
                        ui.weak("·");
                    }
                    *sep = true;
                };
                if let Some(model) = &s.model {
                    dot(ui, &mut sep);
                    ui.weak(short_model(model));
                }
                if let Some(branch) = &s.branch {
                    dot(ui, &mut sep);
                    ui.weak(format!("⎇ {branch}"));
                }
                if let (Some(tok), Some(win)) = (s.context_tokens, s.context_window) {
                    if win > 0 {
                        let pct = (tok * 100 / win).min(999);
                        dot(ui, &mut sep);
                        ui.colored_label(usage_color(pct as f64), format!("{pct}%"))
                            .on_hover_text("Contexto usado");
                    }
                }
                if let Some(n) = s.message_count {
                    dot(ui, &mut sep);
                    ui.weak(format!("{n} msg"));
                }
                dot(ui, &mut sep);
                ui.weak(relative_time(s.last_activity));
            });

            if let Some(m) = meta {
                if !m.tags.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for tag in &m.tags {
                            ui.colored_label(c_teal(), format!("#{tag}"));
                        }
                    });
                }
                if let Some(notes) = &m.notes {
                    ui.weak("🗒 notas").on_hover_text(notes);
                }
            }

            ui.horizontal(|ui| {
                if ui
                    .small_button("✏")
                    .on_hover_text("Renombrar / tags / color")
                    .clicked()
                {
                    *to_edit = Some((provider_id.to_string(), s.id.clone()));
                }
                if ui.small_button("◉").on_hover_text("Preview").clicked() {
                    *to_preview = Some((provider_id.to_string(), s.id.clone(), name.clone()));
                }
                if ui
                    .small_button("⇩")
                    .on_hover_text("Exportar .zip")
                    .clicked()
                {
                    *to_export = Some((gi, si));
                }
                // Compact / move — Claude only.
                if provider_id == "claude" {
                    if ui
                        .small_button("»«")
                        .on_hover_text("Compactar contexto (/compact)")
                        .clicked()
                    {
                        *to_compact = Some((gi, si));
                    }
                    if let Some(cwd) = &s.cwd {
                        if ui
                            .small_button("⇄")
                            .on_hover_text("Mover a otro proyecto")
                            .clicked()
                        {
                            *to_move = Some((s.id.clone(), cwd.clone(), s.is_active));
                        }
                    }
                }
                if ui.small_button("✖").on_hover_text("Eliminar").clicked() {
                    *to_delete = Some((gi, si, s.is_active));
                }
            });
        });
    ui.add_space(6.0);
}

/// Scan the enabled providers (list sessions + quota). Runs off the UI thread.
fn scan_all_providers() -> Vec<ProviderGroup> {
    let cfg = crate::settings::get();
    all_providers()
        .into_iter()
        .filter(|p| cfg.scans(p.id()))
        .map(|p| {
            let display_name = p.display_name().to_string();
            let quota = if cfg.fetch_status { p.quota() } else { None };
            let status = if cfg.fetch_status {
                crate::service_status::fetch(p.id())
            } else {
                None
            };
            match p.list_sessions() {
                Ok(mut sessions) => {
                    for s in &mut sessions {
                        s.resume_argv = p.resume_argv(&s.id);
                    }
                    sessions.sort_by(|a, b| b.last_activity.total_cmp(&a.last_activity));
                    ProviderGroup {
                        provider: p,
                        display_name,
                        sessions,
                        quota,
                        status,
                        error: None,
                    }
                }
                Err(e) => ProviderGroup {
                    provider: p,
                    display_name,
                    sessions: Vec::new(),
                    quota,
                    status,
                    error: Some(e),
                },
            }
        })
        .collect()
}

/// Service-health badge from the provider's statuspage indicator.
fn status_badge(ui: &mut egui::Ui, status: &agent_sessions::types::ServiceStatus) {
    let (color, label) = match status.indicator.as_str() {
        "none" => (c_green(), "operativo"),
        "minor" => (
            egui::Color32::from_rgb(0xf9, 0xe2, 0xaf),
            "incidencia menor",
        ),
        "major" => (
            egui::Color32::from_rgb(0xfa, 0xb3, 0x87),
            "incidencia grave",
        ),
        "critical" => (egui::Color32::from_rgb(0xf3, 0x8b, 0xa8), "caída"),
        _ => (egui::Color32::GRAY, "estado desconocido"),
    };
    let text = if status.description.is_empty() {
        label.to_string()
    } else {
        status.description.clone()
    };
    ui.horizontal(|ui| {
        ui.colored_label(color, "●");
        ui.colored_label(color, text);
    });
}

fn quota_badges(ui: &mut egui::Ui, q: &ProviderQuota) {
    ui.horizontal_wrapped(|ui| {
        for w in &q.windows {
            // Same threshold logic as the session context %.
            ui.colored_label(
                usage_color(w.used_percent),
                format!("{}: {:.0}%", w.label, w.used_percent),
            );
        }
    });
}

fn matches_filter(
    s: &AgentSession,
    metadata: &MetadataStore,
    provider_id: &str,
    filter: &str,
) -> bool {
    if filter.is_empty() {
        return true;
    }
    let meta = metadata.get(provider_id, &s.id);
    let haystacks = [
        s.title.clone(),
        s.model.clone(),
        s.branch.clone(),
        meta.and_then(|m| m.name.clone()),
    ];
    if haystacks
        .iter()
        .flatten()
        .any(|h| h.to_lowercase().contains(filter))
    {
        return true;
    }
    meta.is_some_and(|m| m.tags.iter().any(|t| t.to_lowercase().contains(filter)))
}

/// Whether a session passes the active tag "folder" (None = no folder filter).
fn tag_passes(
    metadata: &MetadataStore,
    provider_id: &str,
    id: &str,
    tag_filter: Option<&str>,
) -> bool {
    match tag_filter {
        None => true,
        Some(tag) => metadata
            .get(provider_id, id)
            .is_some_and(|m| m.tags.iter().any(|t| t == tag)),
    }
}

// Accents pulled from the active theme (so a theme switch recolours the panel).
fn c_lavender() -> egui::Color32 {
    crate::theme::pal().lavender
}
fn c_teal() -> egui::Color32 {
    crate::theme::pal().teal
}
fn c_green() -> egui::Color32 {
    crate::theme::pal().green
}
fn c_card() -> egui::Color32 {
    crate::theme::pal().card
}

/// Colour for any usage percentage (context, session quota, weekly quota):
/// <40% green, 40–60% orange, ≥60% red.
fn usage_color(pct: f64) -> egui::Color32 {
    let p = crate::theme::pal();
    if pct < 40.0 {
        p.green
    } else if pct < 60.0 {
        p.peach
    } else {
        p.red
    }
}

/// Colour + tooltip for a live session's status: working vs waiting.
fn live_state(status: Option<&str>) -> (egui::Color32, &'static str) {
    let p = crate::theme::pal();
    match status {
        Some("busy") => (p.peach, "Trabajando"),
        Some("idle") => (p.green, "En espera"),
        _ => (p.blue, "Activa"),
    }
}

/// Brand-ish accent per provider, for the section headers.
fn provider_color(id: &str) -> egui::Color32 {
    let p = crate::theme::pal();
    match id {
        "claude" => p.peach,
        "codex" => p.green,
        "opencode" => p.sapphire,
        "gemini" => p.mauve,
        "qwen" => p.teal,
        "goose" => p.yellow,
        "factory" => p.red,
        _ => p.lavender,
    }
}

/// The project bucket key for a session: its `cwd`, or a placeholder.
fn project_key(s: &AgentSession) -> String {
    s.cwd.clone().unwrap_or_else(|| NO_PROJECT.to_string())
}

/// Sentinel project key for sessions whose provider didn't record a cwd.
const NO_PROJECT: &str = "(sin proyecto)";

/// Header for a project bucket: alias if set, else the path, in teal.
fn project_header(projects: &ProjectNames, path: &str, count: usize) -> egui::RichText {
    let label = projects
        .get(path)
        .map(str::to_string)
        .unwrap_or_else(|| display_path(path));
    let color = projects.color(path).unwrap_or_else(c_teal);
    egui::RichText::new(format!("{label} ({count})"))
        .color(color)
        .strong()
}

/// Live-session tally split by reported state, for the section headers.
#[derive(Default, Clone, Copy)]
struct StateCounts {
    working: usize,
    waiting: usize,
    other: usize,
}

impl StateCounts {
    /// Coloured `●N` badges (only for non-zero buckets).
    fn badges(self, ui: &mut egui::Ui) {
        if self.working > 0 {
            ui.colored_label(crate::theme::pal().peach, format!("●{}", self.working))
                .on_hover_text("Trabajando");
        }
        if self.waiting > 0 {
            ui.colored_label(crate::theme::pal().green, format!("●{}", self.waiting))
                .on_hover_text("En espera");
        }
        if self.other > 0 {
            ui.colored_label(crate::theme::pal().blue, format!("●{}", self.other))
                .on_hover_text("Activa");
        }
    }
}

fn count_states<'a>(sessions: impl Iterator<Item = &'a AgentSession>) -> StateCounts {
    let mut c = StateCounts::default();
    for s in sessions {
        if !s.is_active {
            continue;
        }
        match s.live_status.as_deref() {
            Some("busy") => c.working += 1,
            Some("idle") => c.waiting += 1,
            _ => c.other += 1,
        }
    }
    c
}

/// A collapsible section with a custom header (title + per-state badges).
fn section(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash,
    title: egui::RichText,
    counts: StateCounts,
    force_open: Option<bool>,
    default_open: bool,
    body: impl FnOnce(&mut egui::Ui),
) {
    let id = ui.make_persistent_id(salt);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        default_open,
    );
    if let Some(open) = force_open {
        state.set_open(open);
    }
    state
        .show_header(ui, |ui| {
            ui.label(title);
            counts.badges(ui);
        })
        .body(|ui| body(ui));
}

/// "New session" for a *project* section: the project is fixed, so the menu
/// picks which provider to launch. Opens the new session in `project`'s cwd.
fn new_session_pick_provider(
    ui: &mut egui::Ui,
    groups: &[ProviderGroup],
    project: Option<&str>,
    action: &mut Option<PanelAction>,
) {
    ui.menu_button("+ Nueva sesión ▾", |ui| {
        ui.label("Proveedor:");
        for group in groups {
            let argv = group.provider.new_session_argv();
            if argv.is_empty() {
                continue;
            }
            if ui
                .button(
                    egui::RichText::new(&group.display_name)
                        .color(provider_color(group.provider.id())),
                )
                .clicked()
            {
                *action = Some(PanelAction::Open {
                    argv,
                    cwd: project.map(PathBuf::from),
                    key: None,
                });
                ui.close_menu();
            }
        }
    });
}

/// "New session" for a *provider* section: the provider is fixed, so the menu
/// picks which project (working directory) to launch in.
fn new_session_pick_project(
    ui: &mut egui::Ui,
    group: &ProviderGroup,
    names: &ProjectNames,
    draft: &mut String,
    action: &mut Option<PanelAction>,
) {
    let argv = group.provider.new_session_argv();
    // Only this provider's own projects (distinct cwds across its sessions).
    let mut projects: Vec<&str> = group
        .sessions
        .iter()
        .filter_map(|s| s.cwd.as_deref())
        .collect();
    projects.sort_unstable();
    projects.dedup();
    ui.add_enabled_ui(!argv.is_empty(), |ui| {
        ui.menu_button("+ Nueva sesión ▾", |ui| {
            ui.label("Proyecto:");
            if ui.button("(directorio actual)").clicked() {
                *action = Some(PanelAction::Open {
                    argv: argv.clone(),
                    cwd: None,
                    key: None,
                });
                ui.close_menu();
            }
            for path in projects {
                let label = names
                    .get(path)
                    .map(str::to_string)
                    .unwrap_or_else(|| display_path(path));
                if ui.button(label).clicked() {
                    *action = Some(PanelAction::Open {
                        argv: argv.clone(),
                        cwd: Some(PathBuf::from(path)),
                        key: None,
                    });
                    ui.close_menu();
                }
            }
            ui.separator();
            // A brand-new project: type any directory to open the session in.
            ui.label("Otra ruta:");
            ui.add(
                egui::TextEdit::singleline(draft)
                    .hint_text("/ruta/al/proyecto")
                    .desired_width(220.0),
            );
            // Filesystem autocomplete (clicking a candidate keeps the menu open).
            let trimmed = draft.trim().to_string();
            if !trimmed.is_empty() && !std::path::Path::new(&trimmed).is_dir() {
                for candidate in path_candidates(&trimmed) {
                    if candidate == trimmed {
                        continue;
                    }
                    if ui
                        .selectable_label(false, completion_label(&candidate))
                        .clicked()
                    {
                        *draft = candidate;
                    }
                }
            }
            let ok = !draft.trim().is_empty();
            if ui.add_enabled(ok, egui::Button::new("Abrir")).clicked() {
                *action = Some(PanelAction::Open {
                    argv: argv.clone(),
                    cwd: Some(PathBuf::from(draft.trim())),
                    key: None,
                });
                draft.clear();
                ui.close_menu();
            }
        });
    });
}

/// First row inside a project bucket: a rename button plus the real path, so
/// the user always sees what an alias maps to. Hidden for the no-cwd bucket.
fn project_rename_row(ui: &mut egui::Ui, path: &str, out: &mut Option<String>) {
    if path == NO_PROJECT {
        return;
    }
    ui.horizontal(|ui| {
        if ui
            .small_button("✎")
            .on_hover_text("Renombrar proyecto")
            .clicked()
        {
            *out = Some(path.to_string());
        }
        ui.weak(display_path(path));
    });
}

/// Shorten a working-directory path for a project header: `$HOME` → `~`.
fn display_path(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if let Ok(rel) = std::path::Path::new(path).strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    path.to_string()
}

/// Trim the long provider prefix from a model id for compact display.
fn short_model(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_string()
}

fn relative_time(unix_secs: f64) -> String {
    let now = now_secs() as f64;
    let delta = (now - unix_secs).max(0.0);
    if delta < 60.0 {
        "ahora".to_string()
    } else if delta < 3600.0 {
        format!("hace {}m", (delta / 60.0) as u64)
    } else if delta < 86400.0 {
        format!("hace {}h", (delta / 3600.0) as u64)
    } else {
        format!("hace {}d", (delta / 86400.0) as u64)
    }
}

fn parse_hex(hex: &str) -> Option<egui::Color32> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}

fn hex_of(c: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Filesystem completions for `input`: directories and `.zip` files in the
/// directory being typed whose name starts with the current leaf. Directories
/// come back with a trailing `/` so a click drills in.
fn path_candidates(input: &str) -> Vec<String> {
    let path = std::path::Path::new(input);
    let (dir, prefix) = if input.ends_with('/') {
        (PathBuf::from(input), String::new())
    } else {
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let prefix = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        (dir, prefix)
    };
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !name.starts_with(&prefix) {
                continue;
            }
            let is_dir = entry.path().is_dir();
            if is_dir || name.to_lowercase().ends_with(".zip") {
                let mut full = dir.join(&name).to_string_lossy().to_string();
                if is_dir {
                    full.push('/');
                }
                out.push(full);
            }
        }
    }
    out.sort();
    out.truncate(12);
    out
}

/// Display just the leaf of a completion candidate (with `/` kept for dirs).
fn completion_label(candidate: &str) -> String {
    let trimmed = candidate.trim_end_matches('/');
    let leaf = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if candidate.ends_with('/') {
        format!("{leaf}/")
    } else {
        leaf.to_string()
    }
}

fn config_dir() -> PathBuf {
    home_dir().join(".config/aterm")
}

fn metadata_path() -> PathBuf {
    config_dir().join("session-metadata.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_label_keeps_dir_slash_and_strips_path() {
        assert_eq!(completion_label("/home/u/proj/"), "proj/");
        assert_eq!(completion_label("/home/u/backup.zip"), "backup.zip");
    }

    #[test]
    fn path_candidates_lists_dirs_and_zips_by_prefix() {
        let dir = std::env::temp_dir().join("aterm_ac_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("keep.zip"), b"x").unwrap();
        std::fs::write(dir.join("skip.txt"), b"x").unwrap();
        std::fs::write(dir.join("other.zip"), b"x").unwrap();

        // Prefix "k" inside the dir should match only keep.zip.
        let input = format!("{}/k", dir.display());
        let got = path_candidates(&input);
        assert_eq!(got.len(), 1);
        assert!(got[0].ends_with("keep.zip"));

        // Trailing slash lists the whole dir: sub/ (dir) + the two .zip files.
        let all = path_candidates(&format!("{}/", dir.display()));
        assert!(all.iter().any(|c| c.ends_with("sub/")));
        assert!(all.iter().filter(|c| c.ends_with(".zip")).count() == 2);
        assert!(!all.iter().any(|c| c.ends_with(".txt")));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
