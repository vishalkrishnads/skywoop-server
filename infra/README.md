# Skywoop Azure infrastructure

Bicep + azd provision for ACA, ACR, Postgres 18 (Entra-only), Storage, Key Vault, and Azure OpenAI.

Auth is JWT (Better Auth JWKS on the frontend). Container Apps Easy Auth is not used.

## Prerequisites

- [Azure Developer CLI](https://learn.microsoft.com/azure/developer/azure-developer-cli/) (`azd`)
- Azure CLI (`az`) logged in with rights to create the subscription resources
- `psql` (for `postprovision.sh` Entra principal + grants)
- Cloudflare DNS for `skywoop.com` (custom domain + Resend)

## One-time provision

```bash
azd auth login
azd env new skywoop-prod

azd env set AZURE_LOCATION centralus
azd env set POSTGRES_ADMIN_OBJECT_ID <admin-entra-object-id>
azd env set POSTGRES_ADMIN_PRINCIPAL_NAME <admin-upn-or-display-name>
azd env set PUBLIC_BASE_URL https://api.skywoop.com
azd env set APP_ORIGIN https://skywoop.com
azd env set AUTH_JWKS_URL https://skywoop.com/api/auth/jwks
azd env set AUTH_ISSUER https://skywoop.com
azd env set AUTH_AUDIENCE skywoop-api

# Model names in infra/modules/openai.bicep are not offered in this region.
# Leave this false until those deployments are replaced with real SKUs.
azd env set DEPLOY_OPENAI_MODELS false

azd up
```

`azd up` deploys `infra/main.bicep`, then runs `infra/postprovision.sh` to create the UAMI Postgres principal and grant database/schema rights.

The first Container App revision may use a public placeholder image until GitHub Actions pushes `skywoop-api:<sha>` to ACR.

## Custom domain (api.skywoop.com)

After the first provision, bind the custom hostname (DNS-only / grey-cloud on Cloudflare so Azure can issue the managed cert):

```bash
# From azd env outputs:
FQDN="$(azd env get-value AZURE_CONTAINER_APP_FQDN)"
VERIFICATION_ID="$(az containerapp show -g rg-skywoop-prod -n skywoop-api \
  --query properties.customDomainVerificationId -o tsv)"

# Cloudflare DNS (cf CLI) — DNS-only, not proxied:
#   CNAME api -> $FQDN
#   TXT   asuid.api -> $VERIFICATION_ID

az containerapp hostname add \
  -g rg-skywoop-prod -n skywoop-api \
  --hostname api.skywoop.com

az containerapp hostname bind \
  -g rg-skywoop-prod -n skywoop-api \
  --hostname api.skywoop.com \
  --environment "$(azd env get-value AZURE_CONTAINER_ENVIRONMENT_NAME)" \
  --validation-method CNAME
```

Also add Resend domain verification records (SPF / DKIM / return-path) for `skywoop.com`.

## CI/CD image deploy

Configure the `production` GitHub Environment on `vishalkrishnads/skywoop-server` with:

| Name | Kind | Purpose |
|------|------|---------|
| `AZURE_CLIENT_ID` | secret | OIDC federated credential app |
| `AZURE_TENANT_ID` | secret | Entra tenant |
| `AZURE_SUBSCRIPTION_ID` | secret | Subscription |
| `AZURE_RESOURCE_GROUP` | variable | e.g. `rg-skywoop-prod` |
| `ACR_NAME` | variable | ACR resource name |
| `CONTAINER_APP_NAME` | variable | defaults to `skywoop-api` |

Create an Entra federated credential for:
`repo:vishalkrishnads/skywoop-server:environment:production`

Push to `main` (or run **deploy** via workflow_dispatch) to build, push to ACR, and `az containerapp update`.

## Secrets to load into Key Vault

After provision, set:

- `meta-app-secret`
- `asc-p8`
- `link-hash-secret`

And register `https://api.skywoop.com/oauth/meta/callback` on the Meta app.

## Resources created

- Resource group `rg-<env>`
- Log Analytics + Application Insights
- Container Apps environment + `skywoop-api` (0.5 vCPU / 1Gi, min=max=1)
- JWT auth env (`AUTH_MODE=jwt`, JWKS / issuer / audience / `APP_ORIGIN`)
- User-assigned managed identity + AcrPull, Key Vault Secrets Officer, Storage Blob Data Contributor, Cognitive Services OpenAI User
- ACR Basic
- Postgres Flexible Server `Standard_B1ms`, PostgreSQL 18, Entra-only auth
- Storage account Hot LRS + private `assets` container with Blob CORS for app origins
- Key Vault (RBAC authorization)
- Azure OpenAI account with deployment placeholders
