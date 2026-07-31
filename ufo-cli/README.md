# ufo-cli

Local rover for UFO-style orchestration.

Pure Rust. Local JSONL mailbox substrate. Pilot commands run through POSIX `sh`.
OpenCode auth is read-only and shared with `ufo-cli-beads`.

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

`ufo auth list` shows sanitized provider IDs and redacted metadata only.

Mailbox: `~/.ufo/mailbox.jsonl`
Workdirs: `~/.ufo/work/<op-id>`
