//! Demo data seeding (`water-erp seed-demo`). OWNER: Subagent A.
//!
//! Idempotent: users / master data are inserted by stable code or name; the operational part (GRNs, blow runs, batches, fleet,
//! stock) is guarded by a `seed-demo` marker in the stock ledger, so a second run never posts stock twice. All stock moves go
//! through `crate::stock`. QR labels are OFF by default, so nothing here needs QR sets; inactive QR sets are inserted for two
//! batches only so the feature can be demonstrated after switching it on.
//! Demo accounts use a KNOWN password - never run this against a production database.
use crate::{
    routes::admin::common::hash_password_blocking,
    stock::{self, ItemType, Move, NIL_LOT},
    util,
};
use anyhow::Context;
use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use std::{collections::HashMap, str::FromStr};
use uuid::Uuid;

pub const DEMO_PASSWORD: &str = "Demo@12345!";
const MARK: &str = "seed-demo";

fn d(s: &str) -> Decimal { Decimal::from_str(s).expect("decimal literal") }

pub async fn run(db: &PgPool) -> anyhow::Result<()> {
    // hash outside the transaction (CPU heavy); one hash per account (own salts)
    let users = demo_users();
    let mut hashes = Vec::new();
    for _ in &users {
        let h = tokio::task::spawn_blocking(|| hash_password_blocking(DEMO_PASSWORD)).await?.map_err(|e| anyhow::anyhow!("{e}"))?;
        hashes.push(h);
    }
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('water-erp-seed-demo'))").execute(&mut *tx).await?;
    let ids = seed_users(&mut tx, &users, &hashes).await?;
    let today = util::plant_today(&mut tx).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let m = seed_master(&mut tx, today).await?;
    let done: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM stock_ledger WHERE note LIKE 'seed-demo%')").fetch_one(&mut *tx).await?;
    if done {
        tracing::info!("seed-demo: operational demo data already present, skipping stock/batches");
    } else {
        seed_operations(&mut tx, &ids, &m, today).await.context("seeding operations")?;
    }
    tx.commit().await?;
    tracing::warn!("seed-demo finished. Demo logins (password {DEMO_PASSWORD}): admin, manager, loader, qc, gate, store, operator, jars - DEVELOPMENT ONLY");
    println!("Demo data ready. Logins: admin, manager, loader, qc, gate, store, operator, jars  (password: {DEMO_PASSWORD})");
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------------------------------------------------

struct DemoUser { name: &'static str, full: &'static str, admin: bool, pages: &'static [&'static str], subs: &'static [&'static str] }

fn demo_users() -> Vec<DemoUser> {
    const ALL_BUT_ADMIN: &[&str] = &["dashboard", "raw-materials", "production", "qc", "warehouse", "qr-labels", "loading", "jars", "maintenance"];
    vec![
        DemoUser { name: "admin", full: "Demo Administrator", admin: true, pages: ALL_BUT_ADMIN, subs: &["LOW_STOCK", "REORDER_RECOMMENDED", "QC_PENDING", "QC_REJECTED", "WATER_OUT_OF_RANGE", "MAINTENANCE_DUE", "VEHICLE_DOC_EXPIRY", "JAR_OVERDUE", "JAR_FLEET_ALERT", "STOCK_EXPIRY"] },
        DemoUser { name: "manager", full: "Plant Manager", admin: false, pages: ALL_BUT_ADMIN, subs: &["LOW_STOCK", "QC_REJECTED", "WATER_OUT_OF_RANGE", "MAINTENANCE_DUE", "VEHICLE_DOC_EXPIRY", "JAR_OVERDUE", "STOCK_EXPIRY"] },
        DemoUser { name: "loader", full: "Loading Supervisor", admin: false, pages: &["loading", "qr-labels", "warehouse", "dashboard"], subs: &["GATE_PASS_ISSUED", "QC_APPROVED"] },
        DemoUser { name: "qc", full: "Lab Analyst", admin: false, pages: &["qc", "dashboard"], subs: &["QC_PENDING", "WATER_OUT_OF_RANGE"] },
        DemoUser { name: "gate", full: "Gate Keeper", admin: false, pages: &["loading", "dashboard"], subs: &["GATE_PASS_ISSUED", "TRIP_SETTLED"] },
        DemoUser { name: "store", full: "Store Keeper", admin: false, pages: &["raw-materials", "warehouse", "dashboard"], subs: &["LOW_STOCK", "REORDER_RECOMMENDED", "STOCK_EXPIRY"] },
        DemoUser { name: "operator", full: "Line Operator", admin: false, pages: &["production", "dashboard"], subs: &["QC_APPROVED", "QC_REJECTED", "WATER_OUT_OF_RANGE"] },
        DemoUser { name: "jars", full: "Jar Section Lead", admin: false, pages: &["jars", "maintenance", "dashboard"], subs: &["JAR_OVERDUE", "JAR_FLEET_ALERT", "MAINTENANCE_DUE"] },
    ]
}

