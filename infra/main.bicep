targetScope = 'subscription'

@minLength(1)
@maxLength(64)
@description('Name of the environment used to generate resource names (azd environmentName)')
param environmentName string

@minLength(1)
@description('Primary Azure region for all resources')
param location string

@description('Entra object ID that becomes the Postgres Flexible Server administrator')
param postgresAdminObjectId string

@description('Display name / UPN of the Postgres Entra administrator')
param postgresAdminPrincipalName string

@description('Principal type for the Postgres Entra administrator')
@allowed([
  'User'
  'Group'
  'ServicePrincipal'
])
param postgresAdminPrincipalType string = 'User'

@description('Create Azure OpenAI model deployments. Off until the model names exist in this region.')
param deployOpenaiModels bool = false

@description('PUBLIC_BASE_URL (production: https://api.skywoop.com)')
param publicBaseUrl string = 'https://api.skywoop.com'

@description('Frontend origin for CORS / OAuth done (production: https://skywoop.com)')
param appOrigin string = 'https://skywoop.com'

@description('Better Auth JWKS URL on the frontend')
param authJwksUrl string = ''

@description('JWT issuer (Better Auth base URL)')
param authIssuer string = ''

@description('JWT audience')
param authAudience string = 'skywoop-api'

@description('Browser origins for Blob CORS. Example: ["http://localhost:3000","https://skywoop.com"]')
param blobCorsOrigins array = [
  'http://localhost:3000'
  'https://skywoop.com'
  'https://www.skywoop.com'
]

@description('Azure OpenAI v1 base URL. Empty uses the account created in this deployment.')
param azureOpenaiBase string = 'https://grower-2.openai.azure.com/openai/v1'

@description('Container image tag to deploy (use a real ACR image after first push)')
param containerImageTag string = 'latest'

@description('Suffix mixed into globally unique names. Bump this if a previous name is soft-deleted.')
param nameSalt string = 'cus'

var namePrefix = toLower(take('${environmentName}${nameSalt}${uniqueString(subscription().id, environmentName, nameSalt)}', 20))
var tags = {
  'azd-env-name': environmentName
  app: 'skywoop'
}

var acrName = take('cr${replace(namePrefix, '-', '')}', 50)
var storageName = take('st${replace(namePrefix, '-', '')}', 24)
var keyVaultName = take('kv-${namePrefix}', 24)
var openaiName = take('oai-${namePrefix}', 64)
var postgresName = take('pg-${namePrefix}', 63)
var resourceGroupName = 'rg-${environmentName}'

resource rg 'Microsoft.Resources/resourceGroups@2024-03-01' = {
  name: resourceGroupName
  location: location
  tags: tags
}

module monitoring 'modules/monitoring.bicep' = {
  name: 'monitoring'
  scope: rg
  params: {
    location: location
    namePrefix: namePrefix
    tags: tags
  }
}

module identity 'modules/identity.bicep' = {
  name: 'identity'
  scope: rg
  params: {
    location: location
    namePrefix: namePrefix
    tags: tags
  }
}

module acr 'modules/acr.bicep' = {
  name: 'acr'
  scope: rg
  params: {
    location: location
    name: acrName
    tags: tags
  }
}

module postgres 'modules/postgres.bicep' = {
  name: 'postgres'
  scope: rg
  params: {
    location: location
    name: postgresName
    databaseName: 'skywoop'
    entraAdminObjectId: postgresAdminObjectId
    entraAdminPrincipalName: postgresAdminPrincipalName
    entraAdminPrincipalType: postgresAdminPrincipalType
    clientIpAddress: postgresClientIp
    tags: tags
  }
}

module storage 'modules/storage.bicep' = {
  name: 'storage'
  scope: rg
  params: {
    location: location
    name: storageName
    containerName: 'assets'
    corsOrigins: blobCorsOrigins
    tags: tags
  }
}

module keyVault 'modules/keyVault.bicep' = {
  name: 'keyVault'
  scope: rg
  params: {
    location: location
    name: keyVaultName
    tags: tags
  }
}

module openai 'modules/openai.bicep' = {
  name: 'openai'
  scope: rg
  params: {
    location: location
    name: openaiName
    deployModels: deployOpenaiModels
    tags: tags
  }
}

module roles 'modules/roles.bicep' = {
  name: 'roles'
  scope: rg
  params: {
    principalId: identity.outputs.principalId
    acrName: acr.outputs.name
    keyVaultName: keyVault.outputs.name
    storageAccountName: storage.outputs.name
    openaiName: openai.outputs.name
  }
}

var placeholderImage = 'mcr.microsoft.com/azuredocs/containerapps-helloworld:latest'
var resolvedImage = containerImageTag == 'latest'
  ? placeholderImage
  : '${acr.outputs.loginServer}/skywoop-api:${containerImageTag}'

module containerApps 'modules/containerApps.bicep' = {
  name: 'containerApps'
  scope: rg
  params: {
    location: location
    namePrefix: namePrefix
    uamiId: identity.outputs.id
    uamiClientId: identity.outputs.clientId
    acrLoginServer: acr.outputs.loginServer
    containerImage: resolvedImage
    logAnalyticsName: monitoring.outputs.logAnalyticsName
    appInsightsConnectionString: monitoring.outputs.appInsightsConnectionString
    publicBaseUrl: publicBaseUrl
    appOrigin: appOrigin
    authJwksUrl: authJwksUrl
    authIssuer: authIssuer
    authAudience: authAudience
    databaseUrl: 'postgres://${identity.outputs.name}@${postgres.outputs.fqdn}:5432/${postgres.outputs.databaseName}?sslmode=require'
    blobAccount: storage.outputs.name
    blobContainer: storage.outputs.containerName
    keyVaultUrl: keyVault.outputs.uri
    azureOpenaiBase: !empty(azureOpenaiBase) ? azureOpenaiBase : openai.outputs.openaiBase
    tags: tags
  }
  dependsOn: [
    roles
  ]
}

// --- azd / deploy outputs ---
output AZURE_LOCATION string = location
output AZURE_RESOURCE_GROUP string = rg.name
output AZURE_CONTAINER_REGISTRY_NAME string = acr.outputs.name
output AZURE_CONTAINER_REGISTRY_ENDPOINT string = acr.outputs.loginServer
output AZURE_UAMI_NAME string = identity.outputs.name
output AZURE_UAMI_CLIENT_ID string = identity.outputs.clientId
output AZURE_UAMI_PRINCIPAL_ID string = identity.outputs.principalId
output AZURE_POSTGRES_FQDN string = postgres.outputs.fqdn
output AZURE_POSTGRES_DB string = postgres.outputs.databaseName
output AZURE_POSTGRES_ADMIN_USER string = postgresAdminPrincipalName
output AZURE_STORAGE_ACCOUNT string = storage.outputs.name
output AZURE_STORAGE_CONTAINER string = storage.outputs.containerName
output AZURE_KEYVAULT_NAME string = keyVault.outputs.name
output AZURE_KEYVAULT_URL string = keyVault.outputs.uri
output AZURE_OPENAI_NAME string = openai.outputs.name
output AZURE_OPENAI_BASE string = openai.outputs.openaiBase
output AZURE_CONTAINER_APP_NAME string = containerApps.outputs.containerAppName
output AZURE_CONTAINER_APP_FQDN string = containerApps.outputs.fqdn
output AZURE_CONTAINER_ENVIRONMENT_NAME string = containerApps.outputs.environmentName
output SERVICE_API_NAME string = containerApps.outputs.containerAppName
output SERVICE_API_URI string = 'https://${containerApps.outputs.fqdn}'
