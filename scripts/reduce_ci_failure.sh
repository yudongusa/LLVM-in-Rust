#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/reduce_ci_failure.sh --input <failing.ll> --predicate <cmd with {{input}}> --evidence-dir <dir>

Creates a crash triage evidence package with original.ll, minimized.ll,
reducer.log, repro.sh, manifest.txt, and bucket-signature.txt. The predicate
must exit non-zero when the failure reproduces.
USAGE
}

INPUT=""
PREDICATE=""
EVIDENCE_DIR="ci-failures/reducer"
COMPONENT="llvm-in-rust-ir"
FAILURE_KIND="crash"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --input) INPUT="${2:?missing --input value}"; shift 2 ;;
    --predicate) PREDICATE="${2:?missing --predicate value}"; shift 2 ;;
    --evidence-dir) EVIDENCE_DIR="${2:?missing --evidence-dir value}"; shift 2 ;;
    --component) COMPONENT="${2:?missing --component value}"; shift 2 ;;
    --failure-kind) FAILURE_KIND="${2:?missing --failure-kind value}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

if [[ -z "$INPUT" || -z "$PREDICATE" ]]; then
  usage
  exit 2
fi

if [[ ! -f "$INPUT" ]]; then
  echo "input does not exist: $INPUT" >&2
  exit 1
fi

mkdir -p "$EVIDENCE_DIR"

if command -v sha256sum >/dev/null 2>&1; then
  HASH=$(sha256sum "$INPUT" | awk '{print $1}')
else
  HASH=$(shasum -a 256 "$INPUT" | awk '{print $1}')
fi
FIRST_LINE=$(grep -m1 -E '^(error|panic|thread|LLVM ERROR|AddressSanitizer|UndefinedBehaviorSanitizer|SUMMARY):' "$INPUT" || true)
FIRST_LINE=${FIRST_LINE:-$(basename "$INPUT")}
BUCKET_SIGNATURE="${COMPONENT}:${FAILURE_KIND}:${FIRST_LINE}:${HASH:0:12}"
printf '%s\n' "$BUCKET_SIGNATURE" > "$EVIDENCE_DIR/bucket-signature.txt"

cargo +stable run -p llvm-in-rust --bin llvm-ir-min -- \
  --input "$INPUT" \
  --predicate "$PREDICATE" \
  --evidence-dir "$EVIDENCE_DIR" \
  --bucket-signature "$BUCKET_SIGNATURE"

cat > "$EVIDENCE_DIR/README.md" <<EOF
# Crash triage evidence package

- Bucket signature: \`$BUCKET_SIGNATURE\`
- Original input: \`original.ll\`
- Minimized reproducer: \`minimized.ll\`
- Reproduction script: \`repro.sh\`
- Reducer log: \`reducer.log\`
EOF

echo "wrote evidence package: $EVIDENCE_DIR"
