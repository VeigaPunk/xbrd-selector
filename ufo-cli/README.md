# ufo-cli

Local rover for UFO-style orchestration.

Pure Rust. Local JSONL mailbox substrate. Pilot commands run through POSIX `sh`.
OpenCode auth is read-only and shared with `ufo-cli-beads`.
OAuth login/logout is delegated to installed `opencode`; only `openai` and `github-copilot` are accepted initially.

## Install

### Cargo

```bash
cargo install --path . --locked
```

### Arch

From the repo root:

```bash
makepkg -si
```

## Use

```bash
ufo enroll --name my-rover --units 2
ufo push --title test --pilot-cmd "echo hello && date"
ufo start --poll-secs 2
ufo mailbox
ufo model prompt --endpoint http://127.0.0.1:8080/v1 --model fixture-model --prompt ping
```

`ufo auth list`/`ufo auth providers` show sanitized provider IDs, source, supported/ignored policy, and OAuth valid/expired state only.

Mailbox: `~/.ufo/mailbox.jsonl`
Workdirs: `~/.ufo/work/<op-id>`
