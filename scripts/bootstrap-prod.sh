#!/usr/bin/env bash
# One-time production bootstrap for api.skywoop.com.
#
# Prerequisites: azd, az, cf (Cloudflare CLI), psql; Azure + Cloudflare logged in.
#
# Usage (from repo root):
#   ./scripts/bootstrap-prod.sh
#
# Optional env:
#   AZURE_SUBSCRIPTION_ID=<guid>   # defaults to `az account show` (same as grower when that is the default)
#   AZURE_LOCATION=centralus   # eastus blocks Postgres on this subscription
#   APP_ORIGIN=https://skywoop.com
#   AUTH_JWKS_URL=https://skywoop.com/api/auth/jwks
#   AUTH_ISSUER=https://skywoop.com
#   AUTH_AUDIENCE=skywoop-api

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"

AZURE_LOCATION="${AZURE_LOCATION:-centralus}"
APP_ORIGIN="${APP_ORIGIN:-https://skywoop.com}"
AUTH_JWKS_URL="${AUTH_JWKS_URL:-${APP_ORIGIN}/api/auth/jwks}"
AUTH_ISSUER="${AUTH_ISSUER:-${APP_ORIGIN}}"
AUTH_AUDIENCE="${AUTH_AUDIENCE:-skywoop-api}"
PUBLIC_BASE_URL="${PUBLIC_BASE_URL:-https://api.skywoop.com}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: $1 is required" >&2
    exit 1
  }
}

need azd
need az
need cf
need curl
need jq

echo "==> Resolve signed-in Entra user (Postgres admin)"
USER_JSON="$(az ad signed-in-user show -o json)"
POSTGRES_ADMIN_OBJECT_ID="$(jq -r '.id' <<<"${USER_JSON}")"
POSTGRES_ADMIN_PRINCIPAL_NAME="$(jq -r '.userPrincipalName // .mail' <<<"${USER_JSON}")"
echo "    objectId=${POSTGRES_ADMIN_OBJECT_ID}"
echo "    principal=${POSTGRES_ADMIN_PRINCIPAL_NAME}"

# azd refuses to pick a subscription when --no-prompt is set. Grower stored this
# in its azd env the first time `azd up` was run interactively. Reuse the current
# Azure CLI default (one subscription on this machine: YC SUS).
if [[ -z "${AZURE_SUBSCRIPTION_ID:-}" ]]; then
  AZURE_SUBSCRIPTION_ID="$(az account show --query id -o tsv)"
fi
if [[ -z "${AZURE_SUBSCRIPTION_ID}" || "${AZURE_SUBSCRIPTION_ID}" == "null" ]]; then
  echo "error: no Azure subscription. Run 'az login' or set AZURE_SUBSCRIPTION_ID." >&2
  exit 1
fi
SUB_NAME="$(az account show --subscription "${AZURE_SUBSCRIPTION_ID}" --query name -o tsv)"
echo "==> Azure subscription ${SUB_NAME} (${AZURE_SUBSCRIPTION_ID})"

if ! azd env list --output json 2>/dev/null | jq -e '.[] | select(.Name=="skywoop-prod")' >/dev/null; then
  echo "==> azd env new skywoop-prod"
  azd env new skywoop-prod
fi
azd env select skywoop-prod

azd env set AZURE_SUBSCRIPTION_ID "${AZURE_SUBSCRIPTION_ID}"
azd env set AZURE_LOCATION "${AZURE_LOCATION}"
azd env set POSTGRES_ADMIN_OBJECT_ID "${POSTGRES_ADMIN_OBJECT_ID}"
azd env set POSTGRES_ADMIN_PRINCIPAL_NAME "${POSTGRES_ADMIN_PRINCIPAL_NAME}"
azd env set PUBLIC_BASE_URL "${PUBLIC_BASE_URL}"
azd env set APP_ORIGIN "${APP_ORIGIN}"
azd env set AUTH_JWKS_URL "${AUTH_JWKS_URL}"
azd env set AUTH_ISSUER "${AUTH_ISSUER}"
azd env set AUTH_AUDIENCE "${AUTH_AUDIENCE}"
azd env set DEPLOY_OPENAI_MODELS false
# East US Postgres is restricted on this subscription. centralus matches grower.
# nameSalt avoids Key Vault / OpenAI names locked by purge protection.
# This machine runs postprovision against the public Postgres endpoint.
if [[ -z "${POSTGRES_CLIENT_IP:-}" ]]; then
  POSTGRES_CLIENT_IP="$(curl -fsS https://api.ipify.org || true)"
fi
if [[ -n "${POSTGRES_CLIENT_IP}" ]]; then
  azd env set POSTGRES_CLIENT_IP "${POSTGRES_CLIENT_IP}"
fi

echo "==> azd provision (placeholder image until ./scripts/deploy.sh)"
# azd up also builds the Rust image locally. That build was getting killed, and the
# first Container App revision is the public placeholder until a real image is pushed.
azd provision --no-prompt

RG="$(azd env get-value AZURE_RESOURCE_GROUP)"
FQDN="$(azd env get-value AZURE_CONTAINER_APP_FQDN)"
ENV_NAME="$(azd env get-value AZURE_CONTAINER_ENVIRONMENT_NAME)"
VERIFICATION_ID="$(az containerapp show -g "${RG}" -n skywoop-api \
  --query properties.customDomainVerificationId -o tsv)"

echo "==> Cloudflare DNS for api.skywoop.com (DNS-only / grey cloud)"
cf dns records create --zone skywoop.com --body "$(jq -n --arg content "${FQDN}" '{type:"CNAME",name:"api",content:$content,proxied:false,ttl:1}')" || true
cf dns records create --zone skywoop.com --body "$(jq -n --arg content "${VERIFICATION_ID}" '{type:"TXT",name:"asuid.api",content:$content,ttl:1}')" || true

echo "==> Bind custom hostname + managed cert"
az containerapp hostname add -g "${RG}" -n skywoop-api --hostname api.skywoop.com || true
az containerapp hostname bind \
  -g "${RG}" -n skywoop-api \
  --hostname api.skywoop.com \
  --environment "${ENV_NAME}" \
  --validation-method CNAME || true

echo "==> Health check"
HEALTH_OK=0
for i in $(seq 1 6); do
  if curl -fsS -m 15 "https://api.skywoop.com/healthz" >/tmp/skywoop-healthz.body; then
    echo "ok: $(tr -d '\n' </tmp/skywoop-healthz.body)"
    HEALTH_OK=1
    break
  fi
  echo "  attempt ${i}: not ready"
  sleep 5
done
if [[ "${HEALTH_OK}" != "1" ]]; then
  echo "Infra is up. https://api.skywoop.com/healthz is not the Skywoop API yet."
  echo "The Container App is still the placeholder image. Build and roll it out with:"
  echo "  ./scripts/deploy.sh"
fi
echo
echo "Next:"
echo "  1. ./scripts/deploy.sh   # real API image"
echo "  2. Put meta-app-secret / asc-p8 / link-hash-secret into Key Vault"
echo "  3. Add https://api.skywoop.com/oauth/meta/callback to the Meta app"
echo "  4. Add Resend DNS (SPF/DKIM) for skywoop.com"
echo "  5. Configure GitHub Actions production OIDC + vars"
