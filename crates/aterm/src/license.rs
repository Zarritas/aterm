//! Pro licence gate — the native port of the extension's `license.ts`.
//!
//! Paridad con la extensión (mismo backend y mismas URLs de compra):
//! - Prueba de 14 días que arranca en el primer uso.
//! - Validación **online** con la Lemon Squeezy License API (`activate` /
//!   `validate`), vía `curl` (igual que `service_status.rs`, sin depender de un
//!   crate HTTP/TLS pesado).
//! - Cache tolerante a estar offline: el último estado conocido se conserva y
//!   se concede un *grace* de 14 días si la red falla.
//!
//! Estado persistido en `~/.config/aterm/license.json`. No hay verificación
//! Ed25519: la extensión tampoco la tiene (es online-first con cache), así que
//! replicarla aquí divergiría del producto sin un emisor que firme licencias.

use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Días de prueba antes de exigir licencia.
const TRIAL_DAYS: u64 = 14;
const DAY_MS: u64 = 86_400_000;
/// Grace offline: si no se puede revalidar, el último "licensed" vale 14 días.
const GRACE_MS: u64 = TRIAL_DAYS * DAY_MS;
/// Revalidar contra Lemon Squeezy como mucho cada 12 h.
const REVALIDATE_MS: u64 = 12 * 3600 * 1000;

const LS_API: &str = "https://api.lemonsqueezy.com/v1/licenses";

/// Checkout de Lemon Squeezy (paridad con `license.ts`).
pub const BUY_URL_ANNUAL: &str =
    "https://aterm.lemonsqueezy.com/checkout/buy/258755f8-8c93-41ab-b0b0-e8d07fdfcc25";
pub const BUY_URL_MONTHLY: &str =
    "https://aterm.lemonsqueezy.com/checkout/buy/87d06b1a-b038-434d-9ad3-b58553f4a4ea";

/// Estado de la licencia, equivalente al `ProStatus` de la extensión.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Licencia válida (último `validate`/`activate` OK, dentro del grace).
    Licensed,
    /// En prueba: quedan `days` días.
    Trial { days_left: u64 },
    /// Prueba terminada y sin licencia válida.
    Expired,
}

/// Lo que se persiste en disco.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct LicenseFile {
    /// Epoch ms del primer uso (arranque de la prueba).
    trial_start: Option<u64>,
    /// Clave de licencia introducida por el usuario.
    license_key: Option<String>,
    /// Id de instancia devuelto por Lemon Squeezy al activar.
    instance_id: Option<String>,
    /// Último resultado de validación (true = licenciado).
    licensed_cache: bool,
    /// Epoch ms hasta el que el cache "licensed" se considera válido (grace).
    licensed_until: Option<u64>,
    /// Epoch ms de la última revalidación online.
    last_check: Option<u64>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/aterm/license.json")
}

