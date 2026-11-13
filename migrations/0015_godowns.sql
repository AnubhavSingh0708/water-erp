-- Multi-godown inventory. One inventory (stock_balances + stock_ledger) keyed by LOCATION; each godown has its own balances per
-- zone/item/lot and its own FEFO queue. Existing rows belong to the MAIN plant location, so everything written before this
-- migration (and code that does not name a location) keeps working unchanged.
CREATE TABLE locations (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code               TEXT NOT NULL UNIQUE,
    name               TEXT NOT NULL,
    kind               TEXT NOT NULL DEFAULT 'GODOWN' CHECK (kind IN ('PLANT','GODOWN','TRANSIT')),
    address            TEXT NOT NULL DEFAULT '',
    city               TEXT NOT NULL DEFAULT '',
    contact_person     TEXT NOT NULL DEFAULT '',
    phone              TEXT NOT NULL DEFAULT '',
    is_production_site BOOLEAN NOT NULL DEFAULT false,   -- production batches / blow runs / QC happen only at production sites
    is_active          BOOLEAN NOT NULL DEFAULT true,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO locations (id, code, name, kind, is_production_site) VALUES
 ('00000000-0000-0000-0000-000000000001', 'MAIN',    'Main Plant',          'PLANT',   true),
 ('00000000-0000-0000-0000-000000000002', 'TRANSIT', 'In transit (system)', 'TRANSIT', false);

ALTER TABLE stock_balances ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES locations(id);
ALTER TABLE stock_balances DROP CONSTRAINT stock_balances_item_type_item_id_zone_lot_id_key;
ALTER TABLE stock_balances ADD CONSTRAINT stock_balances_loc_item_uq UNIQUE (location_id, item_type, item_id, zone, lot_id);
CREATE INDEX stock_balances_loc_idx ON stock_balances (location_id, zone, item_type, item_id);
ALTER TABLE stock_ledger ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001';
CREATE INDEX stock_ledger_loc_idx ON stock_ledger (location_id, at DESC);

-- Documents remember the godown they act on (default MAIN)
ALTER TABLE grns             ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES locations(id);
ALTER TABLE loading_trips    ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES locations(id); -- source godown
ALTER TABLE dispatch_notes   ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES locations(id);
ALTER TABLE stock_write_offs ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES locations(id);
ALTER TABLE stock_counts     ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES locations(id);
ALTER TABLE supplier_returns ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES locations(id);
ALTER TABLE jar_fleet_events ADD COLUMN location_id UUID REFERENCES locations(id);
ALTER TABLE production_batches ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES locations(id); -- production site
ALTER TABLE blow_molding_runs  ADD COLUMN location_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES locations(id);

-- Users may be limited to some godowns ([] = all godowns)
ALTER TABLE users ADD COLUMN allowed_location_ids JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(allowed_location_ids) = 'array');

-- Inter-godown transfers: stock leaves the source immediately into the system TRANSIT location, and is booked into the
-- destination only when the receiving godown confirms (with shortage / damage recorded). Nothing ever "disappears".
CREATE SEQUENCE tf_seq;
CREATE TABLE godown_transfers (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tf_no            TEXT NOT NULL UNIQUE,
    from_location_id UUID NOT NULL REFERENCES locations(id),
    to_location_id   UUID NOT NULL REFERENCES locations(id),
    status           TEXT NOT NULL DEFAULT 'IN_TRANSIT' CHECK (status IN ('IN_TRANSIT','RECEIVED','CANCELLED')),
    vehicle_id       UUID REFERENCES vehicles(id),
    vehicle_no       TEXT NOT NULL DEFAULT '',
    driver_name      TEXT NOT NULL DEFAULT '',
    note             TEXT NOT NULL DEFAULT '',
    created_by       UUID REFERENCES users(id),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    received_by      UUID REFERENCES users(id),
    received_at      TIMESTAMPTZ,
    cancel_reason    TEXT,
    CHECK (from_location_id <> to_location_id)
);
CREATE TABLE godown_transfer_lines (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    transfer_id   UUID NOT NULL REFERENCES godown_transfers(id) ON DELETE CASCADE,
    item_type     TEXT NOT NULL CHECK (item_type IN ('RAW','PRODUCT')),
    item_id       UUID NOT NULL,
    lot_id        UUID NOT NULL,
    zone          TEXT NOT NULL CHECK (zone IN ('RM_STORE','FG_READY','JAR_EMPTY','JAR_CLEAN','BLOW_FLOOR')),  -- never QC_QUARANTINE / SCRAP
    qty_sent      NUMERIC(18,4) NOT NULL CHECK (qty_sent > 0),
    qty_received  NUMERIC(18,4) CHECK (qty_received IS NULL OR qty_received >= 0),
    qty_damaged   NUMERIC(18,4) CHECK (qty_damaged IS NULL OR qty_damaged >= 0),
    note          TEXT NOT NULL DEFAULT ''
);
CREATE INDEX godown_transfers_status_idx ON godown_transfers (status, created_at DESC);

DO $$ DECLARE r RECORD; BEGIN
  FOR r IN SELECT * FROM (VALUES ('locations','master'), ('godown_transfers','stock'), ('godown_transfer_lines','stock')) AS t(tbl, topic) LOOP
    EXECUTE format('CREATE TRIGGER %I AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION erp_notify(%L)', 'evt_'||r.tbl, r.tbl, r.topic);
  END LOOP; END $$;
