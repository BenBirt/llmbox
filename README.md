# llmbox

llmbox runs an agentic loop where an LLM (Claude or Gemini) writes JavaScript, executes it in a sandboxed V8 environment, observes the results, and iterates until the task is complete.

## How it works

1. You provide a natural language prompt.
2. llmbox sends it to Claude along with a description of the available sandbox capabilities.
3. Claude responds with a JavaScript code block.
4. The code runs inside a fresh V8 isolate.
5. The output is fed back to Claude as context.
6. Steps 3–5 repeat (up to 10 rounds) until Claude produces a plain-text answer with no code.

## Building

The build requires two environment variables that point to the prebuilt V8 static library checked in under `third_party/v8/`. If [direnv](https://direnv.net/) is active they are already exported via `.envrc`; otherwise set them manually:

```sh
export RUSTY_V8_ARCHIVE="$PWD/third_party/v8/librusty_v8_release_x86_64-unknown-linux-gnu.a.gz"
export RUSTY_V8_SRC_BINDING_PATH="$PWD/third_party/v8/src_binding_release_x86_64-unknown-linux-gnu.rs"
```

Then build and run with Bazel:

```sh
bazel build //:llmbox
bazel run   //:llmbox -- "<your prompt>"
```

See `CLAUDE.md` for additional network/proxy setup required in restricted environments.

## Usage

```
llmbox <PROMPT> [OPTIONS]

Options:
  --provider <PROVIDER>  LLM provider: anthropic (default) or gemini
  --model <MODEL>        Model name (defaults to claude-sonnet-4-6 or gemini-2.0-flash)
  --mount <PATH:MODE>    Expose a directory to the sandbox (ro = read-only, rw = read-write)
  --http                 Enable HTTP requests from the sandbox
  --env <VAR>            Expose an environment variable by name (repeatable)
```

The required API key environment variable depends on the provider:

| Provider | Environment variable |
|----------|---------------------|
| `anthropic` (default) | `ANTHROPIC_API_KEY` |
| `gemini` | `GEMINI_API_KEY` |

### Examples

```sh
# Summarise a local log file (Anthropic, default)
llmbox "count ERROR lines per hour in app.log" --mount /var/log/myapp:ro

# Same task using Gemini
GEMINI_API_KEY=AIza... llmbox --provider gemini \
  "count ERROR lines per hour in app.log" --mount /var/log/myapp:ro

# Fetch and transform remote data
llmbox "fetch the GitHub Zen API and reverse the words" --http

# Use a specific model
llmbox --provider gemini --model gemini-2.5-pro "explain the code in src/" --mount src/:ro

# Use a secret already in the environment
llmbox "call the internal API and parse the response" --http --env INTERNAL_API_KEY
```

## Sandbox capabilities

Each `--mount`, `--http`, and `--env` flag extends the JavaScript API available inside the sandbox. Only explicitly enabled capabilities are exposed; the system prompt sent to Claude reflects exactly what the sandbox can do.

| Flag | JavaScript API |
|------|---------------|
| `--mount /path:ro` | `fs.read(path)` → string |
| `--mount /path:rw` | `fs.read(path)`, `fs.write(path, content)` |
| `--http` | `http.get(url[, headers])`, `http.post(url, body[, headers])` → string |
| `--env VAR` | `env.get("VAR")` → string \| null |

`console.log(...)` is always available and prints to stderr.

### Security notes

- Each JavaScript execution runs in a **fresh V8 isolate** — no state carries over between rounds.
- Filesystem access is **path-validated**; the sandbox cannot escape a mounted directory.
- Environment variable access is **allowlist-only**; only variables named with `--env` are visible.
- HTTP responses that return non-2xx status throw a JavaScript error.

## Dependencies

| Crate | Purpose |
|-------|---------|
| [v8](https://crates.io/crates/v8) | V8 JavaScript engine bindings |
| [ureq](https://crates.io/crates/ureq) | HTTP client |
| [clap](https://crates.io/crates/clap) | CLI argument parsing |
| [serde\_json](https://crates.io/crates/serde_json) | JSON handling |
| [thiserror](https://crates.io/crates/thiserror) | Error type derivation |
