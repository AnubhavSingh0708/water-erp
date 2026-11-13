//! OWNER: Subagent A. `/api/admin/*` – every route is Admin-only (guard applied in `routes/mod.rs`) and every mutation is audited.
//! Submodules live next to this file (`admin_*.rs`).
use crate::state::AppState;
use axum::Router;

#[path = "admin_common.rs"]
pub mod common;
#[path = "admin_master.rs"]
pub mod master;
#[path = "admin_products.rs"]
mod products;
#[path = "admin_settings.rs"]
mod settings_audit;
#[path = "admin_users.rs"]
mod users;

pub use super::auth_routes::bootstrap_admin;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(settings_audit::router())
        .merge(users::router())
        .merge(master::router())
        .merge(products::router())
}
