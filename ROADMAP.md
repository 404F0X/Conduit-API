# Conduit API Roadmap

Conduit API is currently in alpha. APIs and fresh-database migrations may
change before the first stable release.

## Alpha Priorities

- Complete production validation for model discovery, automatic mapping,
  upstream removal, and channel auto-disable behavior.
- Stabilize cache-aware routing with explicit affinity evidence, bounded
  fallback, and observable cache-hit economics.
- Finish the unified pricing-change workflow: immutable observations,
  reviewable drafts, approval, publication, and audit history.
- Validate accounting-currency conversion and customer credit display across
  all administration and billing surfaces.
- Harden backup/restore, billing reconciliation, scheduled jobs, and
  multi-instance operation on PostgreSQL.
- Publish protocol capability and provider support matrices from executable
  integration tests.

## Stable Release Criteria

- All checks in [RELEASE_GATES.md](RELEASE_GATES.md) pass on a clean commit.
- No critical or high-severity production dependency vulnerability remains.
- Fresh installation, first-owner setup, provider configuration, proxying,
  accounting, and backup/restore are verified end to end.
- Public GraphQL, HTTP, configuration, and protocol contracts are versioned and
  documented.

Feature requests and defects should be tracked in GitHub Issues rather than in
additional repository-local TODO ledgers.
