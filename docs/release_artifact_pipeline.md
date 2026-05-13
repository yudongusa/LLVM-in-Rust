# Release artifact pipeline

Issue #184 tracks the release-engineering gate for reproducible, verifiable release artifacts.

## Build profile and pinned metadata

Release candidates and stable tags use the `Release Artifact Provenance` workflow and
`scripts/release_artifacts.sh`.

The release lane records:

- exact Git commit (`git rev-parse HEAD`)
- commit-derived `SOURCE_DATE_EPOCH` unless explicitly overridden
- stable Rust and Cargo verbose versions
- `Cargo.lock` SHA-256 checksum
- `cargo build --locked --release -p llvm-in-rust --bin llvm-ir-min`
- release provenance in `RELEASE_PROVENANCE.md`

The script intentionally builds with `--locked` so releases cannot silently drift from the
checked-in dependency graph.

## Reproducible artifact contract

The artifact bundle contains:

- `llvm-in-rust-<version>-llvm-ir-min-<os>-<arch>` — release binary
- `llvm-in-rust-<version>-source.tar.gz` — normalized source archive
- `release-metadata.json` — pinned release inputs
- `rustc-stable-version.txt` and `cargo-stable-version.txt`
- `SHA256SUMS` — SHA-256 manifest for every published file
- `*.sig` — detached ASCII-armored GPG signatures
- `RELEASE_PROVENANCE.md` — release-note provenance block

Archive metadata is normalized with sorted paths, fixed owner/group, and
`SOURCE_DATE_EPOCH` so independent rebuilds can compare checksums.

## Signing

Real releases should configure these repository secrets:

- `RELEASE_SIGNING_KEY`: ASCII-armored GPG private key
- `RELEASE_SIGNING_KEY_ID`: signing key fingerprint or key id

Pull requests and CI dry-runs use an ephemeral one-day GPG key. That proves the signing and
verification path works without publishing a trusted release signature from untrusted code.

## Release procedure

1. Ensure all M1/M2/M3 release gates are green for the target ref.
2. Run `Release Artifact Provenance` with the intended ref/version, or let it run on the tag.
3. Download the uploaded `llvm-in-rust-release-artifacts-*` bundle.
4. Verify the bundle:

   ```bash
   scripts/release_artifacts.sh verify --out-dir dist/release
   ```

5. Independently rebuild from the same commit with the same stable toolchain metadata and
   compare `SHA256SUMS`.
6. Paste the `RELEASE_PROVENANCE.md` block into the release notes.
7. Publish artifacts, `SHA256SUMS`, and detached signatures together.

## Rollback trigger

Do not promote a release if any of these are true:

- rebuild checksums differ without a documented accepted explanation
- any artifact lacks a detached signature
- `cargo build --locked --release` fails
- provenance omits commit, toolchain, checksum, or matrix references
- a Tier-1 release gate is red or waived without maintainer sign-off
