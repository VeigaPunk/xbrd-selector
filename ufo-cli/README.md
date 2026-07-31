# xbrd-selector

Local rover for xbrd-selector-style orchestration.

Pure Rust. Local JSONL mailbox substrate. Pilot commands run through POSIX `sh`.
OpenCode auth is read-only and shared with the legacy beads crate.
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
xbrd-selector enroll --name my-rover --units 2
xbrd-selector push --title test --pilot-cmd "echo hello && date"
xbrd-selector start --poll-secs 2
xbrd-selector mailbox
xbrd-selector model list
xbrd-selector model prompt --endpoint http://127.0.0.1:8080/v1 --model fixture-model --prompt ping
xbrd-selector model prompt --provider ollama/llama3.2 --prompt ping
```

`xbrd-selector auth list`/`xbrd-selector auth providers` show sanitized provider IDs, source, supported/ignored policy, and OAuth valid/expired state only.
`xbrd-selector model list` shows only local loopback provider/model pairs and endpoints, with no secrets.

Mailbox: legacy-compatible `~/.ufo/mailbox.jsonl`
Workdirs: legacy-compatible `~/.ufo/work/<op-id>`
