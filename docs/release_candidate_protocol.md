# Release candidate protocol

Issue #185 tracks the auditable release-candidate decision process for LLVM-in-Rust.

## Cadence

- Cut an RC when all open issues in the target release milestone are closed or explicitly deferred.
- Use tags named `rc-<major>.<minor>.<patch>-<n>` for candidates and `v<major>.<minor>.<patch>` for stable releases.
- Keep at least two business days between the first RC and stable promotion unless a maintainer records an emergency-release exception.
- Restart the RC counter after any release-blocking change lands.

## Promotion criteria

A release candidate can be promoted only when every item below has an explicit `go` or documented waiver:

1. `Release Artifact Provenance` dry-run completed for the exact RC commit.
2. Tier-1 host matrix is green on Ubuntu, macOS, and Windows.
3. Tier-1 artifact generation is green for x86_64, AArch64, and RV64GC.
4. Interoperability conformance lanes are green (`link`, `debug`, `abi`, `mixed`).
5. Differential tests against LLVM 19 are green.
6. Golden codegen checksums are locked or the baseline update has maintainer approval.
7. Sanitizer/UB gates are green or a maintainer-approved scheduled-only waiver is recorded.
8. Performance budget is green or a `perf-regression-accepted` rationale is linked.
9. No open `bug`, `crash`, `miscompilation`, or `release-blocker` issue targets the milestone.
10. Release notes include the provenance block, known issues, rollback window, and support contact.

Stable tags require an explicit `GO` decision from the release owner in the RC checklist issue or PR.

## Go/no-go checklist template

Create one tracking issue per candidate and paste this template:

```markdown
# RC checklist: <version> RC<n>

- RC tag: `rc-<version>-<n>`
- Candidate commit: `<sha>`
- Release owner: `@<handle>`
- Artifact workflow run: <url>
- Provenance bundle checksum: `<sha256>`
- Dry-run date: <YYYY-MM-DD>

## Required gates

- [ ] Release artifact dry-run passed for the candidate commit
- [ ] Tier-1 host matrix passed (Ubuntu/macOS/Windows)
- [ ] Tier-1 target artifact generation passed (x86_64/AArch64/RV64GC)
- [ ] Interoperability conformance passed (link/debug/abi/mixed)
- [ ] Differential tests against LLVM 19 passed
- [ ] Golden codegen gate passed or approved baseline update linked
- [ ] Sanitizer/UB gates passed or waiver linked
- [ ] Performance budget passed or accepted-regression rationale linked
- [ ] No unresolved release-blocking issues remain
- [ ] Release notes include provenance, known issues, rollback window, and support contact
- [ ] Rollback playbook dry-run completed

## Decision

- [ ] GO — promote this candidate to `v<version>`
- [ ] NO-GO — do not promote; reason and next action below

Decision notes:
```

For Milestone Z evidence bundles, generate the checklist comment with:

```bash
scripts/rc_evidence_bundle.sh \
  --version 0.1.0 \
  --rc 1 \
  --commit <candidate-sha> \
  --release-owner @<handle> \
  --artifact-run <url> \
  --quality-run <url> \
  --platform-run <url> \
  --interoperability-run <url> \
  --differential-run <url> \
  --golden-run <url> \
  --sanitizer-run <url-or-waiver:reason> \
  --fuzz-run <url-or-waiver:reason> \
  --fuzz-differential-run <url-or-waiver:reason> \
  --performance-run <url-or-waiver:reason> \
  --docs-run <url> \
  --pilot-summary <url-or-waiver:reason>
```

Every URL should point at the exact candidate commit.  When a lane cannot run
for that commit, use a `waiver:<reason>` value and explain the follow-up in the
RC issue before any `GO` decision.

## Rollback triggers

Rollback or yank the release if any of these occur inside the rollback window:

- published artifact checksum differs from the recorded `SHA256SUMS`
- detached signature verification fails for any published artifact
- an RC/stable tag points at the wrong commit
- a Tier-1 platform cannot install or run the published binary
- a confirmed regression causes parser crashes, wrong-code, or incompatible `.ll` emission on supported LLVM versions
- provenance metadata omits commit, toolchain, Cargo.lock checksum, or artifact workflow link
- release notes omit a known severe issue that existed before promotion

## Rollback playbook

1. Announce `NO-GO` or rollback intent in the RC checklist issue with the trigger and impact.
2. Stop promotion: do not create the stable tag, or delete the draft release if it is unpublished.
3. If a stable release is already published:
   - mark the GitHub release as pre-release or append `RETRACTED` to the release notes,
   - remove promoted download links from project docs,
   - publish a rollback advisory that points users to the previous stable tag,
   - keep artifacts available for forensics unless they expose credentials or malicious payloads.
4. Open or update a blocking issue with the evidence package and owner.
5. Land the fix through normal PR review and CI.
6. Cut a new RC tag from the fixed commit and restart the checklist.

## Communication plan

- Primary channel: GitHub release/checklist issue comments.
- Maintainer escalation: mention the release owner and issue owner directly.
- User-facing update: GitHub release notes plus README release section when a stable tag is affected.
- Every rollback notice must include impact, affected versions, workaround, fix owner, and next expected update time.

## Dry-run record

The protocol was dry-run against commit `7b82ebf` while closing issue #185:

- checklist template exercised with the M3 release gates above
- rollback trigger table reviewed against the release artifact pipeline from issue #184
- rollback playbook walked through for the scenario: signed artifact checksum mismatch after draft publication
- result: `GO` for protocol adoption; no stable release promoted by this documentation dry-run
