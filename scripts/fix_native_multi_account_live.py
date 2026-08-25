#!/usr/bin/env python3
from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one fixup anchor, found {count}: {old!r}")
    file.write_text(text.replace(old, new, 1))
    print(f"fixed {path}")


replace_once(
    "codex-rs/core/src/session/turn.rs",
    "            ResponseEvent::Created => mark_sampling_response_started(&turn_context)\n            ResponseEvent::OutputItemDone",
    "            ResponseEvent::Created => mark_sampling_response_started(&turn_context),\n            ResponseEvent::OutputItemDone",
)

replace_once(
    "codex-rs/core/src/session/turn.rs",
    "                    let mut checkpoint = attempt_state\n                        .as_ref()\n                        .map_or_default(crate::sampling_attempt::SamplingAttemptState::snapshot);",
    "                    let mut checkpoint = attempt_state\n                        .as_ref()\n                        .map(crate::sampling_attempt::SamplingAttemptState::snapshot)\n                        .unwrap_or_default();",
)

replace_once(
    "codex-rs/login/src/account_pool.rs",
    "            } if resets_at <= now\n        ) {",
    "            } if &*resets_at <= now\n        ) {",
)

replace_once(
    "codex-rs/core/src/session/inject.rs",
    "        .then_some(CodexHarnessMetadata {\n            client_authored: true,\n        });",
    "        .then_some(CodexHarnessMetadata {\n            client_authored: true,\n            ..CodexHarnessMetadata::default()\n        });",
)

replace_once(
    "codex-rs/core/src/account_transition.rs",
    "            let source_profile_id = envelope\n                .metadata\n                .as_ref()\n                .and_then(|metadata| metadata.execution_profile_id.as_deref())\n                .or(self.legacy_unattributed_profile_id.as_deref());\n            let source_generation = envelope\n                .metadata\n                .as_ref()\n                .and_then(|metadata| metadata.execution_generation);\n\n            let same_profile = source_profile_id == self.target_profile_id.as_deref();",
    "            let source_profile_id = envelope\n                .metadata\n                .as_ref()\n                .and_then(|metadata| metadata.execution_profile_id.clone())\n                .or_else(|| self.legacy_unattributed_profile_id.clone());\n            let source_generation = envelope\n                .metadata\n                .as_ref()\n                .and_then(|metadata| metadata.execution_generation);\n\n            let same_profile = source_profile_id.as_deref() == self.target_profile_id.as_deref();",
)

replace_once(
    "codex-rs/core/src/account_transition.rs",
    "                envelope.into_item(),\n                source_profile_id,\n                self.target_profile_id.as_deref(),",
    "                envelope.into_item(),\n                source_profile_id.as_deref(),\n                self.target_profile_id.as_deref(),",
)

print("native multi-account live patch fixups applied successfully")
