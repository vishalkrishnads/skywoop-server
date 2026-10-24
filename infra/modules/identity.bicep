@description('Azure region')
param location string

@description('Resource name prefix')
param namePrefix string

@description('Tags applied to all resources')
param tags object = {}

resource uami 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: 'id-${namePrefix}'
  location: location
  tags: tags
}

output id string = uami.id
output name string = uami.name
output clientId string = uami.properties.clientId
output principalId string = uami.properties.principalId
