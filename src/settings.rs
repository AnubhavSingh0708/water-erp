//! Runtime settings persisted in `system_settings`; editable live (Admin API) without restart.
use crate::error::{AppError, AppResult};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use std::{collections::HashMap, str::FromStr, sync::{Arc, RwLock}};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Settings {
    pub plant_name: String,
    pub plant_code: String,
    pub qr_base_url: String,
    pub session_ttl_hours: i64,
    pub session_inactivity_purge_days: i64,
    pub max_failed_logins: i64,
    pub lockout_minutes: i64,
    pub burn_rate_lookback_days: i64,
    pub safety_stock_days: i64,
    pub worker_interval_minutes: i64,
    pub jar_overdue_days: i64,
    pub expiry_lead_days: i64,
    pub default_crate_set_size: i64,
    pub strict_qc_lock: bool,
    pub qr_labels_enabled: bool,
    pub utc_offset_minutes: i64,
    pub large_adjust_pct: i64,
    pub scrap_warn_pct: i64,
    pub void_window_hours: i64,
    pub damage_warn_pct: i64,
    pub jar_destroy_warn_pct: i64,
    pub ph_min: Decimal,
    pub ph_max: Decimal,
    pub tds_min: Decimal,
    pub tds_max: Decimal,
}

#[derive(Copy, Clone)]
enum Kind { Str, Int(i64, i64), Bool, Num }

const SCHEMA: &[(&str, Kind)] = &[
    ("plant.name", Kind::Str), ("plant.code", Kind::Str), ("plant.qr_base_url", Kind::Str),
    ("auth.session_ttl_hours", Kind::Int(1, 24 * 90)), ("auth.session_inactivity_purge_days", Kind::Int(1, 365)),
    ("auth.max_failed_logins", Kind::Int(1, 100)), ("auth.lockout_minutes", Kind::Int(1, 1440)),
    ("inventory.burn_rate_lookback_days", Kind::Int(1, 365)), ("inventory.safety_stock_days", Kind::Int(0, 90)),
    ("inventory.worker_interval_minutes", Kind::Int(1, 1440)),
    ("jars.overdue_days", Kind::Int(1, 365)), ("alerts.expiry_lead_days", Kind::Int(0, 365)),
    ("loading.default_crate_set_size", Kind::Int(1, 1000)), ("loading.strict_qc_lock", Kind::Bool),
    ("features.qr_labels", Kind::Bool), ("plant.utc_offset_minutes", Kind::Int(-720, 840)),
    ("entry.large_adjust_pct", Kind::Int(1, 1000)), ("production.scrap_warn_pct", Kind::Int(0, 100)),
    ("loading.void_window_hours", Kind::Int(1, 720)), ("settle.damage_warn_pct", Kind::Int(0, 100)), ("jars.destroy_warn_pct", Kind::Int(0, 100)),
    ("lab.ph_min", Kind::Num), ("lab.ph_max", Kind::Num), ("lab.tds_min", Kind::Num), ("lab.tds_max", Kind::Num),
];

impl Settings {
    fn from_map(m: &HashMap<String, Value>) -> Settings {
        let s = |k: &str, d: &str| m.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let i = |k: &str, d: i64| m.get(k).and_then(Value::as_i64).unwrap_or(d);
        let n = |k: &str, d: i64| m.get(k).and_then(|v| Decimal::from_str(&v.to_string().trim_matches('"').to_string()).ok()).unwrap_or(Decimal::from(d));
        Settings {
            plant_name: s("plant.name", "Aqua Pure Plant"), plant_code: s("plant.code", "PLT1"), qr_base_url: s("plant.qr_base_url", ""),
            session_ttl_hours: i("auth.session_ttl_hours", 12), session_inactivity_purge_days: i("auth.session_inactivity_purge_days", 7),
            max_failed_logins: i("auth.max_failed_logins", 5), lockout_minutes: i("auth.lockout_minutes", 15),
            burn_rate_lookback_days: i("inventory.burn_rate_lookback_days", 14), safety_stock_days: i("inventory.safety_stock_days", 3),
            worker_interval_minutes: i("inventory.worker_interval_minutes", 15),
            jar_overdue_days: i("jars.overdue_days", 30), expiry_lead_days: i("alerts.expiry_lead_days", 30),
            default_crate_set_size: i("loading.default_crate_set_size", 10),
            strict_qc_lock: m.get("loading.strict_qc_lock").and_then(Value::as_bool).unwrap_or(true),
            qr_labels_enabled: m.get("features.qr_labels").and_then(Value::as_bool).unwrap_or(false),
            utc_offset_minutes: i("plant.utc_offset_minutes", 330),
            large_adjust_pct: i("entry.large_adjust_pct", 20), scrap_warn_pct: i("production.scrap_warn_pct", 5),
            void_window_hours: i("loading.void_window_hours", 12), damage_warn_pct: i("settle.damage_warn_pct", 10), jar_destroy_warn_pct: i("jars.destroy_warn_pct", 3),
            ph_min: n("lab.ph_min", 6), ph_max: n("lab.ph_max", 9), tds_min: n("lab.tds_min", 50), tds_max: n("lab.tds_max", 500),
        }
    }
}

/// Cheap-to-clone handle holding the current settings snapshot.
#[derive(Clone)]
pub struct SettingsStore(Arc<RwLock<Arc<Settings>>>);

impl SettingsStore {
    pub async fn load(db: &PgPool) -> AppResult<SettingsStore> {
        let s = Self::fetch(db).await?;
        Ok(SettingsStore(Arc::new(RwLock::new(Arc::new(s)))))
    }
    async fn fetch(db: &PgPool) -> AppResult<Settings> {
        let rows: Vec<(String, Value)> = sqlx::query_as("SELECT key, value FROM system_settings").fetch_all(db).await?;
        Ok(Settings::from_map(&rows.into_iter().collect()))
    }
    /// Current snapshot (never blocks on I/O).
    pub fn get(&self) -> Arc<Settings> { self.0.read().unwrap().clone() }
    pub async fn reload(&self, db: &PgPool) -> AppResult<()> {
        let s = Self::fetch(db).await?;
        *self.0.write().unwrap() = Arc::new(s);
        Ok(())
    }
}

/// All settings rows (for the Admin settings page).
pub async fn list(db: &PgPool) -> AppResult<Vec<(String, Value, String)>> {
    Ok(sqlx::query_as("SELECT key, value, description FROM system_settings ORDER BY key").fetch_all(db).await?)
}

/// Validate + persist one setting, then refresh the in-memory snapshot. Admin API calls this.
pub async fn update(db: &PgPool, store: &SettingsStore, key: &str, value: Value, user_id: Option<Uuid>) -> AppResult<()> {
    let kind = SCHEMA.iter().find(|(k, _)| *k == key).map(|(_, k)| *k)
        .ok_or_else(|| AppError::field(key, "unknown setting"))?;
    let ok = match kind {
        Kind::Str => value.is_string(),
        Kind::Bool => value.is_boolean(),
        Kind::Int(lo, hi) => value.as_i64().map(|v| v >= lo && v <= hi).unwrap_or(false),
        Kind::Num => value.is_number(),
    };
    if !ok { return Err(AppError::field(key, "invalid value for this setting")); }
    let r = sqlx::query("UPDATE system_settings SET value = $2, updated_at = now(), updated_by = $3 WHERE key = $1")
        .bind(key).bind(&value).bind(user_id).execute(db).await?;
    if r.rows_affected() == 0 { return Err(AppError::field(key, "unknown setting")); }
    store.reload(db).await
}
