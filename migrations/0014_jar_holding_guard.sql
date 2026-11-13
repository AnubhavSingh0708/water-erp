-- Safety net (C2): the jars a customer holds can never be negative, whichever module writes jar_ledger
-- (counter receive, trip settlement, write-offs ...). The check is DEFERRED to COMMIT so that the order of rows inside one
-- transaction does not matter, and every (customer, product) pair is serialised with a transaction-level advisory lock so that two
-- concurrent returns cannot both pass. Violation => SQLSTATE 23514 (mapped to HTTP 400 by AppError).
CREATE FUNCTION jar_ledger_guard_holding() RETURNS trigger AS $$
DECLARE held BIGINT;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended('jar:' || NEW.customer_id::text || ':' || NEW.product_id::text, 0));
    SELECT COALESCE(SUM(delta), 0) INTO held FROM jar_ledger WHERE customer_id = NEW.customer_id AND product_id = NEW.product_id;
    IF held < 0 THEN
        RAISE EXCEPTION 'customer would hold a negative number of containers (%)', held USING ERRCODE = '23514', CONSTRAINT = 'jar_ledger_holding_non_negative';
    END IF;
    RETURN NULL;
END; $$ LANGUAGE plpgsql;

CREATE CONSTRAINT TRIGGER jar_ledger_holding_non_negative AFTER INSERT ON jar_ledger
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION jar_ledger_guard_holding();
