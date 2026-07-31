# ufogrokbd

Pure-Rust UFO rover CLIs.

- `ufo-cli/` — local JSONL mailbox (`~/.ufo/mailbox.jsonl`)
- `ufo-cli-beads/` — beads (`bd >= 1.1.2`) mailbox wrapper

Implementation is Rust-only. Pilot commands intentionally run through POSIX `sh`.
The beads variant additionally requires `bd >= 1.1.2` on PATH.

Auth is read-only from OpenCode: `OPENCODE_AUTH_CONTENT`, then `XDG_DATA_HOME/opencode/auth.json`, then `~/.local/share/opencode/auth.json`.
OAuth login/logout is delegated to installed `opencode` (`auth login --pure --provider <id>`, `auth logout --provider <id>`).
Only `openai` and `github-copilot` are allowed initially; API and wellknown entries are listed as ignored.
Local model discovery is read-only from OpenCode config: `OPENCODE_CONFIG_CONTENT`, then `XDG_CONFIG_HOME/opencode/opencode.json(c)`, then `~/.config/opencode/opencode.json(c)`.

## Install

### Cargo

```bash
cargo install --path ufo-cli --locked
```

Beads install (replaces the same `ufo` binary):

```bash
cargo install --path ufo-cli-beads --locked --force
```

### Arch / makepkg

From the repo root of this checkout:

```bash
makepkg -si
```

That builds only `ufo-cli` from the local source tree and installs the binary as `ufo`.
The `ufo-cli-beads` crate is a separate local variant and also installs `ufo`.

## Local mailbox

```bash
ufo enroll --name rover-1
ufo auth status
ufo push --title test --pilot-cmd "echo hello"
ufo start
```

## Beads

Warning: this variant requires `bd` already installed and a project that has run `bd init`.

```bash
ufo enroll
ufo auth list
ufo model list
ufo push --title "do X" --pilot-cmd "cargo test" --project /path/to/project
ufo start --project /path/to/project
```

`ufo model prompt --provider ollama/llama3.2 --prompt hello` resolves a local provider/model pair from config or a built-in loopback template.
