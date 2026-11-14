-- Per-customer deliveries of a settled trip (bottled goods). Settlement may split the delivered crates of a non-returnable
-- product among customers; whatever is not assigned stays "unassigned". Informational only: no stock effect, no prices.
-- (Returnable containers keep their own customer_jars / jar_ledger flow.)
CREATE TABLE trip_customer_deliveries (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    trip_id     UUID NOT NULL REFERENCES loading_trips(id),
    customer_id UUID NOT NULL REFERENCES customers(id),
    product_id  UUID NOT NULL REFERENCES products(id),
    crates      INT  NOT NULL CHECK (crates > 0),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (trip_id, customer_id, product_id)
);
CREATE INDEX trip_customer_deliveries_customer_idx ON trip_customer_deliveries (customer_id, product_id);
CREATE INDEX trip_customer_deliveries_product_idx  ON trip_customer_deliveries (product_id);
CREATE TRIGGER evt_trip_customer_deliveries AFTER INSERT OR UPDATE OR DELETE ON trip_customer_deliveries
    FOR EACH STATEMENT EXECUTE FUNCTION erp_notify('trips');
