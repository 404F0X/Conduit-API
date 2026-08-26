#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIGRATIONS_DIR="${ROOT_DIR}/migrations"
INVENTORY_FILE="${MIGRATIONS_DIR}/INVENTORY.md"

if [[ ! -d "${MIGRATIONS_DIR}" ]]; then
  echo "missing migrations directory: ${MIGRATIONS_DIR}" >&2
  exit 1
fi

if [[ ! -f "${INVENTORY_FILE}" ]]; then
  echo "missing migration inventory: ${INVENTORY_FILE}" >&2
  exit 1
fi

while IFS= read -r -d '' backend_dir; do
  backend="$(basename "${backend_dir}")"
  if [[ "${backend}" != "postgres" ]]; then
    echo "unsupported migration backend directory: ${backend_dir}" >&2
    echo "PostgreSQL is the only supported database backend" >&2
    exit 1
  fi
done < <(find "${MIGRATIONS_DIR}" -mindepth 1 -maxdepth 1 -type d -print0)

dir="${MIGRATIONS_DIR}/postgres"
readme="${dir}/README.md"

if [[ ! -d "${dir}" ]]; then
  echo "missing PostgreSQL migration directory: ${dir}" >&2
  exit 1
fi

if [[ ! -f "${readme}" ]]; then
  echo "missing PostgreSQL migration README: ${readme}" >&2
  exit 1
fi

while IFS= read -r -d '' sql_file; do
  file_name="$(basename "${sql_file}")"
  if [[ ! "${file_name}" =~ ^[0-9]{6}_.+\.sql$ ]]; then
    echo "invalid migration filename: ${sql_file}" >&2
    echo "expected pattern: 000001_descriptive_name.sql" >&2
    exit 1
  fi

  if [[ "${file_name}" =~ (placeholder|template|example|sample|draft) ]]; then
    echo "placeholder/template SQL file would be treated as a real migration: ${sql_file}" >&2
    echo "keep templates in README/Markdown files so sqlx migrate cannot execute them" >&2
    exit 1
  fi

  if grep -Eiq '(placeholder|template only|todo|replace me|not implemented|fake schema)' "${sql_file}"; then
    echo "SQL migration contains placeholder text and cannot be counted as complete: ${sql_file}" >&2
    exit 1
  fi
done < <(find "${dir}" -maxdepth 1 -type f -name '*.sql' -print0)

echo "PostgreSQL migration layout ok"
