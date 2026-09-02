# Internal administrator API

Conduit API exposes its administrator GraphQL schema for external automation at:

```text
POST /internal/v1/graphql
Authorization: Bearer <service-account-key>
Content-Type: application/json
```

The key must be database-backed, enabled, have type `service_account`, and
carry `system:admin`. Missing or invalid credentials and non-service-account
keys return HTTP `401`; a service account without `system:admin` returns HTTP
`403`. Only an owner may grant `system:admin` while creating or editing a
service account.

This endpoint is system-wide: it deliberately does not inherit the service
account's Project as a tenant filter. Operations that mutate customer state
must pass the target user, Project, subscription, or API-key ID explicitly.
Project self-service automation belongs on `/openapi/v1/graphql`.

Do not put the key in a URL. Deploy the endpoint behind TLS, store the key as a
secret, and rotate it like an administrator credential.

## Stable management surface

`system:admin` is the endpoint admission scope and is promoted to owner
authority. The table also records each field's underlying admin-schema scope,
which remains relevant to browser/RBAC callers and is guarded by a contract
test.

| Resource | Queries | Mutations | Underlying scopes |
|---|---|---|---|
| Users | `users` | `createUser`, `updateUser`, `updateUserStatus`, `deleteUser` | `read_users`, `write_users` |
| Projects/membership | `projects` | `createProject`, `updateProject`, `addUserToProject`, `removeUserFromProject`, `updateProjectUser` | `read_projects`, `write_projects`, `write_users` |
| Model groups | `simpleGroups` | `createSimpleGroup`, `updateSimpleGroup`, `assignSimpleGroupUsers`, `updateSimpleGroupModels`, `updateSimpleGroupPrice`, `deleteSimpleGroup` | `read_groups`, `write_groups` |
| Subscriptions | `subscriptionPlans`, `userSubscriptions`, `subscriptionProjects` | `createSubscriptionPlan`, `updateSubscriptionPlan`, `assignUserSubscription`, `pauseUserSubscription`, `resumeUserSubscription`, `cancelUserSubscription`, `renewUserSubscription`, `setSubscriptionAutoRenew` | `read_subscriptions`, `write_subscriptions` |
| Wallet | `userBalance`, `projectBalance`, `projectWalletComparison` | `grantUserCredit`, `grantProjectCredit` | `read_billing`, `grant_credit` |
| Redemption codes | `creditRedemptionCodes` (masked inventory only) | `createCreditRedemptionCodes`, `revokeCreditRedemptionCode` | `read_billing`, `grant_credit` |
| API keys | `apiKeys`, `apiKeyProfileTemplates`, `apiKeyQuotaUsages`, `apiKeyTokenUsageStats` | `createAPIKey`, `updateAPIKey`, `updateAPIKeyStatus`, `rotateAPIKey`, `updateAPIKeyProfiles`, profile-template mutations | `read_api_keys`, `write_api_keys` |
| Channels/models | `channels`, `models`, `modelRoutes`, `upstreamModelDeployments` | channel/model CRUD, `upsertModelRoute`, `createPublicModelWithRoutes` | `read_channels`, `write_channels` |

GraphQL introspection is available over this authenticated POST endpoint. There
is deliberately no unauthenticated playground. The live schema is authoritative
for complete input objects and newly added fields.

`createCreditRedemptionCodes` is the only response that contains newly
generated plaintext codes. Store or deliver that response immediately; later
inventory queries expose only a non-secret suffix hint. Project members redeem
through the authenticated console using the current Project context, not by
supplying a target user or Project ID.

## HTTP and GraphQL errors

Authentication failures use HTTP `401`/`403` JSON errors before GraphQL runs.
Once authenticated, GraphQL validation, authorization, not-found, and storage
errors use the normal GraphQL envelope and may have HTTP `200`:

```json
{"data":null,"errors":[{"message":"...","path":["operationName"]}]}
```

Automation must inspect both the HTTP status and the top-level `errors` array.
IDs are GraphQL `ID` strings. Preserve returned IDs verbatim rather than
assuming every domain uses the same numeric/GID representation.

## Users

```graphql
query Users($first: Int) {
  users(first: $first) {
    totalCount
    edges { node { id email status firstName lastName } }
  }
}

mutation UpdateUser($id: ID!, $input: UpdateUserInput!) {
  updateUser(id: $id, input: $input) { id email status firstName lastName }
}
```

## Subscription lifecycle

Subscription assignment requires an explicit Project and a caller-generated
`idempotencyKey`. Reuse the key when retrying the same intended assignment;
use a new key for every intentional additional subscription. A plan can carry
several model groups through `accessPlanIDs`.

```graphql
mutation CreatePlan($input: CreateSubscriptionPlanInput!) {
  createSubscriptionPlan(input: $input) {
    id name allowance intervalUnit accessPlans { id name }
  }
}

mutation AssignSubscription($input: AssignUserSubscriptionInput!) {
  assignUserSubscription(input: $input) {
    id status projectID remainingAllowance
    plan { id name }
    grantedAccessPlans { id name }
  }
}

mutation CancelSubscription($subscriptionID: ID!) {
  cancelUserSubscription(subscriptionID: $subscriptionID) { id status }
}
```

Example variables:

```json
{
  "input": {
    "userID": "1",
    "projectID": "10",
    "planID": "2",
    "idempotencyKey": "assign-20260823-01",
    "autoRenew": true
  }
}
```

## Project credit

Credit grants require a caller-generated idempotency key. Retrying the same
business operation must reuse that key.

```graphql
mutation GrantProjectCredit($input: GrantProjectCreditInput!) {
  grantProjectCredit(input: $input) {
    projectID currency creditBalance subscriptionBalance reservedBalance availableBalance
  }
}
```

```json
{
  "input": {
    "projectID": "10",
    "amount": "25.00",
    "currency": "USD",
    "description": "support adjustment",
    "idempotencyKey": "ticket-20260816-001"
  }
}
```

## API-key policy and concurrency

API-key policy belongs to the selected active Profile. Model/channel/time/quota
restrictions and `maxConcurrentRequests` are updated atomically together.
`null` or `0` means no concurrency limit.

```graphql
mutation UpdateAPIKeyProfiles($id: ID!, $input: UpdateAPIKeyProfilesInput!) {
  updateAPIKeyProfiles(id: $id, input: $input) {
    id name
    profiles {
      activeProfile
      profiles {
        name modelIDs channelIDs validFrom validUntil maxConcurrentRequests
      }
    }
  }
}
```

```json
{
  "id": "42",
  "input": {
    "activeProfile": "production",
    "profiles": [{
      "name": "production",
      "modelIDs": ["gpt-5"],
      "channelIDs": [3, 7],
      "maxConcurrentRequests": 4
    }]
  }
}
```
