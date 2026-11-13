-- Plant-specific product model.
--  * BOTTLED   = single-use PET SKU: bottle blown from a PREFORM (consumes preforms + caps + labels ...).
--  * RETURNABLE = reusable container (20 L jar etc.): NEVER consumes preforms/bottles; it is washed and refilled, tracked as a fleet.
ALTER TABLE products DROP CONSTRAINT products_kind_check;
UPDATE products SET kind = 'RETURNABLE' WHERE kind = 'JAR20L';
ALTER TABLE products ADD CONSTRAINT products_kind_check CHECK (kind IN ('BOTTLED','RETURNABLE'));

ALTER TABLE products
  ADD COLUMN crates_per_pallet   INT CHECK (crates_per_pallet IS NULL OR crates_per_pallet > 0),   -- standard pallet configuration
  ADD COLUMN preform_material_id UUID REFERENCES raw_materials(id),   -- which preform is blown for this SKU (BOTTLED only)
  ADD COLUMN bottle_material_id  UUID REFERENCES raw_materials(id),   -- the EMPTY_BOTTLE material that blowing produces for this SKU
  ADD COLUMN max_cycles          INT CHECK (max_cycles IS NULL OR max_cycles > 0);   -- retire a returnable after N fills (fleet estimate)
ALTER TABLE products ADD CONSTRAINT products_returnable_no_preform
  CHECK (kind = 'BOTTLED' OR (preform_material_id IS NULL AND bottle_material_id IS NULL));

-- Mineral / chemical dosing: PER_1000L means qty is stock-units per 1000 litres of water in the batch
-- (litres = crates x units_per_crate x size_ml / 1000), so one recipe scales across SKU sizes.
ALTER TABLE bom_items ADD COLUMN basis TEXT NOT NULL DEFAULT 'PER_CRATE' CHECK (basis IN ('PER_CRATE','PER_1000L'));

-- Returnable-container fleet lifecycle (aggregate counts; containers are not individually tagged)
CREATE TABLE jar_fleet_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id  UUID NOT NULL REFERENCES products(id),
    event_type  TEXT NOT NULL CHECK (event_type IN ('NEW_PURCHASED','DESTROYED','WASHED')),
    qty         INT NOT NULL CHECK (qty > 0),
    reason_code TEXT CHECK (reason_code IN ('WASH_REJECT','CRACKED','LEAKING','DAMAGED_RETURN','AGE_RETIRED','LOST','OTHER')),
    location    TEXT CHECK (location IN ('PLANT_EMPTY','PLANT_CLEAN','CUSTOMER','VEHICLE')),
    customer_id UUID REFERENCES customers(id),
    trip_id     UUID REFERENCES loading_trips(id),
    supplier_id UUID REFERENCES suppliers(id),
    note        TEXT NOT NULL DEFAULT '',
    created_by  UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((event_type = 'DESTROYED') = (reason_code IS NOT NULL))
);
CREATE INDEX jar_fleet_events_idx ON jar_fleet_events (product_id, event_type, created_at DESC);
CREATE TRIGGER evt_jar_fleet_events AFTER INSERT OR UPDATE OR DELETE ON jar_fleet_events FOR EACH STATEMENT EXECUTE FUNCTION erp_notify('jars');

INSERT INTO system_settings (key, value, description) VALUES
 ('jars.destroy_warn_pct', '3', 'Alert when containers destroyed in the last 30 days exceed this % of containers filled in that period');
