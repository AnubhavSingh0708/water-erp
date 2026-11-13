//! OWNER: Subagent B. Inter-godown transfers via the TRANSIT location (API_REFERENCE 0.2-F).
use crate::state::AppState;
use axum::Router;
pub fn router() -> Router<AppState> { Router::new() }
