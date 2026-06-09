//! The agent-session panel: scan providers, list sessions with rich metadata,
//! filter, preview conversations, rename/tag/colour, export/import and delete.
//!
//! All the heavy lifting lives in the `agent-sessions` crate (read-only
//! discovery + `MetadataStore` + `transfer`); this module is UI wiring.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use agent_sessions::{
    all_providers, encode_cwd, export_sessions, import_archive_routed, parse_tags,
    types::{AgentSession, DeleteError, PreviewTurn, ProviderQuota},
    AgentProvider, ExportItem, MetadataStore,
};
use eframe::egui;

/// What the panel asks the host app to do (open a PTY tab).
pub enum PanelAction {
    Open {
        argv: Vec<String>,
        cwd: Option<PathBuf>,
    },
}

/// One provider's scan result plus the live trait object (for preview/delete/
/// quota) and its account quota snapshot.
struct ProviderGroup {
    provider: Box<dyn AgentProvider>,
    display_name: String,
    sessions: Vec<AgentSession>,
    quota: Option<ProviderQuota>,
    error: Option<String>,
}

/// In-flight rename/tags/colour edit for one `(provider, id)`.
struct EditState {
    provider: String,
    id: String,
    name: String,
    tags: String,
    color: String,
}

/// Loaded conversation preview for the inspector window.
struct PreviewState {
    title: String,
    turns: Result<Vec<PreviewTurn>, String>,
}

pub struct SessionPanel {
    groups: Vec<ProviderGroup>,
    scanned: bool,
    filter: String,
    metadata: MetadataStore,
    metadata_path: PathBuf,
    edit: Option<EditState>,
    preview: Option<PreviewState>,
    import_path: String,
    status: Option<String>,
}

impl Default for SessionPanel {
    fn default() -> Self {
        let metadata_path = metadata_path();
        let metadata = MetadataStore::load(&metadata_path);
        Self {
            groups: Vec::new(),
            scanned: false,
            filter: String::new(),
            metadata,
            metadata_path,
            edit: None,
            preview: None,
            import_path: String::new(),
            status: None,
        }
    }
}