async fn seed_users(tx: &mut PgConnection, users: &[DemoUser], hashes: &[String]) -> anyhow::Result<HashMap<&'static str, Uuid>> {
    let mut ids = HashMap::new();
    for (u, h) in users.iter().zip(hashes) {
        sqlx::query("INSERT INTO users (username, full_name, password_hash, is_admin, allowed_pages, subscribed_notifications) VALUES ($1,$2,$3,$4,$5,$6)
                     ON CONFLICT (lower(username)) DO NOTHING")
            .bind(u.name).bind(u.full).bind(h).bind(u.admin).bind(json!(u.pages)).bind(json!(u.subs)).execute(&mut *tx).await?;
        let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE lower(username) = $1").bind(u.name).fetch_one(&mut *tx).await?;
        ids.insert(u.name, id);
    }
    Ok(ids)
}

// ---------------------------------------------------------------------------------------------------------------------
// Master data
// ---------------------------------------------------------------------------------------------------------------------

struct Master {
    suppliers: Vec<Uuid>,
    mat: HashMap<&'static str, Uuid>,
    prod: HashMap<&'static str, Uuid>,
    cust: Vec<Uuid>,
    equip: HashMap<&'static str, Uuid>,
}

async fn seed_master(tx: &mut PgConnection, today: NaiveDate) -> anyhow::Result<Master> {
    // suppliers
    let mut suppliers = vec![];
    for (name, contact, phone, email, lead) in [
        ("Polytech Preforms Pvt Ltd", "A. Kulkarni", "+91 20 5550 1001", "sales@polytech.example", 7),
        ("CapSeal Packaging Industries", "R. Shah", "+91 22 5550 2002", "orders@capseal.example", 10),
        ("MineralChem Traders", "S. Iyer", "+91 80 5550 3003", "supply@mineralchem.example", 5),
    ] {
        let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM suppliers WHERE name = $1").bind(name).fetch_optional(&mut *tx).await?;
        let id = match id {
            Some(i) => i,
            None => sqlx::query_scalar("INSERT INTO suppliers (name, contact_person, phone, email, address, lead_time_days) VALUES ($1,$2,$3,$4,'Industrial Estate, Demo City',$5) RETURNING id")
                .bind(name).bind(contact).bind(phone).bind(email).bind(lead).fetch_one(&mut *tx).await?,
        };
        suppliers.push(id);
    }
    // raw materials: code, name, category, stock_unit, purchase_unit, stock_per_purchase_unit, unit_weight_g, min_stock, supplier idx
    let mats: [(&str, &str, &str, &str, &str, &str, Option<&str>, &str, usize); 16] = [
        ("PF-500", "Preform 500 ml (18 g)", "PREFORM", "pcs", "bag", "2500", Some("18"), "10000", 0),
        ("PF-1000", "Preform 1 L (28 g)", "PREFORM", "pcs", "bag", "1500", Some("28"), "5000", 0),
        ("EB-500", "Empty bottle 500 ml", "EMPTY_BOTTLE", "pcs", "pcs", "1", None, "2000", 0),
        ("EB-1000", "Empty bottle 1 L", "EMPTY_BOTTLE", "pcs", "pcs", "1", None, "1000", 0),
        ("CAP-28", "Cap 28 mm", "CAP", "pcs", "bag", "5000", None, "10000", 1),
        ("LBL-500", "Label 500 ml", "LABEL", "pcs", "roll", "3000", None, "6000", 1),
        ("LBL-1000", "Label 1 L", "LABEL", "pcs", "roll", "2000", None, "2000", 1),
        ("SLV-CRATE", "Crate shrink sleeve", "SLEEVE", "pcs", "bundle", "100", None, "100", 1),
        ("SHR-FILM", "Shrink film", "SHRINK_FILM", "kg", "roll", "20", None, "50", 1),
        ("CTN-500", "Carton tray 500 ml x 24", "CARTON", "pcs", "bundle", "50", None, "200", 1),
        ("CTN-1000", "Carton tray 1 L x 12", "CARTON", "pcs", "bundle", "50", None, "100", 1),
        ("MIN-CACL", "Calcium chloride", "MINERAL", "g", "kg", "1000", None, "5000", 2),
        ("MIN-MGSO4", "Magnesium sulphate", "MINERAL", "g", "kg", "1000", None, "5000", 2),
        ("CHEM-AS", "Antiscalant", "CHEMICAL", "ml", "litre", "1000", None, "2000", 2),
        ("JAR-SEAL", "Jar seal cap 20 L", "CAP", "pcs", "bag", "500", None, "100", 1),
        ("JAR-LBL", "Jar label 20 L", "LABEL", "pcs", "roll", "1000", None, "200", 1),
    ];
    let mut mat = HashMap::new();
    for (code, name, cat, su, pu, spp, w, min, sidx) in mats {
        sqlx::query("INSERT INTO raw_materials (code, name, category, stock_unit, purchase_unit, stock_per_purchase_unit, unit_weight_g, min_stock, default_supplier_id)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (code) DO NOTHING")
            .bind(code).bind(name).bind(cat).bind(su).bind(pu).bind(d(spp)).bind(w.map(d)).bind(d(min)).bind(suppliers[sidx]).execute(&mut *tx).await?;
        let id: Uuid = sqlx::query_scalar("SELECT id FROM raw_materials WHERE code = $1").bind(code).fetch_one(&mut *tx).await?;
        mat.insert(code, id);
    }
    // products: code, name, kind, size_ml, upc, cpp, shelf, min_stock_crates, preform, bottle, max_cycles
    let prods: [(&str, &str, &str, i32, i32, Option<i32>, i32, &str, Option<&str>, Option<&str>, Option<i32>); 3] = [
        ("MW-500", "Mineral Water 500 ml", "BOTTLED", 500, 24, Some(60), 180, "200", Some("PF-500"), Some("EB-500"), None),
        ("MW-1000", "Mineral Water 1 L", "BOTTLED", 1000, 12, Some(80), 180, "150", Some("PF-1000"), Some("EB-1000"), None),
        ("MW-20L", "Mineral Water 20 L Returnable Jar", "RETURNABLE", 20000, 1, None, 60, "50", None, None, Some(50)),
    ];
    let mut prod = HashMap::new();
    for (code, name, kind, size, upc, cpp, shelf, min, pre, bot, maxc) in prods {
        sqlx::query("INSERT INTO products (code, name, kind, size_ml, units_per_crate, crates_per_pallet, shelf_life_days, min_stock_crates, preform_material_id, bottle_material_id, max_cycles)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT (code) DO NOTHING")
            .bind(code).bind(name).bind(kind).bind(size).bind(upc).bind(cpp).bind(shelf).bind(d(min))
            .bind(pre.map(|c| mat[c])).bind(bot.map(|c| mat[c])).bind(maxc).execute(&mut *tx).await?;
        let id: Uuid = sqlx::query_scalar("SELECT id FROM products WHERE code = $1").bind(code).fetch_one(&mut *tx).await?;
        prod.insert(code, id);
    }
    // BOMs (only when the product has none yet): material, qty, basis, zone
    let dose = [("MIN-CACL", "60", "PER_1000L", "RM_STORE"), ("MIN-MGSO4", "50", "PER_1000L", "RM_STORE"), ("CHEM-AS", "15", "PER_1000L", "RM_STORE")];
    let boms: Vec<(&str, Vec<(&str, &str, &str, &str)>)> = vec![
        ("MW-500", [vec![("EB-500", "24", "PER_CRATE", "BLOW_FLOOR"), ("CAP-28", "24", "PER_CRATE", "RM_STORE"), ("LBL-500", "24", "PER_CRATE", "RM_STORE"),
                         ("SLV-CRATE", "1", "PER_CRATE", "RM_STORE"), ("SHR-FILM", "0.05", "PER_CRATE", "RM_STORE"), ("CTN-500", "1", "PER_CRATE", "RM_STORE")], dose.to_vec()].concat()),
        ("MW-1000", [vec![("EB-1000", "12", "PER_CRATE", "BLOW_FLOOR"), ("CAP-28", "12", "PER_CRATE", "RM_STORE"), ("LBL-1000", "12", "PER_CRATE", "RM_STORE"),
                          ("SLV-CRATE", "1", "PER_CRATE", "RM_STORE"), ("SHR-FILM", "0.06", "PER_CRATE", "RM_STORE"), ("CTN-1000", "1", "PER_CRATE", "RM_STORE")], dose.to_vec()].concat()),
        ("MW-20L", [vec![("JAR-SEAL", "1", "PER_CRATE", "RM_STORE"), ("JAR-LBL", "1", "PER_CRATE", "RM_STORE")], dose.to_vec()].concat()),
    ];
    for (pcode, lines) in boms {
        let pid = prod[pcode];
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM bom_items WHERE product_id = $1").bind(pid).fetch_one(&mut *tx).await?;
        if n > 0 { continue; }
        for (mcode, qty, basis, zone) in lines {
            sqlx::query("INSERT INTO bom_items (product_id, raw_material_id, qty_per_crate, basis, source_zone) VALUES ($1,$2,$3,$4,$5)")
                .bind(pid).bind(mat[mcode]).bind(d(qty)).bind(basis).bind(zone).execute(&mut *tx).await?;
        }
    }
    // route + customers
    sqlx::query("INSERT INTO routes (name, description) VALUES ('Route A - City Centre', 'Daily city centre round') ON CONFLICT (name) DO NOTHING").execute(&mut *tx).await?;
    let route: Uuid = sqlx::query_scalar("SELECT id FROM routes WHERE name = 'Route A - City Centre'").fetch_one(&mut *tx).await?;
    let mut cust: Vec<Uuid> = vec![];
    for (code, name, phone, addr) in [
        ("C-001", "Sunrise Super Market", "+91 98200 10001", "12 Market Road"), ("C-002", "Green Valley Apartments", "+91 98200 10002", "Green Valley Society"),
        ("C-003", "Blue Lagoon Restaurant", "+91 98200 10003", "5 Lake View"), ("C-004", "City Hospital Canteen", "+91 98200 10004", "City Hospital, Block B"),
        ("C-005", "Nandini General Store", "+91 98200 10005", "44 Station Road"),
    ] {
        sqlx::query("INSERT INTO customers (code, name, phone, address, route_id) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (code) DO NOTHING")
            .bind(code).bind(name).bind(phone).bind(addr).bind(route).execute(&mut *tx).await?;
        cust.push(sqlx::query_scalar("SELECT id FROM customers WHERE code = $1").bind(code).fetch_one(&mut *tx).await?);
    }
    // vehicles (the third has an expired PUC)
    let mut vehicles: Vec<Uuid> = vec![];
    for (reg, name, cap, driver, phone, puc_offset) in [
        ("MH12 AB 1234", "Tata Ace", 300, "Ramesh Patil", "+91 98900 20001", 200i64),
        ("MH12 CD 5678", "Mahindra Bolero Pickup", 200, "Suresh Jadhav", "+91 98900 20002", 120),
        ("MH14 EF 9012", "Eicher Mini Truck", 150, "Imran Shaikh", "+91 98900 20003", -20),
    ] {
        sqlx::query("INSERT INTO vehicles (reg_no, name, capacity_crates, driver_name, driver_phone, insurance_expiry, fitness_expiry, puc_expiry, permit_expiry)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (reg_no) DO NOTHING")
            .bind(reg).bind(name).bind(cap).bind(driver).bind(phone)
            .bind(today + Duration::days(240)).bind(today + Duration::days(300)).bind(today + Duration::days(puc_offset)).bind(today + Duration::days(365))
            .execute(&mut *tx).await?;
        vehicles.push(sqlx::query_scalar("SELECT id FROM vehicles WHERE reg_no = $1").bind(reg).fetch_one(&mut *tx).await?);
    }
    // equipment + maintenance schedules
    let mut equip = HashMap::new();
    for (code, name, kind, loc) in [
        ("RO-01", "RO plant 2000 LPH", "RO", "Water treatment"), ("UV-01", "UV steriliser", "UV", "Water treatment"), ("OZ-01", "Ozonator", "OZONATOR", "Water treatment"),
        ("SF-01", "Sand / carbon filter", "FILTER", "Water treatment"), ("BM-01", "Blow molding machine", "BLOW_MOLDER", "Blow floor"), ("FL-01", "Rinse-fill-cap monoblock", "FILLER", "Filling hall"),
    ] {
        sqlx::query("INSERT INTO equipment (code, name, kind, location) VALUES ($1,$2,$3,$4) ON CONFLICT (code) DO NOTHING").bind(code).bind(name).bind(kind).bind(loc).execute(&mut *tx).await?;
        equip.insert(code, sqlx::query_scalar("SELECT id FROM equipment WHERE code = $1").bind(code).fetch_one(&mut *tx).await?);
    }
    let eq: HashMap<&str, Uuid> = equip.clone();
    for (ecode, task, interval, last, due) in [
        ("RO-01", "Membrane chemical clean", 30, -20i64, 10i64), ("UV-01", "Replace UV lamp", 365, -300, 65), ("OZ-01", "Ozone generator service", 90, -102, -12),
        ("BM-01", "Lubrication and heater check", 7, -5, 2), ("SF-01", "Backwash and media inspection", 14, -9, 5), ("FL-01", "Filling valve gasket replacement", 180, -150, 30),
    ] {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM maintenance_schedules WHERE equipment_id = $1 AND task = $2)").bind(eq[ecode]).bind(task).fetch_one(&mut *tx).await?;
        if !exists {
            sqlx::query("INSERT INTO maintenance_schedules (subject_type, equipment_id, task, interval_days, last_done_on, next_due_on) VALUES ('EQUIPMENT',$1,$2,$3,$4,$5)")
                .bind(eq[ecode]).bind(task).bind(interval).bind(today + Duration::days(last)).bind(today + Duration::days(due)).execute(&mut *tx).await?;
        }
    }
    let task = "Engine oil and brake service";
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM maintenance_schedules WHERE vehicle_id = $1 AND task = $2)").bind(vehicles[0]).bind(task).fetch_one(&mut *tx).await?;
    if !exists {
        sqlx::query("INSERT INTO maintenance_schedules (subject_type, vehicle_id, task, interval_days, last_done_on, next_due_on) VALUES ('VEHICLE',$1,$2,90,$3,$4)")
            .bind(vehicles[0]).bind(task).bind(today - Duration::days(65)).bind(today + Duration::days(25)).execute(&mut *tx).await?;
    }
    Ok(Master { suppliers, mat, prod, cust, equip })
}

