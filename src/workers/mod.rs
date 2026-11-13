//! Background Tokio workers. Each submodule exposes `spawn(state)` which starts its own loop.
use crate::state::AppState;
pub mod maintenance;   // OWNER C: vehicle/equipment expiry + overdue jar alerts
pub mod push;          // OWNER C: alert outbox -> Web Push dispatcher
pub mod reorder;       // OWNER B: low-stock + burn-rate engine
pub mod session_purge; // OWNER A: delete untouched sessions

pub fn spawn_all(state: AppState) {
    crate::events::spawn_listener(state.clone());
    session_purge::spawn(state.clone());
    reorder::spawn(state.clone());
    maintenance::spawn(state.clone());
    push::spawn(state);
}
