//! Open-core seam on the app side.
//!
//! [`module`] returns the active [`ProModule`]: the real implementation from the
//! private `aterm-pro` crate when built with `--features pro`, or a Community
//! stub otherwise. The stub keeps the chrome and wiring identical across
//! editions — Pro actions simply explain they need the Pro edition.
//!
//! `impl ProHost for AtermApp` (the surface Pro features use) lives in `app.rs`.

use aterm_pro_api::{ProHost, ProModule};

/// The active Pro module for this build.
#[cfg(feature = "pro")]
pub fn module() -> Box<dyn ProModule> {
    aterm_pro::module()
}

/// A do-nothing module used as a transient placeholder while the real one is
/// moved out of `AtermApp` for a call (avoids `Option` juggling).
pub fn noop_module() -> Box<dyn ProModule> {
    Box::new(NoopPro)
}

struct NoopPro;

impl ProModule for NoopPro {
    fn open_parallel(&mut self, _host: &mut dyn ProHost) {}
    fn run_compare(&mut self, _host: &mut dyn ProHost) {}
    fn open_cleanup(&mut self, _host: &mut dyn ProHost) {}
    fn ui(&mut self, _ctx: &egui::Context, _host: &mut dyn ProHost) {}
    fn edition(&self) -> &'static str {
        "Community"
    }
}

#[cfg(not(feature = "pro"))]
pub fn module() -> Box<dyn ProModule> {
    Box::new(CommunityPro)
}

/// Community stub: every gated feature politely declines. No state, no dialogs.
#[cfg(not(feature = "pro"))]
struct CommunityPro;

#[cfg(not(feature = "pro"))]
impl CommunityPro {
    fn decline(host: &mut dyn ProHost, feature: &str) {
        host.notify(format!(
            "«{feature}» es una función Pro. Esta es la edición Community."
        ));
    }
}

#[cfg(not(feature = "pro"))]
impl ProModule for CommunityPro {
    fn open_parallel(&mut self, host: &mut dyn ProHost) {
        Self::decline(host, "Comparativa paralela");
    }
    fn run_compare(&mut self, host: &mut dyn ProHost) {
        Self::decline(host, "Comparar worktrees");
    }
    fn open_cleanup(&mut self, host: &mut dyn ProHost) {
        Self::decline(host, "Limpiar worktrees");
    }
    fn ui(&mut self, _ctx: &egui::Context, _host: &mut dyn ProHost) {}
    fn edition(&self) -> &'static str {
        "Community"
    }
}
