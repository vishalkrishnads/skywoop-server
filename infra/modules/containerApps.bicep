@description('Azure region')
param location string

@description('Resource name prefix')
param namePrefix string

@description('User-assigned managed identity resource ID')
param uamiId string

@description('User-assigned managed identity client ID')
param uamiClientId string

@description('ACR login server')
param acrLoginServer string

@description('Container image (registry/repo:tag)')
param containerImage string

@description('Log Analytics workspace name (same resource group)')
param logAnalyticsName string

@description('App Insights connection string')
@secure()
param appInsightsConnectionString string

@description('PUBLIC_BASE_URL (set to https://api.skywoop.com in production)')
param publicBaseUrl string = ''

@description('Frontend origin for CORS and OAuth done redirects')
param appOrigin string = 'https://skywoop.com'

@description('Better Auth JWKS URL (frontend)')
param authJwksUrl string = ''

@description('JWT issuer (Better Auth)')
param authIssuer string = ''

@description('JWT audience')
param authAudience string = 'skywoop-api'

@description('Postgres connection string (no password; Entra auth)')
param databaseUrl string

@description('Storage account name')
param blobAccount string

@description('Blob container name')
param blobContainer string

@description('Key Vault URI')
param keyVaultUrl string

@description('Azure OpenAI v1 base URL')
param azureOpenaiBase string

@description('Tags applied to all resources')
param tags object = {}

resource logAnalytics 'Microsoft.OperationalInsights/workspaces@2022-10-01' existing = {
  name: logAnalyticsName
}

resource managedEnv 'Microsoft.App/managedEnvironments@2024-10-02-preview' = {
  name: 'cae-${namePrefix}'
  location: location
  tags: tags
  properties: {
    appLogsConfiguration: {
      destination: 'log-analytics'
      logAnalyticsConfiguration: {
        customerId: logAnalytics.properties.customerId
        sharedKey: logAnalytics.listKeys().primarySharedKey
      }
    }
    appInsightsConfiguration: {
      connectionString: appInsightsConnectionString
    }
    openTelemetryConfiguration: {
      tracesConfiguration: {
        destinations: [
          'appInsights'
        ]
      }
      logsConfiguration: {
        destinations: [
          'appInsights'
        ]
      }
    }
    workloadProfiles: [
      {
        name: 'Consumption'
        workloadProfileType: 'Consumption'
      }
    ]
  }
}

var resolvedPublicBase = !empty(publicBaseUrl)
  ? publicBaseUrl
  : 'https://skywoop-api.${managedEnv.properties.defaultDomain}'

// Trim trailing slash from appOrigin for OAuth done URL.
var appOriginTrimmed = endsWith(appOrigin, '/') ? substring(appOrigin, 0, length(appOrigin) - 1) : appOrigin
var oauthDoneUrl = '${appOriginTrimmed}/onboarding/meta/done'

resource api 'Microsoft.App/containerApps@2024-03-01' = {
  name: 'skywoop-api'
  location: location
  tags: tags
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: {
      '${uamiId}': {}
    }
  }
  properties: {
    managedEnvironmentId: managedEnv.id
    workloadProfileName: 'Consumption'
    configuration: {
      activeRevisionsMode: 'Single'
      ingress: {
        external: true
        targetPort: 8080
        transport: 'Auto'
        allowInsecure: false
      }
      registries: [
        {
          server: acrLoginServer
          identity: uamiId
        }
      ]
    }
    template: {
      containers: [
        {
          name: 'skywoop-api'
          image: containerImage
          resources: {
            cpu: json('0.5')
            memory: '1Gi'
          }
          probes: [
            {
              type: 'Liveness'
              httpGet: {
                path: '/healthz'
                port: 8080
              }
              initialDelaySeconds: 10
              periodSeconds: 30
            }
            {
              type: 'Readiness'
              httpGet: {
                path: '/readyz'
                port: 8080
              }
              initialDelaySeconds: 5
              periodSeconds: 10
            }
          ]
          env: [
            { name: 'APP_ENV', value: 'azure' }
            { name: 'DB_AUTH', value: 'entra' }
            { name: 'AUTH_MODE', value: 'jwt' }
            { name: 'AZURE_CLIENT_ID', value: uamiClientId }
            { name: 'PUBLIC_BASE_URL', value: resolvedPublicBase }
            { name: 'APP_ORIGIN', value: appOrigin }
            { name: 'OAUTH_DONE_URL', value: oauthDoneUrl }
            { name: 'AUTH_JWKS_URL', value: authJwksUrl }
            { name: 'AUTH_ISSUER', value: authIssuer }
            { name: 'AUTH_AUDIENCE', value: authAudience }
            { name: 'DATABASE_URL', value: databaseUrl }
            { name: 'BLOB_ACCOUNT', value: blobAccount }
            { name: 'BLOB_CONTAINER', value: blobContainer }
            { name: 'KEYVAULT_URL', value: keyVaultUrl }
            { name: 'AZURE_OPENAI_BASE', value: azureOpenaiBase }
            { name: 'DEPLOYMENT_PLANNER', value: 'gpt-6.1-sol' }
            { name: 'DEPLOYMENT_VISION', value: 'gpt-6.1-sol' }
            { name: 'DEPLOYMENT_COPY', value: 'gpt-6-luna' }
            { name: 'DEPLOYMENT_IMAGE_DRAFT', value: 'gpt-image-2.5-flare' }
            { name: 'DEPLOYMENT_IMAGE_FINAL', value: 'gpt-image-2.5-sunburst' }
            { name: 'IMAGE_RPM', value: '5' }
            { name: 'LISTEN_ADDR', value: '0.0.0.0:8080' }
            { name: 'APPLICATIONINSIGHTS_CONNECTION_STRING', value: appInsightsConnectionString }
          ]
        }
      ]
      scale: {
        minReplicas: 1
        maxReplicas: 1
      }
    }
  }
}

output environmentId string = managedEnv.id
output environmentName string = managedEnv.name
output containerAppId string = api.id
output containerAppName string = api.name
output fqdn string = api.properties.configuration.ingress.fqdn
output defaultDomain string = managedEnv.properties.defaultDomain
output customDomainVerificationId string = api.properties.customDomainVerificationId
