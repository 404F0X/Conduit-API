# Contributing

Conduit API is in active alpha development. Open an issue before a broad API,
schema, routing, billing, or user-interface redesign so the intended contract
can be agreed first.

## Development

1. Create a focused branch from the current default branch.
2. Keep changes scoped and include tests for behavior changes.
3. Update GraphQL, HTTP, configuration, or protocol fixtures when a public
   contract intentionally changes.
4. Run the relevant checks from `AGENTS.md` and report any gate that could not
   be run.

Do not commit credentials, production payloads, local databases, generated
build output, or customer data. Real-provider tests must remain opt-in.

## Pull Requests

Describe the behavior change, compatibility impact, validation performed, and
any operational action required. Database changes must include a PostgreSQL
migration and isolated integration coverage. Settings changes must be wired
through the console, GraphQL API, persistence, and runtime behavior.

By contributing, you agree that your contribution is licensed under the
license that applies to the files you modify, as described in `LICENSE`.
