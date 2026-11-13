-- Loading by gross stock (product/batch crates) is the default; QR crate-set labels are an OPTIONAL feature.
ALTER TABLE trip_items ALTER COLUMN qr_set_id DROP NOT NULL;
ALTER TABLE trip_items DROP CONSTRAINT trip_items_trip_id_qr_set_id_key;
CREATE UNIQUE INDEX trip_items_set_uq   ON trip_items (trip_id, qr_set_id) WHERE qr_set_id IS NOT NULL;
CREATE UNIQUE INDEX trip_items_batch_uq ON trip_items (trip_id, batch_id)  WHERE qr_set_id IS NULL;
CREATE INDEX trip_items_batch_idx ON trip_items (batch_id);

INSERT INTO system_settings (key, value, description) VALUES
 ('features.qr_labels',       'false', 'Enable optional QR crate-set labels (stickers). Off = load by product/batch crate counts only'),
 ('plant.utc_offset_minutes', '330',   'Plant local time offset from UTC in minutes (330 = IST). Used for "today" and document numbers');

-- Idempotency for retried POST/PUT/DELETE on weak plant Wi-Fi (Idempotency-Key header)
CREATE TABLE idempotency_keys (
    user_id    UUID NOT NULL,
    key        TEXT NOT NULL,
    method     TEXT NOT NULL,
    path       TEXT NOT NULL,
    status     INT,
    body       BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, key)
);
CREATE INDEX idempotency_created_idx ON idempotency_keys (created_at);
