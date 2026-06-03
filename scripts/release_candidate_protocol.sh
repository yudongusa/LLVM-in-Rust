#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOC="$ROOT/docs/release_candidate_protocol.md"

require() {
  local needle="$1"
  if ! grep -Fq -- "$needle" "$DOC"; then
    echo "release candidate protocol missing required text: $needle" >&2
    exit 1
  fi
}

[[ -f "$DOC" ]] || { echo "missing $DOC" >&2; exit 1; }

require "## Cadence"
require "## Promotion criteria"
require "## Go/no-go checklist template"
require "## Rollback triggers"
require "## Rollback playbook"
require "## Communication plan"
require "## Dry-run record"
require "GO — promote this candidate"
require "NO-GO — do not promote"
require "Release Artifact Provenance"
require "rollback advisory"
require 'Stable tags require an explicit `GO` decision'
require "scripts/rc_evidence_bundle.sh"

TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT
"$ROOT/scripts/rc_evidence_bundle.sh" \
  --version 0.1.0 \
  --rc 1 \
  --commit 0123456789abcdef \
  --release-owner @release-owner \
  --artifact-run https://github.com/example/repo/actions/runs/1 \
  --quality-run https://github.com/example/repo/actions/runs/2 \
  --platform-run https://github.com/example/repo/actions/runs/3 \
  --interoperability-run https://github.com/example/repo/actions/runs/4 \
  --differential-run https://github.com/example/repo/actions/runs/5 \
  --golden-run https://github.com/example/repo/actions/runs/6 \
  --sanitizer-run https://github.com/example/repo/actions/runs/7 \
  --fuzz-run https://github.com/example/repo/actions/runs/8 \
  --fuzz-differential-run https://github.com/example/repo/actions/runs/9 \
  --performance-run https://github.com/example/repo/actions/runs/10 \
  --docs-run https://github.com/example/repo/actions/runs/11 \
  --pilot-summary waiver:example-pilot-not-run \
  --burn-in-start 2026-06-03 \
  --burn-in-end 2026-06-05 \
  --output "$TMP"

grep -Fq "Milestone Z RC evidence" "$TMP"
grep -Fq "Production pilot completed" "$TMP"

echo "release candidate protocol is complete"
