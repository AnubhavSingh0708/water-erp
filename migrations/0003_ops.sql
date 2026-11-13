-- Document number sequences
CREATE SEQUENCE po_seq; CREATE SEQUENCE grn_seq; CREATE SEQUENCE blow_seq;
CREATE SEQUENCE batch_seq; CREATE SEQUENCE trip_seq; CREATE SEQUENCE gatepass_seq; CREATE SEQUENCE qr_seq;

-- ===== Procurement =====
CREATE TABLE purchase_orders (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    po_no       TEXT NOT NULL UNIQUE,
    supplier_id UUID NOT NULL REFERENCES suppliers(id),
    status      TEXT NOT NULL DEFAULT 'OPEN' CHECK (status IN ('OPEN','PARTIAL','RECEIVED','CANCELLED')),
    ordered_on  DATE NOT NULL DEFAULT CURRENT_DATE,
    expected_on DATE,
    notes       TEXT NOT NULL DEFAULT '',
    created_by  UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE po_lines (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    po_id           UUID NOT NULL REFERENCES purchase_orders(id) ON DELETE CASCADE,
    raw_material_id UUID NOT NULL REFERENCES raw_materials(id),
    qty_ordered     NUMERIC(18,4) NOT NULL CHECK (qty_ordered > 0),   -- purchase units
    qty_received    NUMERIC(18,4) NOT NULL DEFAULT 0                  -- purchase units
);
CREATE TABLE grns (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    grn_no      TEXT NOT NULL UNIQUE,
    po_id       UUID REFERENCES purchase_orders(id),
    supplier_id UUID NOT NULL REFERENCES suppliers(id),
    received_on DATE NOT NULL DEFAULT CURRENT_DATE,
    challan_no  TEXT NOT NULL DEFAULT '',
    vehicle_no  TEXT NOT NULL DEFAULT '',
    notes       TEXT NOT NULL DEFAULT '',
    received_by UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE grn_lines (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- also the stock lot_id
    grn_id          UUID NOT NULL REFERENCES grns(id) ON DELETE CASCADE,
    raw_material_id UUID NOT NULL REFERENCES raw_materials(id),
    po_line_id      UUID REFERENCES po_lines(id),
    qty_purchase    NUMERIC(18,4) NOT NULL CHECK (qty_purchase > 0),  -- purchase units
    qty_stock       NUMERIC(18,4) NOT NULL CHECK (qty_stock > 0),     -- converted to stock units
    lot_no          TEXT NOT NULL DEFAULT '',
    expiry_date     DATE
);
CREATE TABLE reorder_recommendations (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    raw_material_id  UUID NOT NULL REFERENCES raw_materials(id) ON DELETE CASCADE,
    status           TEXT NOT NULL DEFAULT 'OPEN' CHECK (status IN ('OPEN','ORDERED','DISMISSED','RESOLVED')),
    reason           TEXT NOT NULL CHECK (reason IN ('BELOW_MIN','BURN_RATE','BOTH')),
    current_stock    NUMERIC(18,4) NOT NULL,           -- stock units
    min_stock        NUMERIC(18,4) NOT NULL,
    avg_daily_burn   NUMERIC(18,4) NOT NULL,
    days_cover       NUMERIC(18,2),                    -- null when burn = 0
    lead_time_days   INT NOT NULL,
    safety_days      INT NOT NULL,
    reorder_point    NUMERIC(18,4) NOT NULL,
    suggested_qty_purchase NUMERIC(18,4) NOT NULL,     -- purchase units (rounded up)
    suggested_supplier_id UUID REFERENCES suppliers(id),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX reorder_open_uq ON reorder_recommendations (raw_material_id) WHERE status = 'OPEN';

-- ===== Ledger-based zone warehouse =====
CREATE TABLE stock_balances (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    item_type   TEXT NOT NULL CHECK (item_type IN ('RAW','PRODUCT')),
    item_id     UUID NOT NULL,
    zone        TEXT NOT NULL CHECK (zone IN ('RM_STORE','BLOW_FLOOR','QC_QUARANTINE','FG_READY','SCRAP_YARD','JAR_EMPTY','JAR_CLEAN')),
    lot_id      UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000', -- grn_lines.id (RAW) / production_batches.id (PRODUCT) / nil
    qty         NUMERIC(18,4) NOT NULL DEFAULT 0 CHECK (qty >= 0),
    expiry_date DATE,                       -- FEFO key
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (item_type, item_id, zone, lot_id)
);
CREATE INDEX stock_balances_item_idx ON stock_balances (item_type, item_id, zone);

CREATE TABLE stock_ledger (
    id            BIGSERIAL PRIMARY KEY,
    at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    txn_group     UUID NOT NULL,
    item_type     TEXT NOT NULL,
    item_id       UUID NOT NULL,
    zone          TEXT NOT NULL,
    lot_id        UUID NOT NULL,
    qty_delta     NUMERIC(18,4) NOT NULL CHECK (qty_delta <> 0),
    balance_after NUMERIC(18,4) NOT NULL CHECK (balance_after >= 0),
    txn_type      TEXT NOT NULL,   -- GRN, BLOW_ISSUE, BLOW_OUTPUT, BLOW_SCRAP, BATCH_CONSUME, BATCH_OUTPUT, QC_RELEASE, QC_REJECT, GATE_PASS, TRIP_RETURN, TRIP_DAMAGED, JAR_WASH, JAR_RECEIVE, JAR_WRITE_OFF, ADJUST, TRANSFER
    ref_type      TEXT,
    ref_id        UUID,
    note          TEXT NOT NULL DEFAULT '',
    user_id       UUID
);
CREATE INDEX stock_ledger_item_idx ON stock_ledger (item_type, item_id, at DESC);
CREATE INDEX stock_ledger_ref_idx ON stock_ledger (ref_type, ref_id);
CREATE INDEX stock_ledger_type_at_idx ON stock_ledger (txn_type, at DESC);
CREATE TRIGGER stock_ledger_immutable BEFORE UPDATE OR DELETE ON stock_ledger
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- ===== Production =====
CREATE TABLE blow_molding_runs (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_no              TEXT NOT NULL UNIQUE,
    run_date            DATE NOT NULL DEFAULT CURRENT_DATE,
    shift               TEXT NOT NULL DEFAULT 'DAY' CHECK (shift IN ('DAY','NIGHT','GENERAL')),
    equipment_id        UUID REFERENCES equipment(id),
    preform_material_id UUID NOT NULL REFERENCES raw_materials(id),
    bottle_material_id  UUID NOT NULL REFERENCES raw_materials(id),
    preforms_issued     NUMERIC(18,4) NOT NULL CHECK (preforms_issued > 0),
    good_bottles        NUMERIC(18,4) NOT NULL CHECK (good_bottles >= 0),
    scrap_pcs           NUMERIC(18,4) NOT NULL CHECK (scrap_pcs >= 0),
    scrap_pct           NUMERIC(8,3) NOT NULL,
    scrap_weight_kg     NUMERIC(14,4),
    notes               TEXT NOT NULL DEFAULT '',
    created_by          UUID REFERENCES users(id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (good_bottles + scrap_pcs = preforms_issued)
);

CREATE TABLE water_treatment_logs (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    logged_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    tds_ppm      NUMERIC(10,2) NOT NULL,
    ph           NUMERIC(5,2)  NOT NULL,
    ozone_ppm    NUMERIC(8,3),
    uv_ok        BOOLEAN NOT NULL DEFAULT true,
    minerals     JSONB NOT NULL DEFAULT '{}'::jsonb,   -- { "Calcium chloride": 1.2, ... }  (ppm or grams dosed)
    out_of_range BOOLEAN NOT NULL DEFAULT false,
    notes        TEXT NOT NULL DEFAULT '',
    logged_by    UUID REFERENCES users(id)
);
CREATE INDEX water_logs_at_idx ON water_treatment_logs (logged_at DESC);

CREATE TABLE production_batches (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    batch_no       TEXT NOT NULL UNIQUE,
    product_id     UUID NOT NULL REFERENCES products(id),
    produced_on    DATE NOT NULL DEFAULT CURRENT_DATE,
    shift          TEXT NOT NULL DEFAULT 'DAY' CHECK (shift IN ('DAY','NIGHT','GENERAL')),
    crates         INT NOT NULL CHECK (crates > 0),     -- crates (or jars for JAR20L) produced
    qc_status      TEXT NOT NULL DEFAULT 'QUARANTINE' CHECK (qc_status IN ('QUARANTINE','APPROVED','REJECTED')),
    mfg_date       DATE NOT NULL DEFAULT CURRENT_DATE,
    expiry_date    DATE NOT NULL,
    water_log_id   UUID REFERENCES water_treatment_logs(id),
    notes          TEXT NOT NULL DEFAULT '',
    qc_decided_by  UUID REFERENCES users(id),
    qc_decided_at  TIMESTAMPTZ,
    qc_notes       TEXT NOT NULL DEFAULT '',
    created_by     UUID REFERENCES users(id),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX production_batches_status_idx ON production_batches (qc_status, created_at DESC);

CREATE TABLE batch_material_usage (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    batch_id        UUID NOT NULL REFERENCES production_batches(id) ON DELETE CASCADE,
    raw_material_id UUID NOT NULL REFERENCES raw_materials(id),
    qty             NUMERIC(18,4) NOT NULL,       -- stock units
    zone            TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX batch_usage_material_idx ON batch_material_usage (raw_material_id, created_at);

CREATE TABLE qc_tests (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    batch_id        UUID NOT NULL REFERENCES production_batches(id) ON DELETE CASCADE,
    tested_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    tested_by       UUID REFERENCES users(id),
    tds_ppm         NUMERIC(10,2),
    ph              NUMERIC(5,2),
    ozone_ppm       NUMERIC(8,3),
    turbidity_ntu   NUMERIC(8,3),
    microbial_ok    BOOLEAN,
    seal_ok         BOOLEAN,
    label_ok        BOOLEAN,
    result          TEXT NOT NULL CHECK (result IN ('PASS','FAIL')),
    remarks         TEXT NOT NULL DEFAULT ''
);

-- ===== QR crate sets, loading trips =====
CREATE TABLE loading_trips (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    trip_no            TEXT NOT NULL UNIQUE,
    vehicle_id         UUID NOT NULL REFERENCES vehicles(id),
    route_id           UUID REFERENCES routes(id),
    driver_name        TEXT NOT NULL DEFAULT '',
    capacity_crates    INT NOT NULL,                  -- snapshot of vehicle capacity
    status             TEXT NOT NULL DEFAULT 'LOADING' CHECK (status IN ('LOADING','DISPATCHED','SETTLED','CANCELLED')),
    gate_pass_no       TEXT UNIQUE,
    gate_pass_issued_at TIMESTAMPTZ,
    issued_by          UUID REFERENCES users(id),
    returned_at        TIMESTAMPTZ,
    settled_at         TIMESTAMPTZ,
    settled_by         UUID REFERENCES users(id),
    notes              TEXT NOT NULL DEFAULT '',
    created_by         UUID REFERENCES users(id),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX loading_trips_status_idx ON loading_trips (status, created_at DESC);

CREATE TABLE qr_crate_sets (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code            TEXT NOT NULL UNIQUE,            -- value placed in the QR URL (?qr=CODE)
    batch_id        UUID NOT NULL REFERENCES production_batches(id),
    product_id      UUID NOT NULL REFERENCES products(id),
    set_label       TEXT NOT NULL,                   -- 'X','Y','Z',... or 'X-R1' for residual children
    seq             INT NOT NULL,
    parent_set_id   UUID REFERENCES qr_crate_sets(id),
    original_crates INT NOT NULL CHECK (original_crates > 0),
    crates          INT NOT NULL CHECK (crates >= 0),   -- current crates this label represents
    status          TEXT NOT NULL DEFAULT 'QUARANTINE' CHECK (status IN
                    ('QUARANTINE','AVAILABLE','RESERVED','LOADED','DELIVERED','REJECTED')),
    trip_id         UUID REFERENCES loading_trips(id),
    loaded_at       TIMESTAMPTZ,
    created_by      UUID REFERENCES users(id),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX qr_sets_batch_idx ON qr_crate_sets (batch_id, seq);
CREATE INDEX qr_sets_status_idx ON qr_crate_sets (status);
CREATE INDEX qr_sets_trip_idx ON qr_crate_sets (trip_id);

CREATE TABLE trip_items (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    trip_id          UUID NOT NULL REFERENCES loading_trips(id) ON DELETE CASCADE,
    qr_set_id        UUID NOT NULL REFERENCES qr_crate_sets(id),
    batch_id         UUID NOT NULL REFERENCES production_batches(id),
    product_id       UUID NOT NULL REFERENCES products(id),
    crates           INT NOT NULL CHECK (crates > 0),
    crates_delivered INT,
    crates_returned  INT,    -- unsold, restocked
    crates_damaged   INT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (trip_id, qr_set_id)
);

-- ===== 20L returnable jars (customer side). Plant-side jars live in stock_balances JAR_EMPTY / JAR_CLEAN =====
CREATE TABLE jar_ledger (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id UUID NOT NULL REFERENCES customers(id),
    product_id  UUID NOT NULL REFERENCES products(id),
    txn_type    TEXT NOT NULL CHECK (txn_type IN ('OPENING','ISSUE_FILLED','RETURN_EMPTY','DAMAGED','LOST','WRITE_OFF')),
    delta       INT NOT NULL CHECK (delta <> 0),   -- change in jars HELD BY CUSTOMER (+ issue/opening, - return/damaged/lost/write-off)
    txn_date    DATE NOT NULL DEFAULT CURRENT_DATE,
    trip_id     UUID REFERENCES loading_trips(id),
    note        TEXT NOT NULL DEFAULT '',
    created_by  UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX jar_ledger_customer_idx ON jar_ledger (customer_id, product_id, txn_date);
