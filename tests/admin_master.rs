mod common;
mod admin_support;
use admin_support::{admin, req};
use axum::{http::StatusCode, Router};
use serde_json::{json, Value};
use sqlx::PgPool;

async fn setup(pool: &PgPool) -> (Router, String) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, k) = admin(pool, "root").await;
    (app, k)
}
async fn post(app: &Router, k: &str, path: &str, body: Value) -> (StatusCode, Value) { req(app, "POST", &format!("/api/admin{path}"), Some(k), Some(body)).await }
async fn mat(app: &Router, k: &str, code: &str, cat: &str) -> String {
    let (s, b) = post(app, k, "/raw-materials", json!({"code":code,"name":format!("{code} name"),"category":cat})).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    b["id"].as_str().unwrap().to_string()
}

#[sqlx::test(migrations = "./migrations")]
async fn suppliers_crud_soft_delete_and_audit(pool: PgPool) {
    let (app, k) = setup(&pool).await;
    let (s, b) = post(&app, &k, "/suppliers", json!({"name":"Acme Caps","contact_person":"Raj","phone":"+91 98765-43210","email":"raj@acme.test","lead_time_days":10})).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let id = b["id"].as_str().unwrap().to_string();
    assert_eq!(b["lead_time_days"], 10);
    for bad in [json!({"name":""}), json!({"name":"x","lead_time_days":-1}), json!({"name":"x","lead_time_days":9999}), json!({"name":"x","email":"nope"}),
                json!({"name":"x","phone":"call me"}), json!({"name":"x\u{0000}"}), json!({"name":"x".repeat(121)})] {
        let (s, r) = post(&app, &k, "/suppliers", bad.clone()).await;
        assert!(s == StatusCode::UNPROCESSABLE_ENTITY || s == StatusCode::BAD_REQUEST, "{bad}: {s} {r}");
    }
    assert_eq!(post(&app, &k, "/suppliers", json!({"name":"x","lead_time_days":"abc"})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(post(&app, &k, "/suppliers", json!({"name":"x","is_admin":true})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(post(&app, &k, "/suppliers", json!({"name":"x","id":"00000000-0000-0000-0000-000000000000"})).await.0, StatusCode::BAD_REQUEST);
    let (s, u) = req(&app, "PUT", &format!("/api/admin/suppliers/{id}"), Some(&k), Some(json!({"lead_time_days":3}))).await;
    assert_eq!((s, u["lead_time_days"].as_i64(), u["name"].as_str()), (StatusCode::OK, Some(3), Some("Acme Caps")), "partial update keeps other fields");
    assert_eq!(req(&app, "PUT", &format!("/api/admin/suppliers/{}", uuid::Uuid::new_v4()), Some(&k), Some(json!({"name":"y"}))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(req(&app, "DELETE", &format!("/api/admin/suppliers/{id}"), Some(&k), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(req(&app, "DELETE", &format!("/api/admin/suppliers/{id}"), Some(&k), None).await.0, StatusCode::NO_CONTENT, "idempotent");
    let (_, l) = req(&app, "GET", "/api/admin/suppliers", Some(&k), None).await;
    assert_eq!(l.as_array().unwrap().len(), 1, "list includes inactive");
    assert_eq!(l[0]["is_active"], false);
    let (_, l) = req(&app, "GET", "/api/admin/suppliers?active=true", Some(&k), None).await;
    assert_eq!(l.as_array().unwrap().len(), 0);
    let (_, l) = req(&app, "GET", "/api/admin/suppliers?q=acme", Some(&k), None).await;
    assert_eq!(l.as_array().unwrap().len(), 1);
    let (_, l) = req(&app, "GET", "/api/admin/suppliers?q=%25", Some(&k), None).await;
    assert_eq!(l.as_array().unwrap().len(), 0, "% is escaped, not a wildcard");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action IN ('SUPPLIER_CREATE','SUPPLIER_UPDATE','SUPPLIER_DEACTIVATE')").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 4); // create, update, deactivate x2
    // row still there (no cascade / hard delete)
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM suppliers").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn raw_materials_validation_and_category_lock(pool: PgPool) {
    let (app, k) = setup(&pool).await;
    let (_, sup) = post(&app, &k, "/suppliers", json!({"name":"S"})).await;
    let sid = sup["id"].as_str().unwrap();
    let (s, m) = post(&app, &k, "/raw-materials", json!({"code":"PF-28","name":"Preform 28g","category":"PREFORM","stock_per_purchase_unit":"2000","unit_weight_g":"18.5","min_stock":10000,"default_supplier_id":sid})).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    assert_eq!(m["stock_per_purchase_unit"], "2000.0000");
    assert_eq!(m["default_supplier_name"], "S");
    let mid = m["id"].as_str().unwrap().to_string();
    assert_eq!(post(&app, &k, "/raw-materials", json!({"code":"PF-28","name":"dup","category":"CAP"})).await.1["error"]["code"], "DUPLICATE");
    for bad in [json!({"code":"A","name":"x","category":"WRONG"}), json!({"code":"A B","name":"x","category":"CAP"}), json!({"code":"A","name":"x","category":"CAP","min_stock":-1}),
                json!({"code":"A","name":"x","category":"CAP","stock_per_purchase_unit":0}), json!({"code":"A","name":"x","category":"CAP","min_stock":"1.123456"}),
                json!({"code":"A","name":"x","category":"CAP","min_stock":"9999999999"}), json!({"code":"A","name":"x","category":"CAP","default_supplier_id":uuid::Uuid::new_v4()}),
                json!({"code":"A","name":"x","category":"CAP","unit_weight_g":-3}), json!({"code":"'; DROP TABLE raw_materials;--","name":"x","category":"CAP"})] {
        let (s, r) = post(&app, &k, "/raw-materials", bad.clone()).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {r}");
    }
    // category change blocked once the material is used by a BOM / product mapping
    let (s, p) = post(&app, &k, "/products", json!({"code":"B500","name":"500 ml","kind":"BOTTLED","size_ml":500,"units_per_crate":24,"preform_material_id":mid})).await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    let (s, r) = req(&app, "PUT", &format!("/api/admin/raw-materials/{mid}"), Some(&k), Some(json!({"category":"CAP"}))).await;
    assert_eq!((s, r["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("IN_USE")));
    assert_eq!(req(&app, "PUT", &format!("/api/admin/raw-materials/{mid}"), Some(&k), Some(json!({"min_stock":5,"default_supplier_id":null}))).await.0, StatusCode::OK);
}

#[sqlx::test(migrations = "./migrations")]
async fn product_kind_rules(pool: PgPool) {
    let (app, k) = setup(&pool).await;
    let pre = mat(&app, &k, "PRE", "PREFORM").await;
    let bot = mat(&app, &k, "BOT", "EMPTY_BOTTLE").await;
    let cap = mat(&app, &k, "CAP", "CAP").await;
    let bottled = json!({"code":"P500","name":"500 ml","kind":"BOTTLED","size_ml":500,"units_per_crate":24,"crates_per_pallet":60,"shelf_life_days":180,
                         "preform_material_id":pre,"bottle_material_id":bot});
    let (s, p) = post(&app, &k, "/products", bottled.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert_eq!((p["crates_per_pallet"].as_i64(), p["preform_material_name"].as_str()), (Some(60), Some("PRE name")));
    let pid = p["id"].as_str().unwrap().to_string();
    // wrong categories for the mappings
    let mut b = bottled.clone(); b["code"] = json!("X1"); b["preform_material_id"] = json!(cap);
    assert_eq!(post(&app, &k, "/products", b).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let mut b = bottled.clone(); b["code"] = json!("X2"); b["bottle_material_id"] = json!(pre);
    assert_eq!(post(&app, &k, "/products", b).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let mut b = bottled.clone(); b["code"] = json!("X3"); b["max_cycles"] = json!(50);
    assert_eq!(post(&app, &k, "/products", b).await.0, StatusCode::UNPROCESSABLE_ENTITY, "max_cycles only for RETURNABLE");
    let mut b = bottled.clone(); b["code"] = json!("X4"); b["size_ml"] = Value::Null;
    assert_eq!(post(&app, &k, "/products", b).await.0, StatusCode::UNPROCESSABLE_ENTITY, "BOTTLED needs size");
    for (f, v) in [("units_per_crate", json!(0)), ("units_per_crate", json!(-4)), ("crates_per_pallet", json!(0)), ("shelf_life_days", json!(0)), ("size_ml", json!(-1)),
                   ("min_stock_crates", json!(-1)), ("kind", json!("JAR20L")), ("kind", json!("bottled"))] {
        let mut b = bottled.clone(); b["code"] = json!("Y"); b[f] = v.clone();
        let (s, r) = post(&app, &k, "/products", b).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{f}={v}: {r}");
    }
    // RETURNABLE: no preform/bottle, units_per_crate=1, max_cycles ok
    let jar = json!({"code":"JAR20","name":"20 L jar","kind":"RETURNABLE","size_ml":20000,"max_cycles":50});
    let (s, j) = post(&app, &k, "/products", jar.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    assert_eq!((j["units_per_crate"].as_i64(), j["max_cycles"].as_i64()), (Some(1), Some(50)), "defaults to one container per unit");
    let jid = j["id"].as_str().unwrap().to_string();
    for (f, v) in [("preform_material_id", json!(pre)), ("bottle_material_id", json!(bot)), ("units_per_crate", json!(12)), ("max_cycles", json!(0))] {
        let mut b = jar.clone(); b["code"] = json!("JARX"); b[f] = v.clone();
        assert_eq!(post(&app, &k, "/products", b).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{f}");
    }
    // converting BOTTLED -> RETURNABLE while preform mapped is rejected
    assert_eq!(req(&app, "PUT", &format!("/api/admin/products/{pid}"), Some(&k), Some(json!({"kind":"RETURNABLE","units_per_crate":1}))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    // ... and with mappings cleared but an EMPTY_BOTTLE BOM line it is still rejected
    let (s, _) = req(&app, "PUT", &format!("/api/admin/products/{pid}/bom"), Some(&k), Some(json!([{"raw_material_id":bot,"qty_per_crate":24}]))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, r) = req(&app, "PUT", &format!("/api/admin/products/{pid}"), Some(&k), Some(json!({"kind":"RETURNABLE","units_per_crate":1,"preform_material_id":null,"bottle_material_id":null}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    // RETURNABLE BOM may not contain PREFORM / EMPTY_BOTTLE
    for m in [&pre, &bot] {
        let (s, r) = req(&app, "PUT", &format!("/api/admin/products/{jid}/bom"), Some(&k), Some(json!({"lines":[{"raw_material_id":m,"qty_per_crate":1}]}))).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    }
    assert_eq!(req(&app, "PUT", &format!("/api/admin/products/{jid}/bom"), Some(&k), Some(json!({"items":[{"raw_material_id":cap,"qty_per_crate":1}]}))).await.0, StatusCode::OK);
    // duplicate code
    assert_eq!(post(&app, &k, "/products", bottled).await.1["error"]["code"], "DUPLICATE");
    // unknown / mass-assignment
    assert_eq!(post(&app, &k, "/products", json!({"code":"Z","name":"z","kind":"BOTTLED","size_ml":1,"is_admin":true})).await.0, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "./migrations")]
async fn bom_rules(pool: PgPool) {
    let (app, k) = setup(&pool).await;
    let bot = mat(&app, &k, "BOT", "EMPTY_BOTTLE").await;
    let cap = mat(&app, &k, "CAP", "CAP").await;
    let min = mat(&app, &k, "MIN", "MINERAL").await;
    let (_, p) = post(&app, &k, "/products", json!({"code":"P1","name":"1 L","kind":"BOTTLED","size_ml":1000,"units_per_crate":12})).await;
    let pid = p["id"].as_str().unwrap();
    let url = format!("/api/admin/products/{pid}/bom");
    let put = |b: Value| { let (app, k, url) = (app.clone(), k.clone(), url.clone()); async move { req(&app, "PUT", &url, Some(&k), Some(b)).await } };
    let (s, b) = put(json!([{"raw_material_id":bot,"qty_per_crate":12},{"raw_material_id":cap,"qty_per_crate":12},{"raw_material_id":min,"qty_per_crate":"0.350000","basis":"PER_1000L"}])).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let lines = b.as_array().unwrap();
    assert_eq!(lines.len(), 3);
    let by = |c: &str| lines.iter().find(|l| l["raw_material_code"] == c).unwrap().clone();
    assert_eq!((by("BOT")["source_zone"].as_str(), by("CAP")["source_zone"].as_str(), by("MIN")["basis"].as_str()), (Some("BLOW_FLOOR"), Some("RM_STORE"), Some("PER_1000L")));
    assert_eq!(by("CAP")["basis"], "PER_CRATE");
    assert_eq!(by("MIN")["qty_per_crate"], "0.350000");
    let (s, g) = req(&app, "GET", &url, Some(&k), None).await;
    assert_eq!((s, g.as_array().unwrap().len()), (StatusCode::OK, 3));
    // violations
    let bad: Vec<Value> = vec![
        json!([{"raw_material_id":bot,"qty_per_crate":12,"source_zone":"RM_STORE"}]),
        json!([{"raw_material_id":cap,"qty_per_crate":12,"source_zone":"BLOW_FLOOR"}]),
        json!([{"raw_material_id":cap,"qty_per_crate":0}]),
        json!([{"raw_material_id":cap,"qty_per_crate":-1}]),
        json!([{"raw_material_id":cap,"qty_per_crate":"1.1234567"}]),
        json!([{"raw_material_id":cap,"qty_per_crate":"2000000000"}]),
        json!([{"raw_material_id":cap,"qty_per_crate":1,"basis":"PER_LITRE"}]),
        json!([{"raw_material_id":cap,"qty_per_crate":1},{"raw_material_id":cap,"qty_per_crate":2}]),
        json!([{"raw_material_id":uuid::Uuid::new_v4(),"qty_per_crate":1}]),
    ];
    for b in bad {
        let (s, r) = put(b.clone()).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}: {r}");
    }
    assert_eq!(put(json!([{"raw_material_id":cap,"qty_per_crate":1,"extra":1}])).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(put(json!("nope")).await.0, StatusCode::BAD_REQUEST);
    // failed PUTs left the BOM untouched (atomic)
    let (_, g) = req(&app, "GET", &url, Some(&k), None).await;
    assert_eq!(g.as_array().unwrap().len(), 3);
    // inactive material refused
    req(&app, "DELETE", &format!("/api/admin/raw-materials/{cap}"), Some(&k), None).await;
    assert_eq!(put(json!([{"raw_material_id":cap,"qty_per_crate":1}])).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    // size_ml cannot be cleared while PER_1000L lines exist
    assert_eq!(req(&app, "PUT", &format!("/api/admin/products/{pid}"), Some(&k), Some(json!({"size_ml":null}))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    // clearing the BOM works and is audited
    assert_eq!(put(json!({"lines":[]})).await.1.as_array().unwrap().len(), 0);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='BOM_REPLACE'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 2);
    assert_eq!(req(&app, "GET", &format!("/api/admin/products/{}/bom", uuid::Uuid::new_v4()), Some(&k), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "./migrations")]
async fn customers_routes_vehicles_equipment_schedules(pool: PgPool) {
    let (app, k) = setup(&pool).await;
    // routes + customers
    let (s, r) = post(&app, &k, "/routes", json!({"name":"North"})).await;
    assert_eq!(s, StatusCode::CREATED);
    let rid = r["id"].as_str().unwrap().to_string();
    assert_eq!(post(&app, &k, "/routes", json!({"name":"North"})).await.1["error"]["code"], "DUPLICATE");
    let (s, c) = post(&app, &k, "/customers", json!({"code":"C-1","name":"Shop One","phone":"98765","route_id":rid})).await;
    assert_eq!((s, c["route_name"].as_str()), (StatusCode::CREATED, Some("North")), "{c}");
    assert_eq!(post(&app, &k, "/customers", json!({"code":"C-2","name":"X","route_id":uuid::Uuid::new_v4()})).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(post(&app, &k, "/customers", json!({"code":"C-1","name":"Dup"})).await.1["error"]["code"], "DUPLICATE");
    req(&app, "DELETE", &format!("/api/admin/routes/{rid}"), Some(&k), None).await;
    assert_eq!(post(&app, &k, "/customers", json!({"code":"C-3","name":"X","route_id":rid})).await.0, StatusCode::UNPROCESSABLE_ENTITY, "inactive route refused");
    // vehicles
    let (s, v) = post(&app, &k, "/vehicles", json!({"reg_no":" mh12 ab-1234 ","capacity_crates":300,"puc_expiry":"2020-01-01","insurance_expiry":"2030-05-05"})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["reg_no"].as_str(), v["puc_expiry"].as_str()), (Some("MH12 AB-1234"), Some("2020-01-01")));
    let vid = v["id"].as_str().unwrap().to_string();
    assert_eq!(post(&app, &k, "/vehicles", json!({"reg_no":"mh12 ab-1234","capacity_crates":5})).await.1["error"]["code"], "DUPLICATE");
    for bad in [json!({"reg_no":"A","capacity_crates":0}), json!({"reg_no":"A","capacity_crates":100001}), json!({"reg_no":"A","capacity_crates":-5}), json!({"reg_no":"A<script>","capacity_crates":5}),
                json!({"reg_no":"B1","capacity_crates":5,"puc_expiry":"1900-01-01"}), json!({"reg_no":"B2","capacity_crates":5,"puc_expiry":"2024-13-01"}), json!({"reg_no":"B3"})] {
        let (s, r) = post(&app, &k, "/vehicles", bad.clone()).await;
        assert!(s == StatusCode::UNPROCESSABLE_ENTITY || s == StatusCode::BAD_REQUEST, "{bad}: {r}");
    }
    let (s, u) = req(&app, "PUT", &format!("/api/admin/vehicles/{vid}"), Some(&k), Some(json!({"puc_expiry":null,"capacity_crates":250}))).await;
    assert_eq!((s, u["puc_expiry"].is_null(), u["capacity_crates"].as_i64()), (StatusCode::OK, true, Some(250)));
    // equipment
    let (s, e) = post(&app, &k, "/equipment", json!({"code":"RO-1","name":"RO plant","kind":"RO","location":"Hall A"})).await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    let eid = e["id"].as_str().unwrap().to_string();
    assert_eq!(post(&app, &k, "/equipment", json!({"code":"X","name":"x","kind":"TOASTER"})).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    // schedules
    let (s, sc) = post(&app, &k, "/maintenance-schedules", json!({"subject_type":"EQUIPMENT","equipment_id":eid,"task":"Replace membrane","interval_days":90,"next_due_on":"2026-01-01"})).await;
    assert_eq!((s, sc["subject_name"].as_str()), (StatusCode::CREATED, Some("RO plant")), "{sc}");
    let scid = sc["id"].as_str().unwrap().to_string();
    let (s, sv) = post(&app, &k, "/maintenance-schedules", json!({"subject_type":"VEHICLE","vehicle_id":vid,"task":"Service","next_due_on":"2026-06-01"})).await;
    assert_eq!((s, sv["subject_name"].as_str()), (StatusCode::CREATED, Some("MH12 AB-1234")));
    for bad in [json!({"subject_type":"EQUIPMENT","task":"t","next_due_on":"2026-01-01"}),
                json!({"subject_type":"EQUIPMENT","equipment_id":eid,"vehicle_id":vid,"task":"t","next_due_on":"2026-01-01"}),
                json!({"subject_type":"VEHICLE","equipment_id":eid,"task":"t","next_due_on":"2026-01-01"}),
                json!({"subject_type":"BOAT","equipment_id":eid,"task":"t","next_due_on":"2026-01-01"}),
                json!({"subject_type":"EQUIPMENT","equipment_id":eid,"task":"t"}),
                json!({"subject_type":"EQUIPMENT","equipment_id":eid,"task":"t","next_due_on":"2026-01-01","interval_days":0}),
                json!({"subject_type":"EQUIPMENT","equipment_id":uuid::Uuid::new_v4(),"task":"t","next_due_on":"2026-01-01"}),
                json!({"subject_type":"EQUIPMENT","equipment_id":eid,"task":"","next_due_on":"2026-01-01"})] {
        let (s, r) = post(&app, &k, "/maintenance-schedules", bad.clone()).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {r}");
    }
    // flipping the subject type drops the stale id automatically
    let (s, u) = req(&app, "PUT", &format!("/api/admin/maintenance-schedules/{scid}"), Some(&k), Some(json!({"subject_type":"VEHICLE","vehicle_id":vid}))).await;
    assert_eq!((s, u["equipment_id"].is_null(), u["vehicle_id"].as_str()), (StatusCode::OK, true, Some(vid.as_str())), "{u}");
    assert_eq!(req(&app, "DELETE", &format!("/api/admin/maintenance-schedules/{scid}"), Some(&k), None).await.0, StatusCode::NO_CONTENT);
    // every mutation audited
    let n: i64 = sqlx::query_scalar("SELECT count(DISTINCT action) FROM audit_logs").fetch_one(&pool).await.unwrap();
    assert!(n >= 9, "{n}");
}

#[sqlx::test(migrations = "./migrations")]
async fn hostile_list_parameters_never_500(pool: PgPool) {
    let (app, k) = setup(&pool).await;
    for fam in ["suppliers", "raw-materials", "products", "customers", "routes", "vehicles", "equipment", "maintenance-schedules", "users"] {
        for q in ["q=%27%20OR%201%3D1--", "q=%25%25%25", "q=%5C", "q=%F0%9F%92%A5", "limit=999999999", "limit=-1&offset=-9", "offset=9223372036854775807", "active=maybe", "limit=x"] {
            let (s, b) = req(&app, "GET", &format!("/api/admin/{fam}?{q}"), Some(&k), None).await;
            assert!(s == StatusCode::OK || s == StatusCode::BAD_REQUEST, "{fam}?{q} -> {s} {b}");
        }
        // NUL byte in a query string must not reach Postgres
        let (s, _) = req(&app, "GET", &format!("/api/admin/{fam}?q=a%00b"), Some(&k), None).await;
        assert!(s.is_client_error(), "{fam} NUL -> {s}");
    }
}
