# xbrd-selector

Pure-Rust xbrd-selector rover CLIs.

- `ufo-cli/` — primary crate for the `xbrd-selector` binary
- `ufo-cli-beads/` — separate legacy beads (`bd >= 1.1.2`) mailbox wrapper
- `xbrd-selector tui` — Ratatui dashboard/chat shell in the primary crate

Implementation runtime stays Rust/Ratatui. Bun is the packaging/distribution plane and test harness, similar to Claude Code's native delivery flow; the installed command itself does not require Bun.
Pilot commands intentionally run through POSIX `sh`.
The beads variant additionally requires `bd >= 1.1.2` on PATH.

Auth is read-only from OpenCode: `OPENCODE_AUTH_CONTENT`, then `XDG_DATA_HOME/opencode/auth.json`, then `~/.local/share/opencode/auth.json`.
OAuth login/logout is delegated to installed `opencode` (`auth login --pure --provider <id>`, `auth logout --provider <id>`).
Only `openai` and `github-copilot` are allowed initially; API and wellknown entries are listed as ignored.
Local model discovery is read-only from OpenCode config: `OPENCODE_CONFIG_CONTENT`, then `XDG_CONFIG_HOME/opencode/opencode.json(c)`, then `~/.config/opencode/opencode.json(c)`.
Legacy-compatible mailbox storage remains under `~/.ufo/` for now.

## Install

### Cargo

```bash
cargo install --path ufo-cli --locked
```

This installs the primary `xbrd-selector` binary. Package builds may also stage a `ufo` compatibility symlink to the same executable.

### Arch / makepkg

From the repo root of this checkout:

```bash
makepkg -si
```

That builds only `xbrd-selector` from the local source tree and installs the primary binary as `xbrd-selector`.
The package may also provide `/usr/bin/ufo` as a compatibility symlink to `xbrd-selector`.
The `ufo-cli-beads` crate remains a separate legacy variant and is not packaged as selector.
The Arch package filename follows `xbrd-selector-<pkgver>-<pkgrel>-x86_64.pkg.tar.zst`.

## Local mailbox

```bash
xbrd-selector enroll --name rover-1
xbrd-selector auth status
xbrd-selector push --title test --pilot-cmd "echo hello"
xbrd-selector start
```

## Beads

Warning: this variant requires `bd` already installed and a project that has run `bd init`.

```bash
ufo enroll
ufo auth list
ufo model list
ufo tui
ufo push --title "do X" --pilot-cmd "cargo test" --project /path/to/project
ufo start --project /path/to/project
```

`ufo model prompt --provider ollama/llama3.2 --prompt hello` resolves a local provider/model pair from config or a built-in loopback template.
`ufo tui` opens the Ratatui dashboard/chat shell.
