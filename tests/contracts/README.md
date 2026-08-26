# Public Contract Fixtures

This directory contains committed compatibility fixtures for Conduit API's
public HTTP, GraphQL, configuration, and provider-protocol surfaces.

- `routes_snapshot.md`: externally reachable route inventory.
- `admin_graphql_schema.graphql`: administration GraphQL contract.
- `openapi_graphql_schema.graphql`: project automation GraphQL contract.
- `config_defaults.json`: source configuration defaults.
- `llm_cases/`: protocol request, response, stream, usage, and error fixtures.

Contract changes must be intentional. Update the implementation and fixture in
the same change, explain the compatibility impact, and keep drift tests green.
Fixtures must not contain credentials, production payloads, or nondeterministic
timestamps and identifiers.

The Rust schema builders and protocol tests are the source of current product
behavior. These fixtures do not imply compatibility with another codebase.
