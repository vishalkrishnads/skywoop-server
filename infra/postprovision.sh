#!/usr/bin/env bash
# Creates the Container App UAMI as an Entra principal in Postgres and grants DB access.
# Invoked by `azd hooks postprovision` after infra deploy.
set -euo pipefail

: "${AZURE_POSTGRES_FQDN:?AZURE_POSTGRES_FQDN is required}"
: "${AZURE_POSTGRES_ADMIN_USER:?AZURE_POSTGRES_ADMIN_USER is required}"
: "${AZURE_UAMI_NAME:?AZURE_UAMI_NAME is required}"

DB_NAME="${AZURE_POSTGRES_DB:-skywoop}"

if ! command -v az >/dev/null 2>&1; then
  echo "az CLI is required" >&2
  exit 1
fi

# Prefer a local client. Fall back to the Postgres image so the hook works without libpq.
psql_cmd() {
  if command -v psql >/dev/null 2>&1; then
    psql "$@"
  else
    docker run --rm -i -e PGPASSWORD postgres:18 psql "$@"
  fi
}

TOKEN="$(az account get-access-token --resource-type oss-rdbms --query accessToken -o tsv)"
export PGPASSWORD="${TOKEN}"

# libpq treats '#' as a comment unless the value is single-quoted. Guest UPNs contain '#'.
quote_conn_value() {
  local v="$1"
  v="${v//\'/\'\'}"
  printf "'%s'" "$v"
}
USER_Q="$(quote_conn_value "${AZURE_POSTGRES_ADMIN_USER}")"

ADMIN_CONN="host=${AZURE_POSTGRES_FQDN} port=5432 dbname=postgres user=${USER_Q} sslmode=require"
DB_CONN="host=${AZURE_POSTGRES_FQDN} port=5432 dbname=${DB_NAME} user=${USER_Q} sslmode=require"

exists="$(psql_cmd "${ADMIN_CONN}" -tAc "SELECT 1 FROM pg_roles WHERE rolname = '${AZURE_UAMI_NAME}'" || true)"
if [[ "${exists}" != "1" ]]; then
  echo "Creating Entra principal ${AZURE_UAMI_NAME}..."
  psql_cmd "${ADMIN_CONN}" -v ON_ERROR_STOP=1 \
    -c "SELECT * FROM pgaadauth_create_principal('${AZURE_UAMI_NAME}', false, false);"
else
  echo "Entra principal ${AZURE_UAMI_NAME} already exists."
fi

psql_cmd "${ADMIN_CONN}" -v ON_ERROR_STOP=1 \
  -c "GRANT ALL PRIVILEGES ON DATABASE ${DB_NAME} TO \"${AZURE_UAMI_NAME}\";"

psql_cmd "${DB_CONN}" -v ON_ERROR_STOP=1 <<SQL
GRANT ALL ON SCHEMA public TO "${AZURE_UAMI_NAME}";
GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA public TO "${AZURE_UAMI_NAME}";
GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public TO "${AZURE_UAMI_NAME}";
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON TABLES TO "${AZURE_UAMI_NAME}";
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON SEQUENCES TO "${AZURE_UAMI_NAME}";
SQL

echo "Postgres grants for ${AZURE_UAMI_NAME} on ${DB_NAME} are in place."
