#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-ci-failures}"
OUT="${2:-crash-buckets.md}"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

{
  echo "# Crash bucket summary"
  echo
  echo "Generated from \`$ROOT\`."
  echo

  if [[ ! -d "$ROOT" ]]; then
    echo "No crash evidence directory found."
    exit 0
  fi

  find "$ROOT" -name bucket-signature.txt -type f -print | sort > "$TMP"
  if [[ ! -s "$TMP" ]]; then
    echo "No bucket signatures found."
    exit 0
  fi

  echo "| Bucket signature | Evidence path |"
  echo "|---|---|"
  while IFS= read -r sig_file; do
    sig=$(tr '\n' ' ' < "$sig_file" | sed 's/[[:space:]]*$//')
    dir=$(dirname "$sig_file")
    echo "| \`$sig\` | \`$dir\` |"
  done < "$TMP"
} > "$OUT"

cat "$OUT"
