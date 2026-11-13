-- Realtime change notifications. Statement-level triggers publish a *topic name only* (never row data) on channel
-- `erp_events`; NOTIFY is delivered on COMMIT so rolled-back work never produces an event. src/events.rs listens,
-- debounces and fans out to browsers over SSE (/api/events), filtered per user by allowed_pages.
CREATE FUNCTION erp_notify() RETURNS trigger AS $$
BEGIN PERFORM pg_notify('erp_events', TG_ARGV[0]); RETURN NULL; END; $$ LANGUAGE plpgsql;

DO $$
DECLARE r RECORD;
BEGIN
  FOR r IN SELECT * FROM (VALUES
    ('stock_balances','stock'),
    ('raw_materials','materials'), ('purchase_orders','materials'), ('po_lines','materials'), ('grns','materials'),
    ('grn_lines','materials'), ('reorder_recommendations','materials'),
    ('blow_molding_runs','production'), ('water_treatment_logs','production'), ('production_batches','production'),
    ('qc_tests','qc'),
    ('qr_crate_sets','qr'),
    ('loading_trips','trips'), ('trip_items','trips'),
    ('jar_ledger','jars'),
    ('maintenance_schedules','maintenance'), ('maintenance_logs','maintenance'), ('equipment','maintenance'), ('vehicles','maintenance'),
    ('alerts','alerts'), ('alert_reads','alerts'),
    ('suppliers','master'), ('products','master'), ('bom_items','master'), ('customers','master'), ('routes','master'),
    ('users','users'), ('system_settings','settings'), ('audit_logs','admin'), ('push_subscriptions','admin')
  ) AS t(tbl, topic) LOOP
    EXECUTE format('CREATE TRIGGER %I AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION erp_notify(%L)',
                   'evt_' || r.tbl, r.tbl, r.topic);
  END LOOP;
END $$;
-- a QC decision updates production_batches => also tell qc/qr listeners
CREATE TRIGGER evt_batches_qc AFTER UPDATE OF qc_status ON production_batches FOR EACH STATEMENT EXECUTE FUNCTION erp_notify('qc');