fn load_from_disk() -> LicenseFile {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_to_disk(f: &LicenseFile) -> std::io::Result<()> {
    let p = path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(f).map_err(std::io::Error::other)?;
    std::fs::write(p, json)
}

static STATE: LazyLock<RwLock<LicenseFile>> = LazyLock::new(|| RwLock::new(load_from_disk()));

impl LicenseFile {
    /// ¿El cache "licensed" sigue dentro del grace offline?
    fn cached_licensed(&self) -> bool {
        self.licensed_cache && self.licensed_until.is_some_and(|until| now_ms() < until)
    }

    fn trial_days_left(&self) -> u64 {
        match self.trial_start {
            None => TRIAL_DAYS,
            Some(start) => {
                let elapsed_days = now_ms().saturating_sub(start) / DAY_MS;
                TRIAL_DAYS.saturating_sub(elapsed_days)
            }
        }
    }

    fn status(&self) -> Status {
        if self.cached_licensed() {
            Status::Licensed
        } else if self.trial_days_left() > 0 {
            Status::Trial {
                days_left: self.trial_days_left(),
            }
        } else {
            Status::Expired
        }
    }
}

/// Arranca el reloj de la prueba la primera vez. Llamar una vez al inicio.
pub fn start_trial_if_needed() {
    let mut s = STATE.write().unwrap();
    if s.trial_start.is_none() {
        s.trial_start = Some(now_ms());
        let _ = save_to_disk(&s);
    }
}

/// Estado actual de la licencia.
pub fn status() -> Status {
    STATE.read().unwrap().status()
}

/// ¿Está desbloqueado el tier Pro (licencia válida o prueba activa)?
pub fn is_pro() -> bool {
    !matches!(status(), Status::Expired)
}

/// Texto corto para el chrome (indicador junto al botón).
pub fn badge() -> String {
    match status() {
        Status::Licensed => "Pro".to_string(),
        Status::Trial { days_left } => format!("Prueba · {days_left}d"),
        Status::Expired => "Community".to_string(),
    }
}

/// POST `x-www-form-urlencoded` a Lemon Squeezy vía `curl`, devolviendo el JSON.
/// Best-effort: `None` si no hay curl/red o la respuesta no es JSON.
fn ls_post(action: &str, params: &[(&str, &str)]) -> Option<serde_json::Value> {
    let url = format!("{LS_API}/{action}");
    let mut cmd = std::process::Command::new("curl");
    cmd.args([
        "-sL",
        "-m",
        "12",
        "-X",
        "POST",
        "-H",
        "Accept: application/json",
        "-H",
        "Content-Type: application/x-www-form-urlencoded",
    ]);
    for (k, v) in params {
        cmd.arg("--data-urlencode");
        cmd.arg(format!("{k}={v}"));
    }
    cmd.arg(&url);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Activa una clave de licencia contra Lemon Squeezy. Devuelve `Ok` si la
/// licencia es válida; persiste el cache. Error con mensaje en caso contrario.
pub fn activate(key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("introduce una clave de licencia".to_string());
    }
    let instance_name = format!("aterm-{}", &now_ms().to_string());
    let resp = ls_post(
        "activate",
        &[("license_key", key), ("instance_name", &instance_name)],
    )
    .ok_or_else(|| "no se pudo contactar con Lemon Squeezy (¿sin red?)".to_string())?;

    let activated = resp.get("activated").and_then(|v| v.as_bool()) == Some(true);
    if !activated {
        let err = resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("licencia no válida");
        return Err(err.to_string());
    }
    let instance_id = resp
        .get("instance")
        .and_then(|i| i.get("id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut s = STATE.write().unwrap();
    s.license_key = Some(key.to_string());
    s.instance_id = instance_id;
    s.licensed_cache = true;
    s.licensed_until = Some(now_ms() + GRACE_MS);
    s.last_check = Some(now_ms());
    save_to_disk(&s).map_err(|e| e.to_string())?;
    Ok(())
}

/// Revalida la licencia cacheada contra Lemon Squeezy si toca (>12 h) y hay
/// clave. Tolerante a estar offline: si falla la red, no toca el cache (el
/// grace sigue corriendo). Pensado para llamarse de vez en cuando.
pub fn revalidate() {
    let (key, instance, due) = {
        let s = STATE.read().unwrap();
        let due = s
            .last_check
            .map(|t| now_ms().saturating_sub(t) > REVALIDATE_MS)
            .unwrap_or(true);
        (s.license_key.clone(), s.instance_id.clone(), due)
    };
    let Some(key) = key else { return };
    if !due {
        return;
    }
    let mut params = vec![("license_key", key.as_str())];
    if let Some(inst) = instance.as_deref() {
        params.push(("instance_id", inst));
    }
    let Some(resp) = ls_post("validate", &params) else {
        return; // offline → conservar cache (grace)
    };
    let valid = resp.get("valid").and_then(|v| v.as_bool()) == Some(true);
    let mut s = STATE.write().unwrap();
    s.last_check = Some(now_ms());
    s.licensed_cache = valid;
    s.licensed_until = if valid {
        Some(now_ms() + GRACE_MS)
    } else {
        None
    };
    let _ = save_to_disk(&s);
}

/// Abre la página de compra en el navegador (best-effort).
pub fn open_buy() {
    // Plan anual por defecto; el diálogo de upsell ofrece ambos.
    open_url(BUY_URL_ANNUAL);
}

/// Abre una URL con el handler del sistema (`xdg-open`/`open`).
pub fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_days_count_down_and_floor_at_zero() {
        let mut f = LicenseFile::default();
        assert_eq!(f.trial_days_left(), TRIAL_DAYS); // sin arrancar
        f.trial_start = Some(now_ms());
        assert_eq!(f.trial_days_left(), TRIAL_DAYS);
        f.trial_start = Some(now_ms().saturating_sub(3 * DAY_MS));
        assert_eq!(f.trial_days_left(), TRIAL_DAYS - 3);
        f.trial_start = Some(now_ms().saturating_sub(100 * DAY_MS));
        assert_eq!(f.trial_days_left(), 0);
    }

    #[test]
    fn status_prefers_licence_then_trial_then_expired() {
        let mut f = LicenseFile::default();
        // Fresh: trial.
        assert!(matches!(f.status(), Status::Trial { .. }));
        // Expired trial, no licence.
        f.trial_start = Some(now_ms().saturating_sub(100 * DAY_MS));
        assert_eq!(f.status(), Status::Expired);
        // Valid cached licence wins even with expired trial.
        f.licensed_cache = true;
        f.licensed_until = Some(now_ms() + DAY_MS);
        assert_eq!(f.status(), Status::Licensed);
        // Cache past its grace no longer counts.
        f.licensed_until = Some(now_ms().saturating_sub(1));
        assert_eq!(f.status(), Status::Expired);
    }
}
