#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/release_artifacts.sh <lane> [--version VERSION] [--out-dir DIR]

Lanes:
  metadata      Write pinned toolchain/build metadata and provenance seed files.
  build         Build deterministic release binaries and package release artifacts.
  checksums     Recompute and verify SHA-256 checksums for packaged artifacts.
  sign          Create detached signatures for packaged artifacts.
  provenance    Write the release provenance note from metadata, checksums, and signatures.
  verify        Verify checksums and signatures for an existing artifact directory.
  dry-run       Run metadata, build, checksums, sign, provenance, and verify with CI-safe defaults.

Environment:
  RELEASE_SIGNING_KEY      Optional ASCII-armored GPG private key for real release signing.
  RELEASE_SIGNING_KEY_ID   Optional GPG key id/fingerprint to use after importing the key.
  SOURCE_DATE_EPOCH        Optional reproducible timestamp; defaults to HEAD commit time.
USAGE
}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LANE="${1:-}"
shift || true
VERSION=""
OUT_DIR="$ROOT/dist/release"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:?--version requires a value}"
      shift 2
      ;;
    --out-dir)
      OUT_DIR="${2:?--out-dir requires a value}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$LANE" ]]; then
  usage >&2
  exit 2
fi

cd "$ROOT"
mkdir -p "$OUT_DIR"

VERSION="${VERSION:-$(git describe --tags --always --dirty 2>/dev/null || git rev-parse --short HEAD)}"
COMMIT="$(git rev-parse HEAD)"
COMMIT_TIME="$(git show -s --format=%ct HEAD)"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$COMMIT_TIME}"
export SOURCE_DATE_EPOCH
ARTIFACT_PREFIX="llvm-in-rust-${VERSION}"
BINARY_NAME="llvm-ir-min"
BINARY_PATH="target/release/$BINARY_NAME"
BINARY_ARTIFACT="$OUT_DIR/${ARTIFACT_PREFIX}-${BINARY_NAME}-${RUNNER_OS:-local}-${RUNNER_ARCH:-$(uname -m)}"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

write_metadata() {
  echo "[release] writing pinned metadata"
  rustc +stable --version --verbose > "$OUT_DIR/rustc-stable-version.txt"
  cargo +stable --version --verbose > "$OUT_DIR/cargo-stable-version.txt"
  cat > "$OUT_DIR/release-metadata.json" <<JSON
{
  "version": "$VERSION",
  "commit": "$COMMIT",
  "source_date_epoch": $SOURCE_DATE_EPOCH,
  "rust_toolchain": "stable",
  "cargo_lock": "$(sha256_file Cargo.lock)",
  "profile": "release",
  "packages": ["llvm-in-rust"]
}
JSON
}

build_artifacts() {
  echo "[release] building deterministic release artifact"
  write_metadata
  cargo +stable build --locked --release -p llvm-in-rust --bin "$BINARY_NAME"
  cp "$BINARY_PATH" "$BINARY_ARTIFACT"
  chmod 0755 "$BINARY_ARTIFACT"
  # Normalize archive metadata for reproducible rebuilds.
  tar --sort=name \
    --mtime="@$SOURCE_DATE_EPOCH" \
    --owner=0 --group=0 --numeric-owner \
    -czf "$OUT_DIR/${ARTIFACT_PREFIX}-source.tar.gz" \
    Cargo.lock Cargo.toml README.md docs src scripts
}

write_checksums() {
  echo "[release] writing checksums"
  : > "$OUT_DIR/SHA256SUMS"
  find "$OUT_DIR" -maxdepth 1 -type f \
    ! -name SHA256SUMS \
    ! -name '*.sig' \
    ! -name 'RELEASE_PROVENANCE.md' \
    -print | sort | while read -r file; do
      printf '%s  %s\n' "$(sha256_file "$file")" "$(basename "$file")" >> "$OUT_DIR/SHA256SUMS"
    done
  verify_checksums
}

