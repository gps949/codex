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

print("native multi-account live patch fixups applied successfully")
