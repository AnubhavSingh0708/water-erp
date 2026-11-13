//! Reorder / low-stock engine + stock-expiry check (OWNER B).
//!
//! Per active, non-EMPTY_BOTTLE raw material (API_REFERENCE §5):
//!   burn          = Σ(−delta of RAW rows with txn_type ∈ {BLOW_ISSUE, BATCH_CONSUME} in the last N days) / N
//!   reorder_point = burn × (supplier.lead_time_days + safety_stock_days),   current = RM_STORE quantity
//!   BELOW_MIN: current < min_stock · BURN_RATE: burn > 0 and current ≤ reorder_point · BOTH
//!   target = 2 × max(min_stock, reorder_point), suggested_qty_purchase = ceil((target − current − on_order) / stock_per_purchase_unit) ≥ 1
//! `on_order` (refinement, documented in docs/notes-B.md) = outstanding quantity of OPEN/PARTIAL purchase orders, in stock units;
//! it prevents a second recommendation for goods that are already on order (when it covers the need no OPEN recommendation exists,
//! but the LOW_STOCK alert still fires).
//! One OPEN recommendation per material (upsert). A DISMISSED recommendation suppresses a new one for 24 h.
use crate::{alerts, error::AppResult, settings::Settings, state::AppState, util};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::time::Duration;
use uuid::Uuid;

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        // first run shortly after boot, then every `inventory.worker_interval_minutes`
        let mut wait = Duration::from_secs(20);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = state.shutdown.wait() => break,
            }
            if let Err(e) = run_once(&state).await { tracing::warn!(error = %e, "reorder worker run failed"); }
            wait = Duration::from_secs(state.settings.get().worker_interval_minutes.max(1) as u64 * 60);
        }
    });
}

/// Evaluate every material and the expiry check. Used by the worker loop AND `POST /reorder-recommendations/run`.
pub async fn run_once(state: &AppState) -> AppResult<Value> {
    let st = state.settings.get();
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM raw_materials WHERE is_active AND category <> 'EMPTY_BOTTLE' ORDER BY id")
        .fetch_all(&state.db).await?;
    let mut failed = 0;
    for id in &ids {
        let mut tx = state.db.begin().await?;
        match evaluate(&mut tx, &st, *id).await {
            Ok(()) => { tx.commit().await?; }
            Err(e) => { failed += 1; tracing::warn!(error = %e, material = %id, "reorder evaluation failed"); }
        }
    }
    let mut tx = state.db.begin().await?;
    let expiry = check_expiry(&mut tx, &st).await?;
    tx.commit().await?;
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM reorder_recommendations WHERE status='OPEN'").fetch_one(&state.db).await?;
    Ok(json!({ "materials_checked": ids.len(), "failed": failed, "open_recommendations": open, "expiry_alerts_raised": expiry }))
}

/// Re-evaluate specific materials inside the caller's transaction (after GRN / void / supplier return).
pub async fn evaluate_materials(conn: &mut PgConnection, st: &Settings, ids: &[Uuid]) -> AppResult<()> {
    let mut seen: Vec<Uuid> = ids.to_vec();
    seen.sort();
    seen.dedup();
    for id in seen { evaluate(conn, st, id).await?; }
    Ok(())
}

