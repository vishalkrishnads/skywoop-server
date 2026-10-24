using 'main.bicep'

param environmentName = readEnvironmentVariable('AZURE_ENV_NAME', 'skywoop')
param location = readEnvironmentVariable('AZURE_LOCATION', 'centralus')
param postgresAdminObjectId = readEnvironmentVariable('POSTGRES_ADMIN_OBJECT_ID', '')
param postgresAdminPrincipalName = readEnvironmentVariable('POSTGRES_ADMIN_PRINCIPAL_NAME', '')
param postgresAdminPrincipalType = readEnvironmentVariable('POSTGRES_ADMIN_PRINCIPAL_TYPE', 'User')
param deployOpenaiModels = readEnvironmentVariable('DEPLOY_OPENAI_MODELS', 'false') == 'true'
param publicBaseUrl = readEnvironmentVariable('PUBLIC_BASE_URL', 'https://api.skywoop.com')
param appOrigin = readEnvironmentVariable('APP_ORIGIN', 'https://skywoop.com')
param authJwksUrl = readEnvironmentVariable('AUTH_JWKS_URL', 'https://skywoop.com/api/auth/jwks')
param authIssuer = readEnvironmentVariable('AUTH_ISSUER', 'https://skywoop.com')
param authAudience = readEnvironmentVariable('AUTH_AUDIENCE', 'skywoop-api')
param containerImageTag = readEnvironmentVariable('CONTAINER_IMAGE_TAG', 'latest')
param nameSalt = readEnvironmentVariable('AZURE_NAME_SALT', 'cus')
param postgresClientIp = readEnvironmentVariable('POSTGRES_CLIENT_IP', '')
param azureOpenaiBase = readEnvironmentVariable('AZURE_OPENAI_BASE', 'https://grower-2.openai.azure.com/openai/v1')