// ---------------------------------------------------------------------------------------------------------------------
// Operations (stock, production, fleet)
// ---------------------------------------------------------------------------------------------------------------------

const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
fn qr_code() -> String {
    let mut b = [0u8; 10];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    b.iter().map(|x| CROCKFORD[(*x as usize) % 32] as char).collect()
}

async fn seed_operations(tx: &mut PgConnection, ids: &HashMap<&'static str, Uuid>, m: &Master, today: NaiveDate) -> anyhow::Result<()> {
    let admin = ids["admin"];
    let plant_code: String = sqlx::query_scalar("SELECT value #>> '{}' FROM system_settings WHERE key = 'plant.code'").fetch_one(&mut *tx).await?;
    let note = |s: &str| format!("{MARK} {s}");

    // ---- goods received ----
    // (supplier idx, challan, days ago, [(material, purchase qty, lot_no, expiry in days)])
    type Line = (&'static str, &'static str, &'static str, Option<i64>);
    let grns: Vec<(usize, &str, i64, Vec<Line>)> = vec![
        (0, "PP-2041", 20, vec![("PF-500", "12", "PF500-L1", None), ("PF-1000", "8", "PF1000-L1", None)]),
        (1, "CS-7781", 18, vec![("CAP-28", "6", "CAP-L1", Some(420)), ("LBL-500", "10", "LB5-L1", Some(500)), ("LBL-1000", "4", "LB1-L1", Some(500)), ("SLV-CRATE", "20", "SLV-L1", None),
                                 ("SHR-FILM", "2", "SF-L1", None), ("CTN-500", "24", "C5-L1", None), ("CTN-1000", "12", "C1-L1", None), ("JAR-SEAL", "1", "JS-L1", Some(700)), ("JAR-LBL", "1", "JL-L1", Some(700))]),
        (2, "MC-310", 15, vec![("MIN-CACL", "15", "CACL-L1", Some(200)), ("MIN-MGSO4", "25", "MGSO4-L1", Some(380)), ("CHEM-AS", "20", "AS-L1", Some(300))]),
        (2, "MC-352", 3, vec![("MIN-CACL", "10", "CACL-L2", Some(420))]),
        (1, "CS-7902", 6, vec![("CAP-28", "4", "CAP-L2", Some(650))]),
    ];
    for (sidx, challan, days_ago, lines) in grns {
        let grn_no = util::next_no(&mut *tx, "grn_seq", "GRN").await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let grn_id: Uuid = sqlx::query_scalar("INSERT INTO grns (grn_no, supplier_id, received_on, challan_no, notes, received_by) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id")
            .bind(&grn_no).bind(m.suppliers[sidx]).bind(today - Duration::days(days_ago)).bind(challan).bind(note("goods received")).bind(admin).fetch_one(&mut *tx).await?;
        for (mcode, qty, lot_no, exp) in lines {
            let mid = m.mat[mcode];
            let spp: Decimal = sqlx::query_scalar("SELECT stock_per_purchase_unit FROM raw_materials WHERE id = $1").bind(mid).fetch_one(&mut *tx).await?;
            let qp = d(qty);
            let expiry = exp.map(|e| today + Duration::days(e));
            let lot: Uuid = sqlx::query_scalar("INSERT INTO grn_lines (grn_id, raw_material_id, qty_purchase, qty_stock, lot_no, expiry_date) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id")
                .bind(grn_id).bind(mid).bind(qp).bind(qp * spp).bind(lot_no).bind(expiry).fetch_one(&mut *tx).await?;
            stock::post_all(&mut *tx, vec![Move::new(ItemType::Raw, mid, "RM_STORE", lot, qp * spp, "GRN").reference("GRN", grn_id).by(Some(admin)).note(note(&grn_no)).expiry(expiry)])
                .await.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }

    // ---- blow molding: preforms -> empty bottles on the blow floor ----
    for (pre, bottle, issued, good, days_ago) in [("PF-500", "EB-500", 12240i64, 12000i64, 8i64), ("PF-1000", "EB-1000", 3060, 3000, 8)] {
        let scrap = issued - good;
        let run_no = util::next_no(&mut *tx, "blow_seq", "BLW").await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let weight: Decimal = sqlx::query_scalar("SELECT unit_weight_g FROM raw_materials WHERE id = $1").bind(m.mat[pre]).fetch_one(&mut *tx).await?;
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO blow_molding_runs (run_no, run_date, shift, equipment_id, preform_material_id, bottle_material_id, preforms_issued, good_bottles, scrap_pcs, scrap_pct, scrap_weight_kg, notes, created_by)
             VALUES ($1,$2,'DAY',$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING id")
            .bind(&run_no).bind(today - Duration::days(days_ago)).bind(m.equip["BM-01"]).bind(m.mat[pre]).bind(m.mat[bottle]).bind(Decimal::from(issued)).bind(Decimal::from(good)).bind(Decimal::from(scrap))
            .bind((Decimal::from(scrap) * Decimal::from(100) / Decimal::from(issued)).round_dp(3)).bind(Decimal::from(scrap) * weight / Decimal::from(1000)).bind(note("demo run")).bind(admin)
            .fetch_one(&mut *tx).await?;
        stock::consume_fefo(&mut *tx, ItemType::Raw, m.mat[pre], "RM_STORE", Decimal::from(issued), "BLOW_ISSUE", Some("BLOW_RUN"), Some(run_id), Some(admin), &note(&run_no)).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        stock::post_all(&mut *tx, vec![
            Move::new(ItemType::Raw, m.mat[bottle], "BLOW_FLOOR", run_id, Decimal::from(good), "BLOW_OUTPUT").reference("BLOW_RUN", run_id).by(Some(admin)).note(note(&run_no)),
            Move::new(ItemType::Raw, m.mat[pre], "SCRAP_YARD", run_id, Decimal::from(scrap), "BLOW_SCRAP").reference("BLOW_RUN", run_id).by(Some(admin)).note(note(&run_no)),
        ]).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    // ---- water treatment logs (operator), the last one a bit out of range ----
    let operator = ids["operator"];
    let mut last_ok_log: Option<Uuid> = None;
    for (mins_ago, tds, ph) in [(480i64, "142", "7.4"), (360, "139", "7.5"), (240, "151", "7.3"), (120, "147", "9.3"), (35, "144", "7.4")] {
        let ph_d = d(ph);
        let oor = ph_d > d("8.5") || ph_d < d("6.5");
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO water_treatment_logs (logged_at, tds_ppm, ph, ozone_ppm, uv_ok, minerals, out_of_range, notes, logged_by) VALUES (now() - make_interval(mins => $1),$2,$3,0.25,true,$4,$5,$6,$7) RETURNING id")
            .bind(mins_ago as i32).bind(d(tds)).bind(ph_d).bind(json!({ "Calcium chloride": 60, "Magnesium sulphate": 50 })).bind(oor).bind(if oor { "pH high - dosing pump re-calibrated" } else { "" }).bind(operator).fetch_one(&mut *tx).await?;
        if !oor { last_ok_log = Some(id); }
    }

    // ---- returnable container fleet (20 L jars) ----
    let jar = m.prod["MW-20L"];
    let supplier = m.suppliers[1];
    let ev = |event: &'static str, qty: i32, reason: Option<&'static str>, loc: Option<&'static str>, cust: Option<Uuid>, sup: Option<Uuid>, days_ago: i64, n: String| {
        (event, qty, reason, loc, cust, sup, days_ago, n)
    };
    stock::post_all(&mut *tx, vec![Move::new(ItemType::Product, jar, "JAR_EMPTY", NIL_LOT, Decimal::from(430), "JAR_PURCHASE").by(Some(admin)).note(note("initial fleet purchase 430"))]).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    stock::transfer(&mut *tx, ItemType::Product, jar, NIL_LOT, "JAR_EMPTY", "JAR_CLEAN", Decimal::from(280), "JAR_WASH", None, None, Some(admin), &note("washed 280")).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    stock::transfer(&mut *tx, ItemType::Product, jar, NIL_LOT, "JAR_EMPTY", "SCRAP_YARD", Decimal::from(5), "JAR_WASH_REJECT", None, None, Some(admin), &note("5 failed the wash inspection")).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    stock::transfer(&mut *tx, ItemType::Product, jar, NIL_LOT, "JAR_EMPTY", "SCRAP_YARD", Decimal::from(3), "JAR_DESTROY", None, None, Some(admin), &note("3 cracked jars")).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let events = vec![
        ev("NEW_PURCHASED", 400, None, None, None, Some(supplier), 40, "first fleet purchase".into()),
        ev("NEW_PURCHASED", 30, None, None, None, Some(supplier), 12, "top-up purchase".into()),
        ev("WASHED", 280, None, None, None, None, 9, "weekly wash".into()),
        ev("DESTROYED", 5, Some("WASH_REJECT"), Some("PLANT_EMPTY"), None, None, 9, "failed wash inspection".into()),
        ev("DESTROYED", 3, Some("CRACKED"), Some("PLANT_EMPTY"), None, None, 7, "cracked in storage".into()),
        ev("DESTROYED", 1, Some("DAMAGED_RETURN"), Some("CUSTOMER"), Some(m.cust[1]), None, 5, "returned broken".into()),
        ev("DESTROYED", 2, Some("LOST"), Some("CUSTOMER"), Some(m.cust[2]), None, 20, "customer reported lost".into()),
    ];
    for (event, qty, reason, loc, cust, sup, days_ago, n) in events {
        sqlx::query("INSERT INTO jar_fleet_events (product_id, event_type, qty, reason_code, location, customer_id, supplier_id, note, created_by, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, now() - make_interval(days => $10))")
            .bind(jar).bind(event).bind(qty).bind(reason).bind(loc).bind(cust).bind(sup).bind(note(&n)).bind(admin).bind(days_ago as i32).execute(&mut *tx).await?;
    }
    // jars held by customers: (customer idx, txn, delta, days ago)
    for (ci, txn, delta, days_ago) in [
        (0usize, "ISSUE_FILLED", 20i32, 45i64), (0, "RETURN_EMPTY", -5, 10), (1, "ISSUE_FILLED", 15, 12), (1, "DAMAGED", -1, 5),
        (2, "OPENING", 30, 90), (2, "LOST", -2, 20), (3, "ISSUE_FILLED", 10, 3),
    ] {
        sqlx::query("INSERT INTO jar_ledger (customer_id, product_id, txn_type, delta, txn_date, note, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(m.cust[ci]).bind(jar).bind(txn).bind(delta).bind(today - Duration::days(days_ago)).bind(note("demo")).bind(admin).execute(&mut *tx).await?;
    }

    // ---- production batches through the real stock flow ----
    let qc_user = ids["qc"];
    struct B { prod: &'static str, crates: i32, days_ago: i64, status: &'static str }
    let batches = [
        B { prod: "MW-500", crates: 200, days_ago: 6, status: "APPROVED" }, B { prod: "MW-500", crates: 150, days_ago: 3, status: "APPROVED" },
        B { prod: "MW-1000", crates: 120, days_ago: 4, status: "APPROVED" }, B { prod: "MW-1000", crates: 20, days_ago: 2, status: "REJECTED" },
        B { prod: "MW-20L", crates: 60, days_ago: 3, status: "APPROVED" }, B { prod: "MW-500", crates: 100, days_ago: 0, status: "QUARANTINE" },
        B { prod: "MW-20L", crates: 30, days_ago: 0, status: "QUARANTINE" },
    ];
    let mut batch_ids: Vec<(Uuid, &'static str, i32, &'static str)> = vec![];
    for b in &batches {
        let pid = m.prod[b.prod];
        let (kind, size_ml, upc, shelf): (String, Option<i32>, i32, i32) = sqlx::query_as("SELECT kind, size_ml, units_per_crate, shelf_life_days FROM products WHERE id = $1").bind(pid).fetch_one(&mut *tx).await?;
        let mfg = today - Duration::days(b.days_ago);
        let expiry = mfg + Duration::days(shelf as i64);
        let batch_no = util::next_no(&mut *tx, "batch_seq", &format!("{plant_code}-B")).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let batch_id: Uuid = sqlx::query_scalar(
            "INSERT INTO production_batches (batch_no, product_id, produced_on, shift, crates, qc_status, mfg_date, expiry_date, water_log_id, notes, created_by, created_at)
             VALUES ($1,$2,$3,'DAY',$4,'QUARANTINE',$3,$5,$6,$7,$8, now() - make_interval(days => $9)) RETURNING id")
            .bind(&batch_no).bind(pid).bind(mfg).bind(b.crates).bind(expiry).bind(last_ok_log).bind(note("demo batch")).bind(ids["operator"]).bind(b.days_ago as i32).fetch_one(&mut *tx).await?;
        // consume the BOM (PER_1000L lines scale with the litres in the batch)
        let litres = Decimal::from(b.crates as i64) * Decimal::from(upc as i64) * Decimal::from(size_ml.unwrap_or(0) as i64) / Decimal::from(1000);
        let lines: Vec<(Uuid, Decimal, String, String)> = sqlx::query_as("SELECT raw_material_id, qty_per_crate, basis, source_zone FROM bom_items WHERE product_id = $1 ORDER BY raw_material_id").bind(pid).fetch_all(&mut *tx).await?;
        for (mid, q, basis, zone) in lines {
            let need = if basis == "PER_1000L" { q * litres / Decimal::from(1000) } else { q * Decimal::from(b.crates as i64) }.round_dp(4);
            let zone: &'static str = if zone == "BLOW_FLOOR" { "BLOW_FLOOR" } else { "RM_STORE" };
            stock::consume_fefo(&mut *tx, ItemType::Raw, mid, zone, need, "BATCH_CONSUME", Some("BATCH"), Some(batch_id), Some(operator), &note(&batch_no)).await.map_err(|e| anyhow::anyhow!("batch {batch_no}: {e}"))?;
            sqlx::query("INSERT INTO batch_material_usage (batch_id, raw_material_id, qty, zone) VALUES ($1,$2,$3,$4)").bind(batch_id).bind(mid).bind(need).bind(zone).execute(&mut *tx).await?;
        }
        if kind == "RETURNABLE" {
            stock::consume_fefo(&mut *tx, ItemType::Product, pid, "JAR_CLEAN", Decimal::from(b.crates as i64), "BATCH_CONSUME", Some("BATCH"), Some(batch_id), Some(operator), &note(&batch_no)).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        stock::post_all(&mut *tx, vec![Move::new(ItemType::Product, pid, "QC_QUARANTINE", batch_id, Decimal::from(b.crates as i64), "BATCH_OUTPUT")
            .reference("BATCH", batch_id).by(Some(operator)).note(note(&batch_no)).expiry(Some(expiry))]).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        if b.status != "QUARANTINE" {
            let (result, microbial) = if b.status == "APPROVED" { ("PASS", true) } else { ("FAIL", false) };
            sqlx::query("INSERT INTO qc_tests (batch_id, tested_by, tds_ppm, ph, ozone_ppm, turbidity_ntu, microbial_ok, seal_ok, label_ok, result, remarks) VALUES ($1,$2,145,7.4,0.22,0.3,$3,true,true,$4,$5)")
                .bind(batch_id).bind(qc_user).bind(microbial).bind(result).bind(if microbial { "all parameters within limits" } else { "microbial count above limit" }).execute(&mut *tx).await?;
            sqlx::query("UPDATE production_batches SET qc_status = $2, qc_decided_by = $3, qc_decided_at = now(), qc_notes = $4 WHERE id = $1")
                .bind(batch_id).bind(b.status).bind(qc_user).bind(if microbial { "Approved" } else { "Rejected: microbial count above limit" }).execute(&mut *tx).await?;
            let (to, tt): (&'static str, &'static str) = if microbial { ("FG_READY", "QC_RELEASE") } else { ("SCRAP_YARD", "QC_REJECT") };
            stock::transfer(&mut *tx, ItemType::Product, pid, batch_id, "QC_QUARANTINE", to, Decimal::from(b.crates as i64), tt, Some("BATCH"), Some(batch_id), Some(qc_user), &note(&batch_no)).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        batch_ids.push((batch_id, b.prod, b.crates, b.status));
    }

    // ---- optional QR sets (inactive while features.qr_labels = false) for the first approved and first quarantined 500 ml batch ----
    for (bid, prod_code, crates, status) in &batch_ids {
        if *prod_code != "MW-500" || (*status != "APPROVED" && *status != "QUARANTINE") { continue; }
        let already: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM qr_crate_sets WHERE batch_id = $1)").bind(bid).fetch_one(&mut *tx).await?;
        if already || (*status == "APPROVED" && *crates != 200) { continue; }
        let size = *crates / 4; // three labelled sets of a quarter each; the remainder stays unlabelled
        for (seq, label) in ["X", "Y", "Z"].iter().enumerate() {
            sqlx::query("INSERT INTO qr_crate_sets (code, batch_id, product_id, set_label, seq, original_crates, crates, status, created_by) VALUES ($1,$2,$3,$4,$5,$6,$6,$7,$8)")
                .bind(qr_code()).bind(bid).bind(m.prod["MW-500"]).bind(*label).bind(seq as i32 + 1).bind(size)
                .bind(if *status == "APPROVED" { "AVAILABLE" } else { "QUARANTINE" }).bind(admin).execute(&mut *tx).await?;
        }
    }

    // ---- one completed maintenance log so the history view is not empty ----
    let uv: Option<Uuid> = sqlx::query_scalar("SELECT id FROM maintenance_schedules WHERE equipment_id = $1 AND task = 'Replace UV lamp'").bind(m.equip["UV-01"]).fetch_optional(&mut *tx).await?;
    if let Some(sid) = uv {
        sqlx::query("INSERT INTO maintenance_logs (schedule_id, done_on, done_by, notes, next_due_on) VALUES ($1,$2,$3,'Lamp replaced, hours counter reset',$4)")
            .bind(sid).bind(today - Duration::days(300)).bind(admin).bind(today + Duration::days(65)).execute(&mut *tx).await?;
    }
    Ok(())
}