impl SessionPanel {
    /// Re-scan every provider. Synchronous; fine for the session counts a
    /// developer machine holds.
    fn scan(&mut self) {
        self.groups = all_providers()
            .into_iter()
            .map(|p| {
                let display_name = p.display_name().to_string();
                let quota = p.quota();
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
                            error: None,
                        }
                    }
                    Err(e) => ProviderGroup {
                        provider: p,
                        display_name,
                        sessions: Vec::new(),
                        quota,
                        error: Some(e),
                    },
                }
            })
            .collect();
        self.scanned = true;
    }

    fn save_metadata(&mut self) {
        if let Err(e) = self.metadata.save(&self.metadata_path) {
            self.status = Some(format!("No se pudo guardar metadata: {e}"));
        }
    }

    /// Render the panel into `ui`; returns an action when the user resumes a
    /// session or starts a new one.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<PanelAction> {
        if !self.scanned {
            self.scan();
        }
        let mut action = None;

        ui.horizontal(|ui| {
            ui.heading("Agent sessions");
            if ui.button("⟳").on_hover_text("Re-escanear").clicked() {
                self.scanned = false;
            }
        });

        ui.horizontal(|ui| {
            ui.label("🔍");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("filtrar…")
                    .desired_width(f32::INFINITY),
            );
        });

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.import_path)
                    .hint_text("ruta .zip a importar")
                    .desired_width(160.0),
            );
            if ui.button("Importar").clicked() {
                self.do_import();
            }
        });

        if let Some(status) = &self.status {
            ui.colored_label(egui::Color32::LIGHT_BLUE, status);
        }
        ui.separator();

        let filter = self.filter.to_lowercase();
        // Snapshot metadata for read during the closure; mutations are deferred.
        let mut to_edit: Option<(String, String)> = None;
        let mut to_preview: Option<(String, String, String)> = None;
        let mut to_export: Option<(usize, usize)> = None;
        let mut to_delete: Option<(usize, usize, bool)> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            for (gi, group) in self.groups.iter().enumerate() {
                let provider_id = group.provider.id().to_string();
                let visible: Vec<usize> = group
                    .sessions
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| matches_filter(s, &self.metadata, &provider_id, &filter))
                    .map(|(i, _)| i)
                    .collect();

                let header = match &group.error {
                    Some(err) => format!("{} — {err}", group.display_name),
                    None => format!("{} ({})", group.display_name, visible.len()),
                };
                egui::CollapsingHeader::new(header)
                    .id_salt(("group", gi))
                    .default_open(!visible.is_empty())
                    .show(ui, |ui| {
                        if let Some(q) = &group.quota {
                            quota_badges(ui, q);
                        }
                        if ui.button("＋ Nueva sesión").clicked() {
                            let argv = group.provider.new_session_argv();
                            if !argv.is_empty() {
                                action = Some(PanelAction::Open { argv, cwd: None });
                            }
                        }
                        for si in &visible {
                            let s = &group.sessions[*si];
                            let meta = self.metadata.get(&provider_id, &s.id);
                            row_ui(
                                ui,
                                s,
                                meta,
                                &provider_id,
                                gi,
                                *si,
                                &mut action,
                                &mut to_edit,
                                &mut to_preview,
                                &mut to_export,
                                &mut to_delete,
                            );
                        }
                    });
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

        self.editor_window(ui.ctx());
        self.preview_window(ui.ctx());

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
        // Routed import targets Claude's project layout (the interop format).
        let projects = home_dir().join(".claude/projects");
        let fallback = projects.join("aterm-imported");
        match import_archive_routed(&zip, &projects, &fallback, encode_cwd) {
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
                self.metadata.update(&provider_id, &session_id, |m| {
                    *m = Default::default()
                });
                self.save_metadata();
                self.groups[gi].sessions.remove(si);
            }
            Err(DeleteError::Active) => {
                self.status =
                    Some("Sesión activa: vuelve a pulsar 🗑 para forzar el borrado".into());
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
            self.metadata.update(&provider, &id, |m| {
                m.name = (!name.is_empty()).then(|| name.clone());
                m.tags = tags.clone();
                m.color = (!color.is_empty()).then(|| color.clone());
            });
            self.save_metadata();
            self.edit = None;
        } else if cancel || !open {
            self.edit = None;
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
    _provider_id: &str,
    gi: usize,
    si: usize,
    action: &mut Option<PanelAction>,
    to_edit: &mut Option<(String, String)>,
    to_preview: &mut Option<(String, String, String)>,
    to_export: &mut Option<(usize, usize)>,
    to_delete: &mut Option<(usize, usize, bool)>,
) {
    let name = meta
        .and_then(|m| m.name.clone())
        .or_else(|| s.title.clone())
        .unwrap_or_else(|| "(sin título)".to_string());

    ui.horizontal(|ui| {
        if let Some(dot) = meta.and_then(|m| m.color.as_ref()).and_then(parse_hex) {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, dot);
        }
        let live = if s.is_active { "● " } else { "" };
        if ui
            .add_enabled(!s.resume_argv.is_empty(), egui::Button::new("▶"))
            .on_hover_text("Resume")
            .clicked()
        {
            *action = Some(PanelAction::Open {
                argv: s.resume_argv.clone(),
                cwd: s.cwd.as_ref().map(PathBuf::from),
            });
        }
        ui.label(format!("{live}{name}"));
    });

    // Metadata line: model · branch · context% · msgs · relative time.
    let mut bits: Vec<String> = Vec::new();
    if let Some(model) = &s.model {
        bits.push(short_model(model));
    }
    if let Some(branch) = &s.branch {
        bits.push(format!("⎇ {branch}"));
    }
    if let (Some(tok), Some(win)) = (s.context_tokens, s.context_window) {
        if win > 0 {
            bits.push(format!("{}%", (tok * 100 / win).min(999)));
        }
    }
    if let Some(n) = s.message_count {
        bits.push(format!("{n} msg"));
    }
    bits.push(relative_time(s.last_activity));
    ui.horizontal_wrapped(|ui| {
        ui.add_space(16.0);
        ui.weak(bits.join("  ·  "));
    });

    if let Some(m) = meta {
        if !m.tags.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(16.0);
                for tag in &m.tags {
                    ui.weak(format!("#{tag}"));
                }
            });
        }
    }

    ui.horizontal(|ui| {
        ui.add_space(16.0);
        if ui.small_button("✏").on_hover_text("Renombrar / tags / color").clicked() {
            *to_edit = Some((_provider_id.to_string(), s.id.clone()));
        }
        if ui.small_button("👁").on_hover_text("Preview").clicked() {
            *to_preview = Some((_provider_id.to_string(), s.id.clone(), name.clone()));
        }
        if ui.small_button("⤓").on_hover_text("Exportar .zip").clicked() {
            *to_export = Some((gi, si));
        }
        if ui.small_button("🗑").on_hover_text("Eliminar").clicked() {
            // Force when the session is active (second-click semantics live in
            // the status message); a plain delete refuses active sessions.
            *to_delete = Some((gi, si, s.is_active));
        }
    });
    ui.separator();
}

fn quota_badges(ui: &mut egui::Ui, q: &ProviderQuota) {
    ui.horizontal_wrapped(|ui| {
        for w in &q.windows {
            let color = if w.used_percent >= 90.0 {
                egui::Color32::LIGHT_RED
            } else if w.used_percent >= 70.0 {
                egui::Color32::YELLOW
            } else {
                egui::Color32::GRAY
            };
            ui.colored_label(color, format!("{}: {:.0}%", w.label, w.used_percent));
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
    meta.map_or(false, |m| {
        m.tags.iter().any(|t| t.to_lowercase().contains(filter))
    })
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

fn parse_hex(hex: &String) -> Option<egui::Color32> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
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

fn metadata_path() -> PathBuf {
    home_dir().join(".config/aterm/session-metadata.json")
}
