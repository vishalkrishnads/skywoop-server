#!/usr/bin/env bash
# Drop and recreate the preview database schema so a squashed baseline can apply.
#
# The Container App applies sqlx migrations on startup. This script only clears
# the public schema (and grants it back to the app identity). It does not print
# tokens or connection strings.
#
# Usage (from repo root):
#   ./scripts/reset-preview-db.sh
#
# Optional overrides match scripts/deploy-preview.sh:
#   RESOURCE_GROUP, POSTGRES_SERVER, POSTGRES_DB, POSTGRES_ADMIN, DB_ROLE

set -euo pipefail

RESOURCE_GROUP="${RESOURCE_GROUP:-rg-skywoop-prod}"
POSTGRES_SERVER="${POSTGRES_SERVER:-pg-gprevm6gg7qpkaznja}"
POSTGRES_DB="${POSTGRES_DB:-skywoop}"
POSTGRES_ADMIN="${POSTGRES_ADMIN:-admin_onlyhours.app#EXT#@adminonlyhours.onmicrosoft.com}"
DB_ROLE="${DB_ROLE:-id-gprevm6gg7qpkaznja}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: $1 is required" >&2
    exit 1
  }
}

need az
need docker
need curl

FQDN="$(az postgres flexible-server show -g "${RESOURCE_GROUP}" -n "${POSTGRES_SERVER}" --query fullyQualifiedDomainName -o tsv)"
MY_IP="$(curl -fsS -m 15 https://api.ipify.org)"
RULE="allow-deploy-$(date +%s)"

echo "==> Allow ${MY_IP} on ${POSTGRES_SERVER}"
az postgres flexible-server firewall-rule create \
  -g "${RESOURCE_GROUP}" \
  -n "${POSTGRES_SERVER}" \
  -r "${RULE}" \
  --start-ip-address "${MY_IP}" \
  --end-ip-address "${MY_IP}" \
  --output none

cleanup() {
  az postgres flexible-server firewall-rule delete \
    -g "${RESOURCE_GROUP}" \
    -n "${POSTGRES_SERVER}" \
    -r "${RULE}" \
    --yes \
    --output none >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "==> Allow pg_trgm on ${POSTGRES_SERVER}"
az postgres flexible-server parameter set \
  -g "${RESOURCE_GROUP}" \
  --server-name "${POSTGRES_SERVER}" \
  --name azure.extensions \
  --value PG_TRGM \
  --output none

echo "==> Reset schema public on ${POSTGRES_DB} (role ${DB_ROLE})"
TOKEN="$(az account get-access-token --resource-type oss-rdbms --query accessToken -o tsv)"

# -i is required so the SQL heredoc reaches psql. Without it Docker drops stdin
# and psql exits 0 without changing anything.
docker run --rm -i \
  -e PGPASSWORD="${TOKEN}" \
  -e PGHOST="${FQDN}" \
  -e PGPORT=5432 \
  -e PGUSER="${POSTGRES_ADMIN}" \
  -e PGDATABASE="${POSTGRES_DB}" \
  -e PGSSLMODE=require \
  postgres:18 \
  psql -v ON_ERROR_STOP=1 <<SQL
SELECT pg_terminate_backend(pid)
  FROM pg_stat_activity
 WHERE datname = current_database()
   AND pid <> pg_backend_pid();
DROP SCHEMA IF EXISTS public CASCADE;
CREATE SCHEMA public;
GRANT ALL ON SCHEMA public TO "${DB_ROLE}";
GRANT ALL ON SCHEMA public TO public;
ALTER SCHEMA public OWNER TO "${DB_ROLE}";
CREATE EXTENSION IF NOT EXISTS pg_trgm;
SELECT to_regclass('public._sqlx_migrations') AS migrations_table;
SQL

unset TOKEN
echo "==> Schema reset. The next app start applies migrations/0001_video_baseline.sql."
