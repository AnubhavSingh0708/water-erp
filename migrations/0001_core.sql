-- Core: settings, users, sessions, audit, alerts, push
CREATE TABLE system_settings (
    key         TEXT PRIMARY KEY,
    value       JSONB NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by  UUID
);

INSERT INTO system_settings (key, value, description) VALUES
 ('plant.name',                         '"Aqua Pure Plant"',  'Plant display name'),
 ('plant.code',                         '"PLT1"',             'Short plant code used in batch/trip numbers'),
 ('plant.qr_base_url',                  '""',                 'Override base URL embedded in QR labels (blank = auto from --domain/bind)'),
 ('auth.session_ttl_hours',             '12',                 'Sliding session lifetime (hours since last use)'),
 ('auth.session_inactivity_purge_days', '7',                  'Delete session keys unused for this many days'),
 ('auth.max_failed_logins',             '5',                  'Failed logins before temporary lockout'),
 ('auth.lockout_minutes',               '15',                 'Lockout duration after too many failed logins'),
 ('inventory.burn_rate_lookback_days',  '14',                 'Rolling window for consumption burn-rate'),
 ('inventory.safety_stock_days',        '3',                  'Safety buffer days added to supplier lead time'),
 ('inventory.worker_interval_minutes',  '15',                 'How often the low-stock engine runs'),
 ('jars.overdue_days',                  '30',                 '20L jar considered overdue at customer after N days'),
 ('alerts.expiry_lead_days',            '30',                 'Maintenance / vehicle document alert lead time (days)'),
 ('loading.default_crate_set_size',     '10',                 'Default crates per QR crate-set'),
 ('loading.strict_qc_lock',             'true',               'Hard-block loading of any non-APPROVED batch'),
 ('lab.ph_min',                         '6.5',                'Water treatment pH lower limit'),
 ('lab.ph_max',                         '8.5',                'Water treatment pH upper limit'),
 ('lab.tds_min',                        '50',                 'Water treatment TDS lower limit (ppm)'),
 ('lab.tds_max',                        '500',                'Water treatment TDS upper limit (ppm)');

CREATE TABLE users (
    id                       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    username                 TEXT NOT NULL,
    full_name                TEXT NOT NULL DEFAULT '',
    password_hash            TEXT NOT NULL,
    is_admin                 BOOLEAN NOT NULL DEFAULT false,
    is_active                BOOLEAN NOT NULL DEFAULT true,
    allowed_pages            JSONB NOT NULL DEFAULT '[]'::jsonb,
    subscribed_notifications JSONB NOT NULL DEFAULT '[]'::jsonb,
    failed_login_count       INT NOT NULL DEFAULT 0,
    locked_until             TIMESTAMPTZ,
    last_login_at            TIMESTAMPTZ,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT users_pages_array CHECK (jsonb_typeof(allowed_pages) = 'array'),
    CONSTRAINT users_subs_array  CHECK (jsonb_typeof(subscribed_notifications) = 'array')
);
CREATE UNIQUE INDEX users_username_lower_uq ON users (lower(username));

CREATE TABLE user_sessions (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    key_hash     TEXT NOT NULL UNIQUE,            -- hex(SHA-256(raw key)); the raw key is never stored
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    user_agent   TEXT,
    ip           TEXT
);
CREATE INDEX user_sessions_user_idx ON user_sessions (user_id);
CREATE INDEX user_sessions_last_used_idx ON user_sessions (last_used_at);

CREATE TABLE audit_logs (
    id         BIGSERIAL PRIMARY KEY,
    at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    user_id    UUID,
    username   TEXT,
    action     TEXT NOT NULL,
    entity     TEXT NOT NULL DEFAULT '',
    entity_id  TEXT,
    details    JSONB NOT NULL DEFAULT '{}'::jsonb,
    ip         TEXT
);
CREATE INDEX audit_logs_at_idx ON audit_logs (at DESC);
CREATE INDEX audit_logs_entity_idx ON audit_logs (entity, entity_id);

CREATE FUNCTION forbid_mutation() RETURNS trigger AS $$
BEGIN RAISE EXCEPTION '% is append-only', TG_TABLE_NAME; END; $$ LANGUAGE plpgsql;
CREATE TRIGGER audit_logs_immutable BEFORE UPDATE OR DELETE ON audit_logs
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- In-app alerts; also the outbox for Web Push dispatch
CREATE TABLE alerts (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_type    TEXT NOT NULL,
    dedupe_key    TEXT NOT NULL,
    severity      TEXT NOT NULL DEFAULT 'INFO' CHECK (severity IN ('INFO','WARNING','CRITICAL')),
    title         TEXT NOT NULL,
    body          TEXT NOT NULL DEFAULT '',
    url           TEXT NOT NULL DEFAULT '/dashboard.html',
    payload       JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at   TIMESTAMPTZ,
    dispatched_at TIMESTAMPTZ
);
-- an unresolved alert with the same (event_type, dedupe_key) is never raised twice
CREATE UNIQUE INDEX alerts_open_dedupe_uq ON alerts (event_type, dedupe_key) WHERE resolved_at IS NULL;
CREATE INDEX alerts_pending_dispatch_idx ON alerts (created_at) WHERE dispatched_at IS NULL;
CREATE INDEX alerts_created_idx ON alerts (created_at DESC);

CREATE TABLE alert_reads (
    alert_id UUID NOT NULL REFERENCES alerts(id) ON DELETE CASCADE,
    user_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    read_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (alert_id, user_id)
);

CREATE TABLE push_subscriptions (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    endpoint        TEXT NOT NULL UNIQUE,
    p256dh          TEXT NOT NULL,
    auth            TEXT NOT NULL,
    user_agent      TEXT,
    active          BOOLEAN NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_success_at TIMESTAMPTZ,
    last_error      TEXT,
    deactivated_at  TIMESTAMPTZ
);
CREATE INDEX push_subscriptions_user_idx ON push_subscriptions (user_id) WHERE active;

CREATE TABLE push_deliveries (
    alert_id  UUID NOT NULL REFERENCES alerts(id) ON DELETE CASCADE,
    user_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    sent_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (alert_id, user_id)
);
