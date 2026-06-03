#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/rc_evidence_bundle.sh --version VERSION --rc N --commit SHA --release-owner @handle [options]

Required options:
  --version VERSION                  Candidate version, for example 0.1.0
  --rc N                             RC number, for example 1
  --commit SHA                       Exact candidate commit
  --release-owner @handle            Release owner to record in the checklist

Evidence options, each must be a GitHub URL or waiver:<reason>:
  --artifact-run VALUE               Release Artifact Provenance run
  --quality-run VALUE                Quality Gates run
  --platform-run VALUE               Platform Matrix Gate run
  --interoperability-run VALUE       Interoperability Conformance Gate run
  --differential-run VALUE           Differential Tests (LLVM 19) run
  --golden-run VALUE                 Golden Codegen Gate run
  --sanitizer-run VALUE              Sanitizer and UB hardening run
  --fuzz-run VALUE                   Fuzzing (LLVM-Stress + CSmith) run
  --fuzz-differential-run VALUE      fuzz-differential run
  --performance-run VALUE            Performance Budget Gate run
  --docs-run VALUE                   Production Operations Docs run
  --pilot-summary VALUE              Pilot workload summary or waiver:<reason>

Optional:
  --burn-in-start YYYY-MM-DD         Burn-in start date (UTC)
  --burn-in-end YYYY-MM-DD           Burn-in target/end date (UTC)
  --provenance-checksum SHA256       Provenance bundle checksum
  --output FILE                      Write markdown to FILE instead of stdout

The generated markdown is intended for the Milestone Z (#385) issue and the
roadmap #93 go/no-go comment.
USAGE
}

VERSION=""
RC=""
COMMIT=""
RELEASE_OWNER=""
ARTIFACT_RUN=""
QUALITY_RUN=""
PLATFORM_RUN=""
INTEROP_RUN=""
DIFFERENTIAL_RUN=""
GOLDEN_RUN=""
SANITIZER_RUN=""
FUZZ_RUN=""
FUZZ_DIFFERENTIAL_RUN=""
PERFORMANCE_RUN=""
DOCS_RUN=""
PILOT_SUMMARY=""
BURN_IN_START=""
BURN_IN_END=""
PROVENANCE_CHECKSUM=""
OUTPUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:?--version requires a value}"; shift 2 ;;
    --rc) RC="${2:?--rc requires a value}"; shift 2 ;;
    --commit) COMMIT="${2:?--commit requires a value}"; shift 2 ;;
    --release-owner) RELEASE_OWNER="${2:?--release-owner requires a value}"; shift 2 ;;
    --artifact-run) ARTIFACT_RUN="${2:?--artifact-run requires a value}"; shift 2 ;;
    --quality-run) QUALITY_RUN="${2:?--quality-run requires a value}"; shift 2 ;;
    --platform-run) PLATFORM_RUN="${2:?--platform-run requires a value}"; shift 2 ;;
    --interoperability-run) INTEROP_RUN="${2:?--interoperability-run requires a value}"; shift 2 ;;
    --differential-run) DIFFERENTIAL_RUN="${2:?--differential-run requires a value}"; shift 2 ;;
    --golden-run) GOLDEN_RUN="${2:?--golden-run requires a value}"; shift 2 ;;
    --sanitizer-run) SANITIZER_RUN="${2:?--sanitizer-run requires a value}"; shift 2 ;;
    --fuzz-run) FUZZ_RUN="${2:?--fuzz-run requires a value}"; shift 2 ;;
    --fuzz-differential-run) FUZZ_DIFFERENTIAL_RUN="${2:?--fuzz-differential-run requires a value}"; shift 2 ;;
    --performance-run) PERFORMANCE_RUN="${2:?--performance-run requires a value}"; shift 2 ;;
    --docs-run) DOCS_RUN="${2:?--docs-run requires a value}"; shift 2 ;;
    --pilot-summary) PILOT_SUMMARY="${2:?--pilot-summary requires a value}"; shift 2 ;;
    --burn-in-start) BURN_IN_START="${2:?--burn-in-start requires a value}"; shift 2 ;;
    --burn-in-end) BURN_IN_END="${2:?--burn-in-end requires a value}"; shift 2 ;;
    --provenance-checksum) PROVENANCE_CHECKSUM="${2:?--provenance-checksum requires a value}"; shift 2 ;;
    --output) OUTPUT="${2:?--output requires a value}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

require_non_empty() {
  local name="$1"
  local value="$2"
  if [[ -z "$value" ]]; then
    echo "missing required option: $name" >&2
    exit 2
  fi
}

