#!/usr/bin/env bash
# Build, push, and roll out skywoop-api to the Container App.
#
# Migrations run when the new revision connects (see src/db/mod.rs). # Auth is JWT. Env, Blob CORS, and Entra roles are left as already configured on the app.
#
# A squashed baseline cannot apply over an older _sqlx_migrations history. For
# that one-time case, reset the schema first:
#   RESET_SCHEMA=1 ./scripts/deploy.sh
#
# Usage (from repo root):
#   ./scripts/deploy.sh
#
# Optional overrides:
#   RESOURCE_GROUP=rg-skywoop-prod
#   ACR_NAME=   # defaults to the registry in RESOURCE_GROUP
#   CONTAINER_APP=skywoop-api
#   IMAGE_TAG=latest
#   HEALTH_URL=https://api.skywoop.com/healthz
#   PLATFORM=linux/amd64
#   RESET_SCHEMA=1

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"

RESOURCE_GROUP="${RESOURCE_GROUP:-rg-skywoop-prod}"
if [[ -z "${ACR_NAME:-}" || "${ACR_NAME}" == "CHANGE_ME_ACR" ]]; then
  ACR_NAME="$(az acr list -g "${RESOURCE_GROUP}" --query "[0].name" -o tsv)"
fi
if [[ -z "${ACR_NAME}" ]]; then
  echo "error: ACR_NAME is required" >&2
  exit 1
fi
CONTAINER_APP="${CONTAINER_APP:-skywoop-api}"
IMAGE_TAG="${IMAGE_TAG:-latest}"
PLATFORM="${PLATFORM:-linux/amd64}"
HEALTH_URL="${HEALTH_URL:-https://api.skywoop.com/healthz}"
RESET_SCHEMA="${RESET_SCHEMA:-0}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: $1 is required" >&2
    exit 1
  }
}

need az
need docker
need curl

echo "==> Azure account"
az account show --query "{name:name,id:id}" -o json

LOGIN_SERVER="$(az acr show -n "${ACR_NAME}" -g "${RESOURCE_GROUP}" --query loginServer -o tsv)"
IMAGE="${LOGIN_SERVER}/skywoop-api:${IMAGE_TAG}"

echo "==> ACR login (${ACR_NAME})"
az acr login -n "${ACR_NAME}"

echo "==> Build ${IMAGE} (${PLATFORM})"
docker build --platform "${PLATFORM}" -t "${IMAGE}" .

echo "==> Push ${IMAGE}"
docker push "${IMAGE}"

DIGEST="$(docker image inspect "${IMAGE}" --format '{{index .RepoDigests 0}}')"
if [[ "${DIGEST}" == *"@"* ]]; then
  ROLL_IMAGE="${DIGEST}"
else
  DIGEST_ONLY="$(az acr repository show -n "${ACR_NAME}" --image "skywoop-api:${IMAGE_TAG}" --query digest -o tsv)"
  ROLL_IMAGE="${LOGIN_SERVER}/skywoop-api@${DIGEST_ONLY}"
fi

if [[ "${RESET_SCHEMA}" == "1" ]]; then
  echo "==> Stop replicas before resetting the schema"
  az containerapp update \
    -g "${RESOURCE_GROUP}" \
    -n "${CONTAINER_APP}" \
    --min-replicas 0 \
    --output none
  for rev in $(az containerapp revision list -g "${RESOURCE_GROUP}" -n "${CONTAINER_APP}" --query "[?properties.active].name" -o tsv); do
    az containerapp revision deactivate -g "${RESOURCE_GROUP}" -n "${CONTAINER_APP}" --revision "${rev}" --output none || true
  done
  "${ROOT}/scripts/reset-db.sh"
fi

echo "==> Update ${CONTAINER_APP} -> ${ROLL_IMAGE}"
az containerapp update \
  -g "${RESOURCE_GROUP}" \
  -n "${CONTAINER_APP}" \
  --image "${ROLL_IMAGE}" \
  --min-replicas 1 \
  --output none

REVISION="$(az containerapp show -g "${RESOURCE_GROUP}" -n "${CONTAINER_APP}" --query properties.latestRevisionName -o tsv)"
echo "==> Latest revision: ${REVISION}"

echo "==> Wait for revision ${REVISION}"
HEALTH=""
RUNNING=""
for i in $(seq 1 40); do
  LINE="$(az containerapp revision show \
    -g "${RESOURCE_GROUP}" \
    -n "${CONTAINER_APP}" \
    --revision "${REVISION}" \
    --query "[properties.healthState, properties.runningState]" \
    -o tsv 2>/dev/null || true)"
  HEALTH="$(printf '%s' "${LINE}" | awk 'NR==1{print}')"
  RUNNING="$(printf '%s' "${LINE}" | awk 'NR==2{print}')"
  echo "  attempt ${i}: health=${HEALTH:-pending} running=${RUNNING:-pending}"
  # ACA reports RunningAtMaxScale when min==max replicas; treat that as ready.
  if [[ "${HEALTH}" == "Healthy" && ( "${RUNNING}" == "Running" || "${RUNNING}" == "RunningAtMaxScale" ) ]]; then
    break
  fi
  case "${HEALTH}${RUNNING}" in
    *Failed*|*Degraded*|*ActivationFailed*)
      echo "error: revision ${REVISION} is not healthy (health=${HEALTH} running=${RUNNING})" >&2
      az containerapp logs show -g "${RESOURCE_GROUP}" -n "${CONTAINER_APP}" --revision "${REVISION}" --tail 80 >&2 || true
      exit 1
      ;;
  esac
  sleep 10
done
if [[ "${HEALTH}" != "Healthy" || ( "${RUNNING}" != "Running" && "${RUNNING}" != "RunningAtMaxScale" ) ]]; then
  echo "error: revision ${REVISION} did not become healthy (health=${HEALTH} running=${RUNNING})" >&2
  az containerapp logs show -g "${RESOURCE_GROUP}" -n "${CONTAINER_APP}" --revision "${REVISION}" --tail 80 >&2 || true
  exit 1
fi

echo "==> Health check ${HEALTH_URL}"
for i in $(seq 1 20); do
  if CODE="$(curl -sS -m 15 -o /tmp/skywoop-healthz.body -w '%{http_code}' "${HEALTH_URL}")" \
    && [[ "${CODE}" == "200" ]]; then
    BODY="$(tr -d '\n' </tmp/skywoop-healthz.body)"
    echo "ok (${CODE}) ${BODY}"
    exit 0
  fi
  echo "  attempt ${i}: HTTP ${CODE:-none}"
  sleep 5
done

echo "error: health check failed after rollout" >&2
az containerapp logs show -g "${RESOURCE_GROUP}" -n "${CONTAINER_APP}" --revision "${REVISION}" --tail 80 >&2 || true
exit 1
