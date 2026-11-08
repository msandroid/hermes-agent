# Hermes Agent Rust preview

The Rust binary supports single-query chat and a line-based interactive
session with an OpenAI-compatible Chat Completions endpoint. It runs
independently of Python.

From the repository root, with a local compatible provider on port 8000:

```powershell
cargo run --locked --manifest-path native/agent/Cargo.toml -- --base-url http://127.0.0.1:8000/v1 --model MODEL -q "Hello"
cargo run --locked --manifest-path native/agent/Cargo.toml -- --base-url http://127.0.0.1:8000/v1 --model MODEL --history chat.json
```

Use `--query-file prompt.txt` or `--query-file -` for a single query from
a file or standard input. Omit the query options for the interactive session.
Type `/exit` or `/quit` to finish it. Reuse the same `--history` path to resume
completed turns. Use a separate history file for each conversation. This
option uses a versioned Rust transcript, stored separately from Hermes
SQLite history.

Set `OPENAI_API_KEY` in the process environment when the selected endpoint
requires a bearer token. The base URL and model are explicit. The preview
does not load Hermes profiles, config files or credentials from disk.


## Read an existing SQLite session

Use the full session ID and the profile's database path:

```powershell
cargo run --locked --manifest-path native/agent/Cargo.toml -- --base-url http://127.0.0.1:8000/v1 --model MODEL --state-db PATH_TO_STATE_DB --resume-session SESSION_ID
```

The Rust CLI reads a snapshot of a standalone, completed session. It reads
the stored system prompt and provider-visible message text. The connection
is read-only. Subsequent turns in this mode stay in memory. Use the Python
client for sessions that need compaction, lineage resolution, transcript
repair or provider-specific reasoning replay.

## Tools

Add `--allow-todo` to permit the model to run the Rust `todo_list` tool in the
CLI. The tool reads or updates a conversation-local checklist. Without the
flag the tool is absent from the schema and a provider call to it is refused.
The Rust library allows other hosts to supply a tool registry. The Python
client still owns the other Hermes tools and its approval workflows.

## Connect the Python gateway to Rust

Set a shared `GATEWAY_PROXY_KEY` in the Rust process and the gateway profile.
Start the Rust proxy on the same host as the gateway:

```powershell
cargo run --locked --manifest-path native/agent/Cargo.toml -- --base-url http://127.0.0.1:8000/v1 --model MODEL --gateway-listen 127.0.0.1:8642
```

Set `GATEWAY_PROXY_URL` to `http://127.0.0.1:8642` for the gateway profile
and start the existing gateway normally. It forwards its text history and
reads the completed reply in an SSE frame. The Rust proxy is stateless and
requires the shared key. Platform adapters, gateway persistence, and
platform approvals continue to run in Python.

## Tests

Run the Rust tests against a local mock provider:

```powershell
cargo test --locked --manifest-path native/agent/Cargo.toml
```

With an interpreter that can import this checkout's Hermes and gateway
dependencies, set `HERMES_PYTHON` to that interpreter and include the two
cross-runtime tests:

```powershell
cargo test --locked --manifest-path native/agent/Cargo.toml -- --include-ignored
```
