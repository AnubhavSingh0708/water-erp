//! OWNER: Subagent A. `/api/admin/*` – every route is Admin-only (guard applied in `routes/mod.rs`) and every mutation is audited.
//! Submodules live next to this file (`admin_*.rs`).
use crate::state::AppState;
use axum::Router;

#[path = "admin_common.rs"]
pub mod common;

pub use super::auth_routes::bootstrap_admin;

pub fn router() -> Router<AppState> { Router::new() }
