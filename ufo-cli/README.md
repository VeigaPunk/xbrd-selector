# ufo-cli

Local rover for UFO-style orchestration.

Pure Rust. Local JSONL mailbox substrate. Pilot commands run through POSIX `sh`.
OpenCode auth is read-only and shared with `ufo-cli-beads`.
OAuth login/logout is delegated to installed `opencode`; only `openai` and `github-copilot` are accepted initially.
Local model discovery is read-only from OpenCode config: `OPENCODE_CONFIG_CONTENT`, then `XDG_CONFIG_HOME/opencode/opencode.json(c)`, then `~/.config/opencode/opencode.json(c)`.

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
ufo model list
ufo model prompt --endpoint http://127.0.0.1:8080/v1 --model fixture-model --prompt ping
ufo model prompt --provider ollama/llama3.2 --prompt ping
```

`ufo auth list`/`ufo auth providers` show sanitized provider IDs, source, supported/ignored policy, and OAuth valid/expired state only.
`ufo model list` shows only local loopback provider/model pairs and endpoints, with no secrets.

Mailbox: `~/.ufo/mailbox.jsonl`
Workdirs: `~/.ufo/work/<op-id>`
