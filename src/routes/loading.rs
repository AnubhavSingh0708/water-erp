//! `/api/loading/*` (page `loading`): availability, trips, bulk items, gate pass, settlement, dispatch notes, gate-pass void.
//! Submodules live in `src/routes/loading/`. Lock order everywhere: trip -> batch rows (canonical order) -> stock rows -> customers' jar rows.
use crate::state::AppState;
use axum::{
    routing::{delete, get, post},
    Router,
};

pub(crate) mod common;
mod dispatch;
mod gate;
mod settle;
mod trips;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/availability", get(trips::availability))
        .route("/summary", get(trips::summary))
        .route("/trips", get(trips::list_trips).post(trips::create_trip))
        .route("/trips/{id}", get(trips::get_trip))
        .route("/trips/{id}/items", post(trips::add_item))
        .route("/trips/{id}/items/{item_id}", delete(trips::delete_item))
        .route("/trips/{id}/cancel", post(trips::cancel_trip))
        .route("/trips/{id}/gate-pass", get(gate::print).post(gate::issue))
        .route("/trips/{id}/settle", post(settle::settle))
        .route("/trips/{id}/void", post(gate::void))
        .route("/dispatches", get(dispatch::list).post(dispatch::create))
        .route("/dispatches/{id}", get(dispatch::get))
        .route("/dispatches/{id}/void", post(dispatch::void))
}
