//! `/api/dashboard` – KPI tiles and short lists (OWNER B, page `dashboard`). Read-only.
use crate::{alerts, auth::AuthUser, error::AppResult, routes::ref_data::bh::{self, batch_j, Arg}, state::AppState};
use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};

pub fn router() -> Router<AppState> { Router::new().route("/", get(dashboard)) }

async fn dashboard(State(s): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let can = |pages: &[&str]| pages.iter().any(|p| user.can_access(p));
    let may = |ev: &str| alerts::user_may_receive(user.is_admin, &user.allowed_pages, ev);
    let visible_events: Vec<String> = alerts::EVENT_TYPES.iter().filter(|(c, _)| may(c)).map(|(c, _)| c.to_string()).collect();
    let burn_days = s.settings.get().burn_rate_lookback_days.max(1);
    let mut kpis = bh::jval(&s.db,
        "WITH rm AS (SELECT m.id, m.min_stock, COALESCE((SELECT SUM(qty) FROM stock_balances sb WHERE sb.item_type='RAW' AND sb.item_id=m.id AND sb.zone='RM_STORE'),0) AS q
                     FROM raw_materials m WHERE m.is_active AND m.category <> 'EMPTY_BOTTLE')
         SELECT jsonb_build_object(
           'low_stock_count', (SELECT count(*) FROM rm WHERE q < min_stock),
           'open_reorders', (SELECT count(*) FROM reorder_recommendations WHERE status='OPEN'),
           'batches_in_quarantine', (SELECT count(*) FROM production_batches WHERE qc_status='QUARANTINE'),
           'fg_ready_crates', COALESCE((SELECT SUM(qty) FROM stock_balances WHERE item_type='PRODUCT' AND zone='FG_READY'),0)::text,
           'trips_on_road', (SELECT count(*) FROM loading_trips WHERE status='DISPATCHED'),
           'jars_overdue', (SELECT count(*) FROM alerts WHERE event_type='JAR_OVERDUE' AND resolved_at IS NULL),
           'maintenance_due', (SELECT count(*) FROM alerts WHERE event_type='MAINTENANCE_DUE' AND resolved_at IS NULL),
           'water_log_overdue', COALESCE((SELECT max(logged_at) FROM water_treatment_logs) < now() - interval '60 minutes', true))", &[]).await?;
    let pending_qc = bh::jval(&s.db,
        concat!("SELECT COALESCE(jsonb_agg(j ORDER BY ca), '[]'::jsonb) FROM (SELECT ", batch_j!(), " || jsonb_build_object('age_minutes', floor(extract(epoch FROM (now() - b.created_at)) / 60)::bigint) AS j,
                 b.created_at AS ca FROM production_batches b JOIN products p ON p.id = b.product_id WHERE b.qc_status='QUARANTINE' ORDER BY b.created_at LIMIT 20) t"), &[]).await?;
    let low_stock = bh::jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY ratio, nm), '[]'::jsonb) FROM (
           SELECT jsonb_build_object('raw_material_id', m.id, 'name', m.name, 'unit', m.stock_unit, 'stock', x.q::text, 'min_stock', m.min_stock::text,
                    'days_cover', CASE WHEN COALESCE(b.used,0) > 0 THEN round(x.q / (b.used / $1::numeric), 1)::text END,
                    'status', CASE WHEN x.q <= 0 THEN 'OUT' ELSE 'LOW' END) AS j,
                  CASE WHEN m.min_stock > 0 THEN x.q / m.min_stock ELSE 0 END AS ratio, m.name AS nm
           FROM raw_materials m
           CROSS JOIN LATERAL (SELECT COALESCE(SUM(qty),0) AS q FROM stock_balances sb WHERE sb.item_type='RAW' AND sb.item_id=m.id AND sb.zone='RM_STORE') x
           LEFT JOIN (SELECT item_id, -SUM(qty_delta) AS used FROM stock_ledger WHERE item_type='RAW' AND txn_type IN ('BLOW_ISSUE','BATCH_CONSUME','BATCH_WASTE')
                      AND at >= now() - make_interval(days => $1::int) GROUP BY item_id) b ON b.item_id = m.id
           WHERE m.is_active AND m.category <> 'EMPTY_BOTTLE' AND x.q < m.min_stock ORDER BY ratio, m.name LIMIT 20) t", &[Arg::I(burn_days)]).await?;
    let active_trips = bh::jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY ca DESC), '[]'::jsonb) FROM (
           SELECT jsonb_build_object('id', t.id, 'trip_no', t.trip_no, 'status', t.status, 'vehicle_id', t.vehicle_id, 'vehicle_reg', v.reg_no, 'route_name', r.name,
                  'driver_name', t.driver_name, 'capacity_crates', t.capacity_crates, 'gate_pass_no', t.gate_pass_no, 'gate_pass_issued_at', t.gate_pass_issued_at,
                  'created_at', t.created_at, 'loaded_crates', COALESCE((SELECT SUM(ti.crates) FROM trip_items ti WHERE ti.trip_id = t.id), 0)) AS j, t.created_at AS ca
           FROM loading_trips t JOIN vehicles v ON v.id = t.vehicle_id LEFT JOIN routes r ON r.id = t.route_id
           WHERE t.status IN ('LOADING','DISPATCHED') ORDER BY t.created_at DESC LIMIT 20) q", &[]).await?;
    let recent_alerts = bh::jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY ca DESC), '[]'::jsonb) FROM (
           SELECT jsonb_build_object('id', a.id, 'event_type', a.event_type, 'severity', a.severity, 'title', a.title, 'body', a.body, 'url', a.url,
                  'created_at', a.created_at, 'resolved', a.resolved_at IS NOT NULL) AS j, a.created_at AS ca
           FROM alerts a WHERE a.event_type = ANY($1) ORDER BY a.created_at DESC LIMIT 10) q", &[Arg::TA(visible_events)]).await?;
    // numbers of pages the caller cannot open are hidden (null), as are the matching lists
    let hide = |k: &mut Value, key: &str, ok: bool| { if !ok { k[key] = Value::Null; } };
    hide(&mut kpis, "low_stock_count", can(&["raw-materials"]));
    hide(&mut kpis, "open_reorders", can(&["raw-materials"]));
    hide(&mut kpis, "batches_in_quarantine", can(&["qc", "production"]));
    hide(&mut kpis, "fg_ready_crates", can(&["warehouse"]));
    hide(&mut kpis, "trips_on_road", can(&["loading"]));
    hide(&mut kpis, "jars_overdue", may("JAR_OVERDUE"));
    hide(&mut kpis, "maintenance_due", may("MAINTENANCE_DUE"));
    hide(&mut kpis, "water_log_overdue", can(&["production"]));
    let empty = || json!([]);
    let pending_qc = if can(&["qc", "production"]) { pending_qc } else { empty() };
    let low_stock = if can(&["raw-materials"]) { low_stock } else { empty() };
    let active_trips = if can(&["loading"]) { active_trips } else { empty() };
    Ok(Json(json!({ "kpis": kpis, "pending_qc": pending_qc, "low_stock": low_stock, "active_trips": active_trips, "recent_alerts": recent_alerts })))
}