require_evidence() {
  local name="$1"
  local value="$2"
  require_non_empty "$name" "$value"
  if [[ "$value" != https://github.com/* && "$value" != waiver:* ]]; then
    echo "$name must be a GitHub URL or waiver:<reason>" >&2
    exit 2
  fi
}

require_date() {
  local name="$1"
  local value="$2"
  [[ -z "$value" || "$value" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || {
    echo "$name must use YYYY-MM-DD" >&2
    exit 2
  }
}

require_non_empty "--version" "$VERSION"
require_non_empty "--rc" "$RC"
require_non_empty "--commit" "$COMMIT"
require_non_empty "--release-owner" "$RELEASE_OWNER"
[[ "$COMMIT" =~ ^[0-9a-fA-F]{7,40}$ ]] || {
  echo "--commit must be a 7-40 character hex SHA" >&2
  exit 2
}
[[ "$RC" =~ ^[0-9]+$ ]] || {
  echo "--rc must be numeric" >&2
  exit 2
}
[[ "$RELEASE_OWNER" == @* ]] || {
  echo "--release-owner must be a Slock/GitHub-style @handle" >&2
  exit 2
}

require_evidence "--artifact-run" "$ARTIFACT_RUN"
require_evidence "--quality-run" "$QUALITY_RUN"
require_evidence "--platform-run" "$PLATFORM_RUN"
require_evidence "--interoperability-run" "$INTEROP_RUN"
require_evidence "--differential-run" "$DIFFERENTIAL_RUN"
require_evidence "--golden-run" "$GOLDEN_RUN"
require_evidence "--sanitizer-run" "$SANITIZER_RUN"
require_evidence "--fuzz-run" "$FUZZ_RUN"
require_evidence "--fuzz-differential-run" "$FUZZ_DIFFERENTIAL_RUN"
require_evidence "--performance-run" "$PERFORMANCE_RUN"
require_evidence "--docs-run" "$DOCS_RUN"
require_evidence "--pilot-summary" "$PILOT_SUMMARY"
require_date "--burn-in-start" "$BURN_IN_START"
require_date "--burn-in-end" "$BURN_IN_END"

DRY_RUN_DATE="$(date -u +%F)"
RC_TAG="rc-${VERSION}-${RC}"
PROVENANCE_CHECKSUM="${PROVENANCE_CHECKSUM:-pending}"
BURN_IN_START="${BURN_IN_START:-pending}"
BURN_IN_END="${BURN_IN_END:-pending}"

render() {
  cat <<MD
# Milestone Z RC evidence: ${VERSION} RC${RC}

- RC tag: \`${RC_TAG}\`
- Candidate commit: \`${COMMIT}\`
- Release owner: ${RELEASE_OWNER}
- Artifact workflow run: ${ARTIFACT_RUN}
- Provenance bundle checksum: \`${PROVENANCE_CHECKSUM}\`
- Dry-run date: ${DRY_RUN_DATE}
- Burn-in window: ${BURN_IN_START} to ${BURN_IN_END}

## Required gates

- [ ] Release artifact dry-run passed for the candidate commit: ${ARTIFACT_RUN}
- [ ] Quality gates passed for the candidate commit: ${QUALITY_RUN}
- [ ] Tier-1 host/target platform matrix passed: ${PLATFORM_RUN}
- [ ] Interoperability conformance passed: ${INTEROP_RUN}
- [ ] Differential tests against LLVM 19 passed: ${DIFFERENTIAL_RUN}
- [ ] Golden codegen gate passed or approved baseline update linked: ${GOLDEN_RUN}
- [ ] Sanitizer/UB gates passed or waiver linked: ${SANITIZER_RUN}
- [ ] LLVM-Stress/CSmith fuzzing passed or waiver linked: ${FUZZ_RUN}
- [ ] Differential fuzzing passed or waiver linked: ${FUZZ_DIFFERENTIAL_RUN}
- [ ] Performance budget passed or accepted-regression rationale linked: ${PERFORMANCE_RUN}
- [ ] Production operations/docs validation passed: ${DOCS_RUN}
- [ ] Production pilot completed with upstream LLVM fallback and comparison: ${PILOT_SUMMARY}
- [ ] No unresolved release-blocking issues remain
- [ ] Rollback playbook dry-run completed

## Decision

- [ ] GO — promote this candidate to \`v${VERSION}\`
- [ ] NO-GO — do not promote; reason and next action below

Decision notes:
MD
}

if [[ -n "$OUTPUT" ]]; then
  render > "$OUTPUT"
else
  render
fi
