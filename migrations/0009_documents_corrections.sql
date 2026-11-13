-- Additional stock documents + correction (void/reverse) support. Principle: documents are never deleted and the ledger is
-- never edited; mistakes are corrected with VOID / reversing ledger rows linked to the original document + a reason.
CREATE SEQUENCE dn_seq; CREATE SEQUENCE wo_seq; CREATE SEQUENCE cnt_seq; CREATE SEQUENCE rtv_seq;

-- GRN void (wrong qty / duplicate entry)
ALTER TABLE grns ADD COLUMN status TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE','VOID'));
ALTER TABLE grns ADD COLUMN void_reason TEXT, ADD COLUMN voided_by UUID REFERENCES users(id), ADD COLUMN voided_at TIMESTAMPTZ;
-- a (supplier, challan) pair can only be entered once while ACTIVE => catches double entry
CREATE UNIQUE INDEX grns_challan_uq ON grns (supplier_id, lower(challan_no)) WHERE challan_no <> '' AND status = 'ACTIVE';

-- Gate-pass void (loaded the wrong goods / vehicle never left)
ALTER TABLE loading_trips ADD COLUMN void_reason TEXT, ADD COLUMN voided_by UUID REFERENCES users(id), ADD COLUMN voided_at TIMESTAMPTZ;

-- Direct dispatch / walk-in sale of finished goods (quantities only – no prices, taxes or payments)
CREATE TABLE dispatch_notes (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dn_no         TEXT NOT NULL UNIQUE,
    customer_id   UUID REFERENCES customers(id),
    party_name    TEXT NOT NULL DEFAULT '',      -- walk-in name when no customer record
    vehicle_no    TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT 'ISSUED' CHECK (status IN ('ISSUED','VOID')),
    note          TEXT NOT NULL DEFAULT '',
    void_reason   TEXT, voided_by UUID REFERENCES users(id), voided_at TIMESTAMPTZ,
    created_by    UUID REFERENCES users(id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (customer_id IS NOT NULL OR party_name <> '')
);
CREATE TABLE dispatch_lines (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dn_id       UUID NOT NULL REFERENCES dispatch_notes(id) ON DELETE CASCADE,
    product_id  UUID NOT NULL REFERENCES products(id),
    batch_id    UUID NOT NULL REFERENCES production_batches(id),
    crates      INT NOT NULL CHECK (crates > 0)
);
CREATE INDEX dispatch_lines_batch_idx ON dispatch_lines (batch_id);

-- Destroyed / written-off stock with a mandatory reason code
CREATE TABLE stock_write_offs (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    wo_no       TEXT NOT NULL UNIQUE,
    item_type   TEXT NOT NULL CHECK (item_type IN ('RAW','PRODUCT')),
    item_id     UUID NOT NULL,
    zone        TEXT NOT NULL,
    lot_id      UUID NOT NULL,
    qty         NUMERIC(18,4) NOT NULL CHECK (qty > 0),
    reason_code TEXT NOT NULL CHECK (reason_code IN ('BREAKAGE','EXPIRED','CONTAMINATED','SPILLAGE','QC_REJECT','THEFT_LOSS','TRANSIT_DAMAGE','OTHER')),
    note        TEXT NOT NULL,
    disposed    BOOLEAN NOT NULL DEFAULT false,   -- true = physically removed from SCRAP_YARD (DISPOSE ledger row)
    created_by  UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Supplier returns (receipt rejected / defective goods sent back)
CREATE TABLE supplier_returns (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rtv_no      TEXT NOT NULL UNIQUE,
    supplier_id UUID NOT NULL REFERENCES suppliers(id),
    grn_id      UUID REFERENCES grns(id),
    note        TEXT NOT NULL DEFAULT '',
    created_by  UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE supplier_return_lines (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    return_id       UUID NOT NULL REFERENCES supplier_returns(id) ON DELETE CASCADE,
    raw_material_id UUID NOT NULL REFERENCES raw_materials(id),
    lot_id          UUID NOT NULL,
    qty_stock       NUMERIC(18,4) NOT NULL CHECK (qty_stock > 0),
    reason          TEXT NOT NULL DEFAULT ''
);

-- Physical stock count (cycle count): snapshot expected -> enter counted -> review variance -> post adjustments
CREATE TABLE stock_counts (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cnt_no      TEXT NOT NULL UNIQUE,
    zone        TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'DRAFT' CHECK (status IN ('DRAFT','POSTED','CANCELLED')),
    note        TEXT NOT NULL DEFAULT '',
    created_by  UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    posted_by   UUID REFERENCES users(id),
    posted_at   TIMESTAMPTZ
);
CREATE TABLE stock_count_lines (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    count_id     UUID NOT NULL REFERENCES stock_counts(id) ON DELETE CASCADE,
    item_type    TEXT NOT NULL,
    item_id      UUID NOT NULL,
    lot_id       UUID NOT NULL,
    expected_qty NUMERIC(18,4) NOT NULL,
    counted_qty  NUMERIC(18,4) CHECK (counted_qty IS NULL OR counted_qty >= 0),
    note         TEXT NOT NULL DEFAULT '',
    UNIQUE (count_id, item_type, item_id, lot_id)
);

INSERT INTO system_settings (key, value, description) VALUES
 ('entry.large_adjust_pct',      '20', 'Adjustments/write-offs larger than this % of the balance need explicit confirm'),
 ('production.scrap_warn_pct',   '5',  'Blow-molding scrap % above which a note is required and a warning is shown'),
 ('loading.void_window_hours',   '12', 'Hours after issue during which a gate pass / dispatch note may be voided (Admin: unlimited)'),
 ('settle.damage_warn_pct',      '10', 'Trip settlement with damaged crates above this % of loaded needs confirm');

-- realtime topics for the new tables
DO $$ DECLARE r RECORD; BEGIN
  FOR r IN SELECT * FROM (VALUES
    ('dispatch_notes','trips'), ('dispatch_lines','trips'), ('stock_write_offs','stock'), ('stock_counts','stock'),
    ('stock_count_lines','stock'), ('supplier_returns','materials'), ('supplier_return_lines','materials')
  ) AS t(tbl, topic) LOOP
    EXECUTE format('CREATE TRIGGER %I AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION erp_notify(%L)', 'evt_'||r.tbl, r.tbl, r.topic);
  END LOOP; END $$;
