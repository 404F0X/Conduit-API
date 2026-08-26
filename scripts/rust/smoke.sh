#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
CARGO_BIN="${CARGO:-cargo}"

is_enabled() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    *) return 1 ;;
  esac
}

run_step() {
  local label="$1"
  shift

  echo "==> ${label}"
  "$@"
}

usage() {
  cat <<'USAGE'
Run the local Rust workspace smoke checks.

Usage:
  bash scripts/rust/smoke.sh

Environment:
  CONDUIT_SMOKE_SKIP_METADATA=1  Skip cargo metadata.
  CONDUIT_SMOKE_SKIP_FMT=1       Skip cargo fmt --check.
  CONDUIT_SMOKE_SKIP_CHECK=1     Skip cargo check.
  CONDUIT_SMOKE_SKIP_TESTS=1     Skip cargo test package checks.

  CONDUIT_SMOKE_CHECK_ARGS       Override cargo check args.
                                  Default: --workspace --all-targets
  CONDUIT_SMOKE_TEST_PACKAGES    Space-separated packages to test.
                                  Default: conduit-core
  CONDUIT_SMOKE_EXTRA_TEST_ARGS  Extra args appended to every cargo test.
  CARGO                           Cargo executable to run when cargo is not
                                  available on PATH.

Examples:
  CONDUIT_SMOKE_SKIP_CHECK=1 bash scripts/rust/smoke.sh
  CONDUIT_SMOKE_TEST_PACKAGES="conduit-core conduit-config" bash scripts/rust/smoke.sh
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

cd "${ROOT_DIR}"

if ! is_enabled "${CONDUIT_SMOKE_SKIP_METADATA:-0}"; then
  run_step "cargo metadata" "${CARGO_BIN}" metadata --no-deps --format-version 1
else
  echo "==> skipping cargo metadata"
fi

if ! is_enabled "${CONDUIT_SMOKE_SKIP_FMT:-0}"; then
  run_step "cargo fmt --check" "${CARGO_BIN}" fmt --check
else
  echo "==> skipping cargo fmt --check"
fi

if ! is_enabled "${CONDUIT_SMOKE_SKIP_CHECK:-0}"; then
  read -r -a check_args <<< "${CONDUIT_SMOKE_CHECK_ARGS:---workspace --all-targets}"
  run_step "cargo check ${check_args[*]}" "${CARGO_BIN}" check "${check_args[@]}"
else
  echo "==> skipping cargo check"
fi

if ! is_enabled "${CONDUIT_SMOKE_SKIP_TESTS:-0}"; then
  read -r -a test_packages <<< "${CONDUIT_SMOKE_TEST_PACKAGES:-conduit-core}"
  read -r -a extra_test_args <<< "${CONDUIT_SMOKE_EXTRA_TEST_ARGS:-}"

  for package in "${test_packages[@]}"; do
    if [[ -z "${package}" ]]; then
      continue
    fi
    run_step "cargo test -p ${package}" "${CARGO_BIN}" test -p "${package}" "${extra_test_args[@]}"
  done
else
  echo "==> skipping cargo test package checks"
fi

echo "rust smoke checks passed"
