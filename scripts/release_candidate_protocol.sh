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

echo "release candidate protocol is complete"
