//! OWNER: Subagent A. Login / first-run setup / current user.
use crate::state::AppState;
use axum::Router;
pub fn public_router() -> Router<AppState> { Router::new() }  // mounted at /api/auth  (POST /login)
pub fn setup_router() -> Router<AppState> { Router::new() }   // mounted at /api/setup (GET /status, POST /admin)
pub fn me_router() -> Router<AppState> { Router::new() }      // mounted at /api/me    (GET /, POST /logout, POST /password)
