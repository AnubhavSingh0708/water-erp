-- Development / test helper: a SUPERUSER role (needed by `cargo test`, which creates throw-away databases via #[sqlx::test]).
-- NEVER run this in production.   sudo -u postgres psql -f db/01_dev_database.sql
CREATE ROLE erp_dev LOGIN PASSWORD 'erp_dev' SUPERUSER;
CREATE DATABASE water_erp_dev OWNER erp_dev;
CREATE DATABASE water_erp_test OWNER erp_dev;
