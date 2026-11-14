//! OWNER: W4. Barcode scanning of items to record arrival (receive) or use (issue). See API_REFERENCE 0.2-J.
use crate::state::AppState;
use axum::Router;
pub fn router() -> Router<AppState> { Router::new() }
