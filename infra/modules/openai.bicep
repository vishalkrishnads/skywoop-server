@description('Azure region (must support Azure OpenAI)')
param location string

@description('Cognitive Services / Foundry account name')
param name string

@description('Create model deployment placeholders (set false until SKUs exist in the region)')
param deployModels bool = true

@description('Tags applied to all resources')
param tags object = {}

// Placeholder deployments — update model versions/capacity when the SKUs are available.
// Placeholder deployments — update model names/versions when SKUs exist in the region.
// Kept: chat (frontier + mini), image gen (asset start frames), TTS, transcription.
var modelDeployments = [
  {
    name: 'gpt-5.5'
    modelName: 'gpt-5.5'
    modelVersion: '1'
    capacity: 10
  }
  {
    name: 'gpt-5.5-mini'
    modelName: 'gpt-5.5-mini'
    modelVersion: '1'
    capacity: 20
  }
  {
    name: 'gpt-image-2.5'
    modelName: 'gpt-image-2.5'
    modelVersion: '1'
    capacity: 1
  }
  {
    name: 'gpt-audio-1.5'
    modelName: 'gpt-audio-1.5'
    modelVersion: '2026-02-23'
    capacity: 5
  }
  {
    name: 'gpt-4o-transcribe'
    modelName: 'gpt-4o-transcribe'
    modelVersion: '1'
    capacity: 5
  }
]

resource account 'Microsoft.CognitiveServices/accounts@2024-10-01' = {
  name: name
  location: location
  tags: tags
  kind: 'OpenAI'
  sku: {
    name: 'S0'
  }
  identity: {
    type: 'SystemAssigned'
  }
  properties: {
    customSubDomainName: name
    publicNetworkAccess: 'Enabled'
    disableLocalAuth: true
  }
}

resource deployments 'Microsoft.CognitiveServices/accounts/deployments@2024-10-01' = [
  for (m, i) in modelDeployments: if (deployModels) {
    parent: account
    name: m.name
    sku: {
      name: 'GlobalStandard'
      capacity: m.capacity
    }
    properties: {
      model: {
        format: 'OpenAI'
        name: m.modelName
        version: m.modelVersion
      }
    }
  }
]

output id string = account.id
output name string = account.name
output endpoint string = account.properties.endpoint
output openaiBase string = '${account.properties.endpoint}openai/v1'
