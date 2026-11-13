-- One-time PostgreSQL provisioning. Run as a superuser, e.g.:
--   sudo -u postgres psql -v erp_password="'CHANGE-ME-LONG-RANDOM'" -f db/00_create_role_and_database.sql
-- (the quotes inside the -v value are intentional: they make it a SQL string literal.)
-- Application tables are created by the migrations in ./migrations, which the server applies automatically on start
-- (embedded in the binary). The role below deliberately is NOT a superuser.
\set ON_ERROR_STOP on

CREATE ROLE erp LOGIN PASSWORD :erp_password NOSUPERUSER NOCREATEDB NOCREATEROLE;
CREATE DATABASE water_erp OWNER erp ENCODING 'UTF8' TEMPLATE template0;

\connect water_erp
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT ALL ON SCHEMA public TO erp;
-- gen_random_uuid() is built in since PostgreSQL 13; no extensions are required.