ensure_gpg_key() {
  if ! command -v gpg >/dev/null 2>&1; then
    echo "gpg is required for release signing" >&2
    exit 1
  fi

  export GNUPGHOME="${GNUPGHOME:-${RUNNER_TEMP:-/tmp}/llvm-in-rust-release-gnupg}"
  mkdir -p "$GNUPGHOME"
  chmod 0700 "$GNUPGHOME"

  if [[ -n "${RELEASE_SIGNING_KEY:-}" ]]; then
    printf '%s\n' "$RELEASE_SIGNING_KEY" | gpg --batch --import
    return
  fi

  if ! gpg --batch --list-secret-keys release-dry-run@example.invalid >/dev/null 2>&1; then
    cat > "$GNUPGHOME/gpg-batch.conf" <<'GPG'
Key-Type: eddsa
Key-Curve: ed25519
Name-Real: LLVM-in-Rust release dry run
Name-Email: release-dry-run@example.invalid
Expire-Date: 1d
%no-protection
%commit
GPG
    gpg --batch --generate-key "$GNUPGHOME/gpg-batch.conf"
  fi
  RELEASE_SIGNING_KEY_ID="${RELEASE_SIGNING_KEY_ID:-release-dry-run@example.invalid}"
}

sign_artifacts() {
  echo "[release] signing artifacts"
  [[ -f "$OUT_DIR/SHA256SUMS" ]] || write_checksums
  ensure_gpg_key
  (cd "$OUT_DIR" && while read -r _checksum file; do
    [[ -n "${file:-}" ]] || continue
    signer=()
    if [[ -n "${RELEASE_SIGNING_KEY_ID:-}" ]]; then
      signer=(--local-user "$RELEASE_SIGNING_KEY_ID")
    fi
    gpg --batch --yes --armor "${signer[@]}" --output "$file.sig" --detach-sign "$file"
  done < SHA256SUMS)
}

write_provenance() {
  echo "[release] writing provenance note"
  [[ -f "$OUT_DIR/SHA256SUMS" ]] || write_checksums
  cat > "$OUT_DIR/RELEASE_PROVENANCE.md" <<MD
# LLVM-in-Rust release provenance

- Version: \`$VERSION\`
- Commit: \`$COMMIT\`
- Source date epoch: \`$SOURCE_DATE_EPOCH\`
- Toolchain: stable Rust (see \`rustc-stable-version.txt\` and \`cargo-stable-version.txt\`)
- Build profile: \`release\`
- Artifact workflow: \`Release Artifact Provenance\`
- Checksum manifest: \`SHA256SUMS\`
- Signatures: detached ASCII-armored GPG signatures (\`*.sig\`)

## Verification

\`\`\`bash
scripts/release_artifacts.sh verify --out-dir "$OUT_DIR"
\`\`\`

## Checksums

\`\`\`
$(cat "$OUT_DIR/SHA256SUMS")
\`\`\`
MD
}

verify_checksums() {
  echo "[release] verifying checksums"
  (cd "$OUT_DIR" && while read -r checksum file; do
    [[ -n "${checksum:-}" && -n "${file:-}" ]] || continue
    actual="$(sha256_file "$file")"
    if [[ "$actual" != "$checksum" ]]; then
      echo "checksum mismatch for $file: expected $checksum got $actual" >&2
      exit 1
    fi
  done < SHA256SUMS)
}

verify_signatures() {
  echo "[release] verifying signatures"
  if compgen -G "$OUT_DIR/*.sig" >/dev/null; then
    for sig in "$OUT_DIR"/*.sig; do
      gpg --batch --verify "$sig" "${sig%.sig}"
    done
  else
    echo "no signatures found in $OUT_DIR" >&2
    exit 1
  fi
}

case "$LANE" in
  metadata)
    write_metadata
    ;;
  build)
    build_artifacts
    ;;
  checksums)
    write_checksums
    ;;
  sign)
    sign_artifacts
    ;;
  provenance)
    write_provenance
    ;;
  verify)
    verify_checksums
    verify_signatures
    ;;
  dry-run)
    build_artifacts
    write_checksums
    sign_artifacts
    write_provenance
    verify_checksums
    verify_signatures
    ;;
  *)
    echo "unknown lane: $LANE" >&2
    usage >&2
    exit 2
    ;;
esac
