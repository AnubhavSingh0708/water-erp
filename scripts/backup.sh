#!/usr/bin/env bash
# Nightly logical backup. Usage: DATABASE_URL=postgres://... scripts/backup.sh /var/backups/water-erp
# Keeps 14 days. Restore: createdb water_erp_restore && pg_restore -d water_erp_restore <file>.dump
set -euo pipefail
dir="${1:-/var/backups/water-erp}"; mkdir -p "$dir"
f="$dir/water_erp_$(date +%Y%m%d_%H%M%S).dump"
pg_dump --format=custom --no-owner "$DATABASE_URL" -f "$f"
chmod 600 "$f"
find "$dir" -name 'water_erp_*.dump' -mtime +14 -delete
echo "backup written: $f"
