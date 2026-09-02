# Conduit API Release Checklist

A release is publishable only when these checks pass from a clean commit. Alpha
tags may document known product limitations, but they must not bypass build,
security, licensing, or data-integrity checks.

## Source And Identity

- [ ] Tracked paths and contents contain no predecessor product identifiers.
- [ ] Cargo packages, binary names, environment variables, API identifiers,
      Docker resources, and repository URLs use Conduit API naming.
- [ ] `LICENSE`, `NOTICE`, dependency notices, and frontend attribution are
      present in source and distribution artifacts.

## Rust

- [ ] `cargo metadata --locked --no-deps --format-version 1`
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test --locked --workspace --all-targets`
- [ ] `cargo clippy --locked --workspace --all-targets`
- [ ] `bash scripts/generate_config_schema.sh --check`
- [ ] `node scripts/rust/security-static-check.mjs`
- [ ] `node scripts/rust/security-static-check.mjs --self-test`

## Frontend

- [ ] `pnpm --dir frontend install --frozen-lockfile`
- [ ] `pnpm --dir frontend format:check`
- [ ] `pnpm --dir frontend lint`
- [ ] `pnpm --dir frontend test:unit`
- [ ] `pnpm --dir frontend build`
- [ ] `pnpm --dir frontend test:e2e:check`
- [ ] `pnpm --dir frontend test:e2e -- --reporter=line` against a fresh,
      dedicated `conduit_e2e*` PostgreSQL database and the local mock upstream

## PostgreSQL And Runtime

- [ ] Fresh PostgreSQL migration succeeds on the supported major version.
- [ ] Repository integration tests pass against an isolated database.
- [ ] Docker Compose starts with no embedded application or database password.
- [ ] First-owner setup, sign-in, provider configuration, model sync, proxying,
      usage settlement, and backup/restore pass end to end.
- [ ] Graceful shutdown, retry, rate limiting, route health, and scheduled jobs
      are verified under failure injection.

## Security And Supply Chain

- [ ] Secret scanning passes for the release commit and distributable files.
- [ ] `cargo audit` has no unacknowledged production vulnerability.
- [ ] `pnpm --dir frontend audit --prod` has no critical or high finding.
- [ ] Container runs as a non-root user with loopback-safe source defaults.
- [ ] Release artifacts include checksums, build provenance, and an SBOM.

The Rust security-invariant check is intentionally repository-specific. It
blocks test/system bypass principals at request boundaries, empty project-id
fallbacks in authorization/query code, raw sensitive values passed to tracing
macros, and regressions in the process-wide TLS setting or usage-query project
guards. Inline `#[cfg(test)]` modules and integration-test directories are
excluded. Fourteen legacy `Principal::test()` fallbacks are pinned by exact
file and occurrence count in the script: any new occurrence fails, and removal
makes the baseline stale so it must shrink in the same change. This is a
containment measure, not closure of HLT-002/MNT-002; those fallbacks can only be
removed by carrying an authenticated request context through the legacy wiring
trait interfaces.

This gate does not replace a Rust-aware whole-program SAST engine. CodeQL does
not currently support Rust, and broad regex rules for taint flow, SQL injection,
or async correctness would create an unactionable false-positive baseline.
Likewise, the existing Clippy debt remains a separate maintenance task: the
release gate runs workspace Clippy with the repository lint policy, but does not
turn every stylistic/pedantic lint into a release blocker.

## Automated publication

`.github/workflows/publish-release.yml` accepts only a `v*` tag whose value
exactly matches `workspace.package.version` and whose commit is contained in
`main`. It reuses the complete release-gates workflow before publishing. The
result is a multi-architecture GHCR image, keyless Cosign signature, GitHub
build-provenance attestation, registry SBOM/provenance attestations, immutable
image-digest manifest, checksum, and GitHub Release. Third-party Actions are
pinned to immutable commits and updated through Dependabot.

Record the command output and commit SHA with each release. Do not check an item
based on an older dirty-worktree run.
