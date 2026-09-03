# Rebuilding and relinking Conduit API

Official binary releases combine Apache-2.0 code with the
`LGPL-3.0-only` crates identified in [LICENSING.md](LICENSING.md). Each GitHub
Release therefore attaches a separate `conduit-api-*-source.tar.gz` archive
from the exact tagged commit used for its binaries. Each native archive carries
`SOURCE.md`, which identifies that source asset and commit. The same commit is
reported by `conduit-api build-info`.

To rebuild a release, extract that source archive. Install the Rust version
pinned by `rust-toolchain.toml`, Node.js from `.node-version`, and pnpm from
`frontend/package.json`. Then run:

```sh
corepack enable
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend build
cargo build --locked --release -p conduit-bin --bin conduit-api \
  --features release-binary
```

Pass `--target <rust-target>` to rebuild for a platform-specific target. The
`release-binary` feature includes Redis support and embeds the generated web
console, so the rebuilt executable does not depend on an external
`frontend/dist` directory. Optional `CONDUIT_VERSION`, `CONDUIT_COMMIT`,
`CONDUIT_BUILD_TIME`, and `CONDUIT_BRANCH` build-time environment variables
populate `build-info`; the version also drives the `version` command, while
version, commit, and build time populate the administrative GraphQL
system-version metadata.

You may replace the distributed executable with a rebuilt one; Conduit API
does not use signature checks or other technical measures to prevent modified
builds from running. PostgreSQL remains an external runtime dependency. Cargo
and pnpm lockfiles identify the exact third-party source versions used by the
release.

The license texts control over this practical build note. See `LICENSE`,
`LICENSES/LGPL-3.0-only.txt`, `LICENSES/RUST_THIRD_PARTY_LICENSES.html`,
`NOTICE`, and `LICENSING.md` for the applicable terms. This document is not
legal advice.
