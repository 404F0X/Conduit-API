#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
output="${repo_root}/LICENSES/RUST_THIRD_PARTY_LICENSES.html"
mode="${1:---check}"

case "${mode}" in
  --check | --write) ;;
  *)
    echo "usage: $0 [--check|--write]" >&2
    exit 2
    ;;
esac

tool_version="0.9.2"
tool_archive="cargo-about-${tool_version}-x86_64-unknown-linux-musl.tar.gz"
tool_sha256="9099a59e820c38a68b9d65f300662a567d56562f9a10f6aa4c7e86c17c2566af"
temporary_dir="$(mktemp -d)"
trap 'rm -rf "${temporary_dir}"' EXIT

if command -v cargo-about >/dev/null 2>&1; then
  if [[ "$(cargo-about --version)" != "cargo-about ${tool_version}" ]]; then
    echo "cargo-about ${tool_version} is required" >&2
    exit 1
  fi
  tool=(cargo-about)
else
  if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
    echo "install cargo-about ${tool_version} before running this script on this platform" >&2
    exit 1
  fi
  curl --fail --location --proto '=https' --proto-redir '=https' \
    --silent --show-error --tlsv1.2 \
    "https://github.com/EmbarkStudios/cargo-about/releases/download/${tool_version}/${tool_archive}" \
    --output "${temporary_dir}/${tool_archive}"
  printf '%s  %s\n' "${tool_sha256}" "${temporary_dir}/${tool_archive}" | sha256sum --check --status
  tar -C "${temporary_dir}" -xzf "${temporary_dir}/${tool_archive}"
  tool=("${temporary_dir}/cargo-about-${tool_version}-x86_64-unknown-linux-musl/cargo-about")
fi

generated="${temporary_dir}/RUST_THIRD_PARTY_LICENSES.html"
"${tool[@]}" \
  generate \
  --config "${repo_root}/about.toml" \
  --manifest-path "${repo_root}/crates/conduit-bin/Cargo.toml" \
  --features release-binary \
  --locked \
  --fail \
  --output-file "${generated}" \
  "${repo_root}/scripts/licenses/rust-third-party.hbs"
sed -e 's/\r$//' -e 's/[[:blank:]]\+$//' \
  <"${generated}" >"${generated}.normalized"
mv "${generated}.normalized" "${generated}"

if [[ "${mode}" == "--write" ]]; then
  cp "${generated}" "${output}"
  exit 0
fi

tracked="${temporary_dir}/tracked-RUST_THIRD_PARTY_LICENSES.html"
sed -e 's/\r$//' -e 's/[[:blank:]]\+$//' <"${output}" >"${tracked}"
if ! cmp --silent "${generated}" "${tracked}"; then
  echo "${output#"${repo_root}/"} is stale; run scripts/licenses/check-rust-third-party.sh --write" >&2
  diff --unified "${output}" "${generated}" || true
  exit 1
fi
