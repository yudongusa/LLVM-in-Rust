#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOC="$ROOT/docs/production_operations.md"
README="$ROOT/README.md"

require_in_doc() {
  local needle="$1"
  if ! grep -Fq -- "$needle" "$DOC"; then
    echo "production operations guide missing required text: $needle" >&2
    exit 1
  fi
}

require_in_readme() {
  local needle="$1"
  if ! grep -Fq -- "$needle" "$README"; then
    echo "README missing production operations link/text: $needle" >&2
    exit 1
  fi
}

[[ -f "$DOC" ]] || { echo "missing $DOC" >&2; exit 1; }

require_in_doc "## Build and validation quick start"
require_in_doc "## Observability checklist"
require_in_doc "## Incident response: start to resolution"
require_in_doc "## Contributor triage paths"
require_in_doc "## FAQ: common integration failures"
require_in_doc "## Runbook index"
require_in_doc "scripts/reduce_ci_failure.sh"
require_in_doc "scripts/release_artifacts.sh verify"
require_in_doc "docs/release_candidate_protocol.md"
require_in_doc "docs/crash_triage_runbook.md"
require_in_readme "docs/production_operations.md"

echo "production operations docs are complete"
