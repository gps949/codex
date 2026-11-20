# Fork 0.170.0 release readiness

The user authorized completing the audit findings and optimizations, validating them, and publishing a new fork release. The existing integration source already contains official rust-v0.170.0 and its CI passed. This work makes the release deliverable complete.

## Approach

Use the upstream canonical package layout and source-pinned native Voice build recipes, retaining the fork's own executable and release channel. A flat bundle cannot expose the new runtime. Reusing unverified native binaries or relabeling upstream receipts would lose build provenance, so build matching helper/runtime inputs and validate moved packages on each release runner.

Keep the installer entry command and CODEX_INSTALL_DIR. Support old flat archives and canonical packages; preserve integrity validation, serialized atomic activation, prior installation recovery and all Voice resources. The default install must keep helper discovery valid through the physical executable location.

Broaden existing CI selection for request identity, runtime authentication, model catalogs, install-context and daemon changes; run actual archive/layout checks in release. Do not spend real account quota or reset credits for verification.

Make default model roles robust to harmless description changes using exact reviewed model IDs and validated catalog origin/capabilities, with explicit overrides preserved. Keep unknown and synthetic models excluded. Record bounded recent routing observations without query/credential data so decisions can be inspected without claiming quota savings.

Strengthen daemon compatibility using the probed executable identity when available, with a fail-safe fork capability/version boundary for same-version source builds. Retain graceful handling of missing optional metadata.

Explain legacy numeric fork_turns now means full history. Keep portable compaction and payment consent semantics.

No repository visibility changes, installed-user binary replacement, real login mutation, or paid service configuration changes. User-facing strings remain English/default-English i18n.
