# ufo-cli-beads

Beads-backed rover wrapper.

Implementation is Rust-only. Pilot commands run through POSIX `sh`.
This variant additionally requires `bd >= 1.1.2` on PATH and a project that already ran `bd init`.
OpenCode auth is read-only and shared with `xbrd-selector` in the primary crate.
OAuth login/logout is delegated to installed `opencode`; only `openai` (ChatGPT) and `xai` (Grok) are accepted for OAuth; Claude/Anthropic and github-copilot are not Usable.

It uses `bd ready --claim --json` atomically, then runs the pilot, then closes on success.

## Install

```bash
cargo install --path ufo-cli-beads --locked --force
```

This legacy crate still installs the same `ufo` binary name.

## Use

```bash
ufo enroll
ufo push --title "do the thing" --pilot-cmd "cargo test" --project /your/project
ufo start --project /your/project
```

`ufo auth list` shows sanitized provider IDs, source, supported/ignored policy, and OAuth valid/expired state only.
