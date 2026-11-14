-- Counter returns of finished goods outside trips ("customer returns"): a customer or walk-in party brings crates back.
-- Intact crates of an approved, unexpired batch go back into FG_READY of the godown (same batch lot), damaged crates go to
-- SCRAP_YARD. Documents are never deleted; a mistake is corrected with a VOID (reversing ledger rows). Quantities only.
CREATE SEQUENCE cr_seq;

CREATE TABLE customer_returns (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cr_no       TEXT NOT NULL UNIQUE,
    location_id UUID NOT NULL REFERENCES locations(id),
    customer_id UUID REFERENCES customers(id),
    party_name  TEXT NOT NULL DEFAULT '',          -- walk-in name when there is no customer record
    note        TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'ISSUED' CHECK (status IN ('ISSUED','VOID')),
    void_reason TEXT, voided_by UUID REFERENCES users(id), voided_at TIMESTAMPTZ,
    created_by  UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (customer_id IS NOT NULL OR party_name <> ''),
    CHECK ((status = 'VOID') = (void_reason IS NOT NULL))
);
CREATE INDEX customer_returns_loc_idx      ON customer_returns (location_id, created_at DESC);
CREATE INDEX customer_returns_customer_idx ON customer_returns (customer_id, created_at DESC);

CREATE TABLE customer_return_lines (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cr_id          UUID NOT NULL REFERENCES customer_returns(id) ON DELETE CASCADE,
    product_id     UUID NOT NULL REFERENCES products(id),
    batch_id       UUID NOT NULL REFERENCES production_batches(id),
    crates_intact  INT NOT NULL DEFAULT 0 CHECK (crates_intact >= 0),
    crates_damaged INT NOT NULL DEFAULT 0 CHECK (crates_damaged >= 0),
    reason         TEXT NOT NULL,
    CHECK (crates_intact + crates_damaged > 0)
);
CREATE INDEX customer_return_lines_cr_idx    ON customer_return_lines (cr_id);
CREATE INDEX customer_return_lines_batch_idx ON customer_return_lines (batch_id);

CREATE TRIGGER evt_customer_returns      AFTER INSERT OR UPDATE OR DELETE ON customer_returns      FOR EACH STATEMENT EXECUTE FUNCTION erp_notify('trips');
CREATE TRIGGER evt_customer_return_lines AFTER INSERT OR UPDATE OR DELETE ON customer_return_lines FOR EACH STATEMENT EXECUTE FUNCTION erp_notify('trips');