/// Evaluate one material: upsert/resolve its recommendation and raise/resolve LOW_STOCK + REORDER_RECOMMENDED.
pub async fn evaluate(conn: &mut PgConnection, st: &Settings, rm_id: Uuid) -> AppResult<()> {
    let row = sqlx::query(
        "SELECT m.name, m.min_stock, m.stock_per_purchase_unit AS spp, m.default_supplier_id, COALESCE(s.lead_time_days, 7) AS lead,
                COALESCE((SELECT SUM(qty) FROM stock_balances WHERE item_type='RAW' AND item_id=m.id AND zone='RM_STORE'), 0) AS cur,
                COALESCE((SELECT -SUM(qty_delta) FROM stock_ledger WHERE item_type='RAW' AND item_id=m.id
                          AND txn_type IN ('BLOW_ISSUE','BATCH_CONSUME') AND at >= now() - make_interval(days => $2::int)), 0) AS used,
                COALESCE((SELECT SUM(GREATEST(l.qty_ordered - l.qty_received, 0)) FROM po_lines l JOIN purchase_orders o ON o.id = l.po_id
                          WHERE l.raw_material_id = m.id AND o.status IN ('OPEN','PARTIAL')), 0) * m.stock_per_purchase_unit AS on_order,
                EXISTS(SELECT 1 FROM reorder_recommendations r WHERE r.raw_material_id = m.id AND r.status = 'DISMISSED'
                       AND r.updated_at > now() - interval '24 hours') AS dismissed_recently
         FROM raw_materials m LEFT JOIN suppliers s ON s.id = m.default_supplier_id
         WHERE m.id = $1 AND m.is_active AND m.category <> 'EMPTY_BOTTLE'")
        .bind(rm_id).bind(st.burn_rate_lookback_days as i32).fetch_optional(&mut *conn).await?;
    let Some(r) = row else { return Ok(()); };
    let name: String = r.get("name");
    let min: Decimal = r.get("min_stock");
    let spp: Decimal = r.get("spp");
    let cur: Decimal = r.get("cur");
    let used: Decimal = r.get("used");
    let on_order: Decimal = r.get("on_order");
    let lead: i32 = r.get("lead");
    let supplier: Option<Uuid> = r.get("default_supplier_id");
    let dismissed: bool = r.get("dismissed_recently");

    let days = Decimal::from(st.burn_rate_lookback_days.max(1));
    let burn = (used / days).round_dp(4);
    let rp = (burn * Decimal::from(lead as i64 + st.safety_stock_days)).round_dp(4);
    let below = cur < min;
    let by_burn = burn > Decimal::ZERO && cur <= rp;
    let reason = match (below, by_burn) { (true, true) => Some("BOTH"), (true, false) => Some("BELOW_MIN"), (false, true) => Some("BURN_RATE"), _ => None };
    let key = rm_id.to_string();
    let url = "/raw-materials.html";

    // LOW_STOCK alert follows BELOW_MIN
    if below {
        let _ = alerts::raise(&mut *conn, alerts::NewAlert {
            event_type: "LOW_STOCK", dedupe_key: &key, severity: if cur <= Decimal::ZERO { "CRITICAL" } else { "WARNING" },
            title: &format!("Low stock: {name}"), body: &format!("{} in store, minimum {}", cur.normalize(), min.normalize()), url,
            payload: json!({ "raw_material_id": rm_id, "current": cur.to_string(), "min_stock": min.to_string() }),
        }).await?;
    } else {
        alerts::resolve(&mut *conn, "LOW_STOCK", &key).await?;
    }

    let mut rec_open = false;
    if let Some(reason) = reason {
        let target = Decimal::from(2) * min.max(rp);
        let need = target - cur - on_order;
        let suggested = if need > Decimal::ZERO { util::ceil(need / spp).max(Decimal::ONE) } else { Decimal::ZERO };
        let has_open: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM reorder_recommendations WHERE raw_material_id=$1 AND status='OPEN')")
            .bind(rm_id).fetch_one(&mut *conn).await?;
        if suggested > Decimal::ZERO && (has_open || !dismissed) {
            let cover = if burn > Decimal::ZERO { Some((cur / burn).round_dp(2)) } else { None };
            sqlx::query(
                "INSERT INTO reorder_recommendations (raw_material_id, status, reason, current_stock, min_stock, avg_daily_burn, days_cover, lead_time_days,
                        safety_days, reorder_point, suggested_qty_purchase, suggested_supplier_id)
                 VALUES ($1,'OPEN',$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
                 ON CONFLICT (raw_material_id) WHERE status = 'OPEN' DO UPDATE SET reason=EXCLUDED.reason, current_stock=EXCLUDED.current_stock,
                    min_stock=EXCLUDED.min_stock, avg_daily_burn=EXCLUDED.avg_daily_burn, days_cover=EXCLUDED.days_cover,
                    lead_time_days=EXCLUDED.lead_time_days, safety_days=EXCLUDED.safety_days, reorder_point=EXCLUDED.reorder_point,
                    suggested_qty_purchase=EXCLUDED.suggested_qty_purchase, suggested_supplier_id=EXCLUDED.suggested_supplier_id, updated_at=now()")
                .bind(rm_id).bind(reason).bind(cur).bind(min).bind(burn).bind(cover).bind(lead).bind(st.safety_stock_days as i32)
                .bind(rp).bind(suggested).bind(supplier).execute(&mut *conn).await?;
            rec_open = true;
        } else if has_open {
            // on-order quantity now covers the need
            sqlx::query("UPDATE reorder_recommendations SET status='RESOLVED', updated_at=now() WHERE raw_material_id=$1 AND status='OPEN'")
                .bind(rm_id).execute(&mut *conn).await?;
        }
        if rec_open && matches!(reason, "BURN_RATE" | "BOTH") {
            let _ = alerts::raise(&mut *conn, alerts::NewAlert {
                event_type: "REORDER_RECOMMENDED", dedupe_key: &key, severity: "WARNING",
                title: &format!("Reorder recommended: {name}"),
                body: &format!("{} in store, ~{} per day; reorder point {}", cur.normalize(), burn.normalize(), rp.normalize()), url,
                payload: json!({ "raw_material_id": rm_id, "reason": reason }),
            }).await?;
        }
    } else {
        sqlx::query("UPDATE reorder_recommendations SET status='RESOLVED', updated_at=now() WHERE raw_material_id=$1 AND status='OPEN'")
            .bind(rm_id).execute(&mut *conn).await?;
    }
    if !(rec_open && matches!(reason, Some("BURN_RATE") | Some("BOTH"))) {
        alerts::resolve(&mut *conn, "REORDER_RECOMMENDED", &key).await?;
    }
    Ok(())
}

/// STOCK_EXPIRY alerts (key `lot:<lot_id>:<zone>`) for RAW and PRODUCT lots (zones other than SCRAP_YARD) expiring within
/// `alerts.expiry_lead_days` or already expired; resolved when the lot's quantity in that zone reaches 0.
pub async fn check_expiry(conn: &mut PgConnection, st: &Settings) -> AppResult<i64> {
    let today = util::plant_today(&mut *conn).await?;
    let horizon = today + chrono::Duration::days(st.expiry_lead_days);
    let rows = sqlx::query(
        "SELECT sb.item_type, sb.item_id, sb.zone, sb.lot_id, sb.qty, sb.expiry_date, COALESCE(rm.name, p.name, '?') AS name
         FROM stock_balances sb
         LEFT JOIN raw_materials rm ON sb.item_type = 'RAW' AND rm.id = sb.item_id
         LEFT JOIN products p ON sb.item_type = 'PRODUCT' AND p.id = sb.item_id
         WHERE sb.qty > 0 AND sb.zone <> 'SCRAP_YARD' AND sb.expiry_date IS NOT NULL AND sb.expiry_date <= $1
         ORDER BY sb.expiry_date LIMIT 500")
        .bind(horizon).fetch_all(&mut *conn).await?;
    let mut raised = 0;
    for r in &rows {
        let lot: Uuid = r.get("lot_id");
        let zone: String = r.get("zone");
        let exp: chrono::NaiveDate = r.get("expiry_date");
        let name: String = r.get("name");
        let qty: Decimal = r.get("qty");
        let item_type: String = r.get("item_type");
        let key = format!("lot:{lot}:{zone}");
        let expired = exp < today;
        let created = alerts::raise(&mut *conn, alerts::NewAlert {
            event_type: "STOCK_EXPIRY", dedupe_key: &key, severity: if expired { "CRITICAL" } else { "WARNING" },
            title: &format!("{}: {name}", if expired { "Expired stock" } else { "Stock nearing expiry" }),
            body: &format!("{} in {zone}, expiry {exp}", qty.normalize()),
            url: if item_type == "RAW" { "/raw-materials.html" } else { "/warehouse.html" },
            payload: json!({ "item_type": item_type, "item_id": r.get::<Uuid, _>("item_id"), "lot_id": lot, "zone": zone, "expiry_date": exp.to_string() }),
        }).await?;
        if created { raised += 1; }
    }
    sqlx::query(
        "UPDATE alerts SET resolved_at = now() WHERE event_type = 'STOCK_EXPIRY' AND resolved_at IS NULL AND NOT EXISTS (
           SELECT 1 FROM stock_balances sb WHERE sb.qty > 0 AND sb.zone <> 'SCRAP_YARD' AND sb.expiry_date IS NOT NULL
             AND 'lot:' || sb.lot_id::text || ':' || sb.zone = alerts.dedupe_key)")
        .execute(&mut *conn).await?;
    Ok(raised)
}
