-- Master data (Admin-managed)
CREATE TABLE suppliers (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name           TEXT NOT NULL,
    contact_person TEXT NOT NULL DEFAULT '',
    phone          TEXT NOT NULL DEFAULT '',
    email          TEXT NOT NULL DEFAULT '',
    address        TEXT NOT NULL DEFAULT '',
    lead_time_days INT NOT NULL DEFAULT 7 CHECK (lead_time_days >= 0),
    is_active      BOOLEAN NOT NULL DEFAULT true,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE raw_materials (
    id                     UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code                   TEXT NOT NULL UNIQUE,
    name                   TEXT NOT NULL,
    category               TEXT NOT NULL CHECK (category IN
        ('PREFORM','CAP','LABEL','SLEEVE','SHRINK_FILM','CARTON','MINERAL','CHEMICAL','EMPTY_BOTTLE','OTHER')),
    stock_unit             TEXT NOT NULL DEFAULT 'pcs',   -- unit all stock quantities are held in (pcs, kg, ...)
    purchase_unit          TEXT NOT NULL DEFAULT 'pcs',   -- unit on POs / GRNs (bag, kg, roll, carton ...)
    stock_per_purchase_unit NUMERIC(18,4) NOT NULL DEFAULT 1 CHECK (stock_per_purchase_unit > 0), -- e.g. 1 bag = 2000 pcs
    unit_weight_g          NUMERIC(12,4),                 -- grams per piece (preform weight)
    min_stock              NUMERIC(18,4) NOT NULL DEFAULT 0 CHECK (min_stock >= 0), -- static threshold, in stock_unit
    default_supplier_id    UUID REFERENCES suppliers(id),
    is_active              BOOLEAN NOT NULL DEFAULT true,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE products (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code             TEXT NOT NULL UNIQUE,
    name             TEXT NOT NULL,
    kind             TEXT NOT NULL DEFAULT 'BOTTLED' CHECK (kind IN ('BOTTLED','JAR20L')),
    size_ml          INT,
    units_per_crate  INT NOT NULL DEFAULT 12 CHECK (units_per_crate > 0),  -- bottles per crate/case (1 for 20L jar)
    shelf_life_days  INT NOT NULL DEFAULT 180 CHECK (shelf_life_days > 0),
    min_stock_crates NUMERIC(18,4) NOT NULL DEFAULT 0,
    is_active        BOOLEAN NOT NULL DEFAULT true,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Bill of materials: stock-unit quantity of a raw material per ONE crate of product.
CREATE TABLE bom_items (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id      UUID NOT NULL REFERENCES products(id) ON DELETE CASCADE,
    raw_material_id UUID NOT NULL REFERENCES raw_materials(id),
    qty_per_crate   NUMERIC(18,6) NOT NULL CHECK (qty_per_crate > 0),
    source_zone     TEXT NOT NULL DEFAULT 'RM_STORE' CHECK (source_zone IN ('RM_STORE','BLOW_FLOOR')),
    UNIQUE (product_id, raw_material_id)
);

CREATE TABLE routes (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    is_active   BOOLEAN NOT NULL DEFAULT true
);

CREATE TABLE customers (
    id        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code      TEXT NOT NULL UNIQUE,
    name      TEXT NOT NULL,
    phone     TEXT NOT NULL DEFAULT '',
    address   TEXT NOT NULL DEFAULT '',
    route_id  UUID REFERENCES routes(id),
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE vehicles (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    reg_no           TEXT NOT NULL UNIQUE,
    name             TEXT NOT NULL DEFAULT '',
    capacity_crates  INT NOT NULL CHECK (capacity_crates > 0),
    driver_name      TEXT NOT NULL DEFAULT '',
    driver_phone     TEXT NOT NULL DEFAULT '',
    insurance_expiry DATE,
    fitness_expiry   DATE,
    puc_expiry       DATE,
    permit_expiry    DATE,
    is_active        BOOLEAN NOT NULL DEFAULT true,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE equipment (
    id        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code      TEXT NOT NULL UNIQUE,
    name      TEXT NOT NULL,
    kind      TEXT NOT NULL CHECK (kind IN ('RO','UV','OZONATOR','BLOW_MOLDER','FILTER','COMPRESSOR','FILLER','OTHER')),
    location  TEXT NOT NULL DEFAULT '',
    is_active BOOLEAN NOT NULL DEFAULT true
);

-- Recurring service tasks AND dated expiries (insurance, fitness, PUC ...) for equipment or vehicles
CREATE TABLE maintenance_schedules (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_type TEXT NOT NULL CHECK (subject_type IN ('EQUIPMENT','VEHICLE')),
    equipment_id UUID REFERENCES equipment(id) ON DELETE CASCADE,
    vehicle_id   UUID REFERENCES vehicles(id) ON DELETE CASCADE,
    task         TEXT NOT NULL,
    interval_days INT CHECK (interval_days IS NULL OR interval_days > 0),
    last_done_on DATE,
    next_due_on  DATE NOT NULL,
    is_active    BOOLEAN NOT NULL DEFAULT true,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((subject_type = 'EQUIPMENT' AND equipment_id IS NOT NULL AND vehicle_id IS NULL)
        OR (subject_type = 'VEHICLE'   AND vehicle_id IS NOT NULL AND equipment_id IS NULL))
);

CREATE TABLE maintenance_logs (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    schedule_id UUID NOT NULL REFERENCES maintenance_schedules(id) ON DELETE CASCADE,
    done_on     DATE NOT NULL,
    done_by     UUID REFERENCES users(id),
    notes       TEXT NOT NULL DEFAULT '',
    next_due_on DATE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
