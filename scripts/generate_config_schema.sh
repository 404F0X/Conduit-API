#!/usr/bin/env bash
# Regenerate config.schema.json from the Rust AppConfig model.
#
# Source of truth: crates/conduit-config/src/model.rs (structs derive
# `schemars::JsonSchema`). The schema is materialized by
# `conduit_config::schema::write_schema` (see
# crates/conduit-config/src/schema.rs), which calls
# `schemars::schema_for!(AppConfig)` and pretty-prints it.
#
# This script runs the conduit-config schema example and writes the result to
# config.schema.json at the repo root. It then re-parses the output
# to guarantee it is valid JSON, and prints a one-line diff summary so CI can
# fail on drift.
#
# Usage:
#   scripts/generate_config_schema.sh            # write config.schema.json
#   scripts/generate_config_schema.sh --check    # exit non-zero on drift, no write
#
# Why a script and not a build.rs: the schema is a published artifact consumed
# by ops tooling and editor integrations; regenerating it should be an
# explicit, reproducible step, not a side effect of every `cargo build`.

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd -P)"
SCHEMA_PATH="${REPO_ROOT}/config.schema.json"
CHECK_ONLY=0

if [[ "${1:-}" == "--check" || "${1:-}" == "check" ]]; then
  CHECK_ONLY=1
fi

# --- 1. Generate into a temp file, then validate JSON. ---
TMP_OUT="$(mktemp -t config.schema.XXXXXX.json)"
trap 'rm -f "${TMP_OUT}"' EXIT

echo "[generate_config_schema] compiling conduit-config schema generator …"
(cd "${REPO_ROOT}" && cargo run --locked --quiet --release -p conduit-config --example generate_schema -- "${TMP_OUT}") >&2

# Re-parse to guarantee the output is valid JSON before touching the real file.
if ! python3 -c "import json,sys; json.load(open(sys.argv[1]))" "${TMP_OUT}" 2>/dev/null \
   && ! python -c "import json,sys; json.load(open(sys.argv[1]))" "${TMP_OUT}" 2>/dev/null; then
  echo "error: regenerated schema is not valid JSON (${TMP_OUT})" >&2
  exit 2
fi

if [[ "${CHECK_ONLY}" -eq 1 ]]; then
  if diff -u "${SCHEMA_PATH}" "${TMP_OUT}" >/tmp/schema-diff 2>&1; then
    echo "[generate_config_schema] OK: no drift"
    exit 0
  else
    echo "error: config.schema.json is out of date. Run scripts/generate_config_schema.sh to regenerate." >&2
    cat /tmp/schema-diff >&2
    exit 1
  fi
fi

# Replace the committed file.
cp "${TMP_OUT}" "${SCHEMA_PATH}"
echo "[generate_config_schema] wrote ${SCHEMA_PATH}"

# --- 2. Drift summary. ---
# `write_schema` is deterministic by construction (schemars schema_for! is
# order-stable), so re-running should always produce byte-identical output.
echo "[generate_config_schema] done. Validate in CI with: $0 --check"
