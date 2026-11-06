# Stem core

One Rust binary (6.8 MB release build) with QuickJS inside. It works in the folder you run it in.

## Install and start

```sh
cargo install --path core          # puts `stem` in ~/.cargo/bin
echo 'ANTHROPIC_API_KEY=sk-ant-...' >> ~/.stem/.env && chmod 600 ~/.stem/.env
cd any/project && stem             # the input; or: stem "fix the failing test"
```

Keys are read from the shell, then the project's `.env`, then `~/.stem/.env`. Any provider listed on models.dev works if its key variable is set (its name comes from models.dev, for example `OPENROUTER_API_KEY`, `LLMGATEWAY_API_KEY`; `LLMGW_KEY` is accepted for LLM Gateway). `STEM_API_KEY` overrides all of them.

No key? `stem login claude-native` (alias `claude-n`, or `/login claude-native` in the input) signs in to your Claude Pro/Max plan in the browser with no Claude Code needed, over OAuth (callback on `localhost:54545`, or paste the code or redirect URL when the browser cannot reach this machine). The tokens go to `~/.stem/auth/claude.json` (mode 600) and refresh on their own; requests go straight to the Anthropic Messages API with Claude Code's OAuth headers, so native tools, thinking, effort and prompt caching all work. Models are `claude-native/<anthropic model id>`, prices show $0.

`stem login claude` (or `/login claude`) instead runs Claude Code's own sign-in and switches Stem to your Claude plan. Stem then runs the installed `claude` binary as a bare model: `--tools ""`, its own `--system-prompt`, no settings, MCP, skills, memory or CLAUDE.md, one process per conversation fed over stream-json. Models are `claude-code/<anthropic model id>`; `STEM_CLAUDE` points at another binary.

## Commands

| Command | What it does |
|---|---|
| `stem` | one native input. Enter sends; Alt+Enter, Shift+Enter or Ctrl+J add a line; Ctrl+U, Ctrl+W, Ctrl+K, Ctrl+Y, Ctrl+R and history work as in a shell. Type `/` for the command menu |
| `stem "task"` | runs one task, prints the final answer on stdout, the cost line on stderr. Piped stdin works too |
| `stem models [filter]` | models you can use now (● = key set), live from models.dev |
| `stem doctor` | which model, key, API, project commands and language servers Stem picks, and how long each server takes to be ready |
| `stem stats` | cost, tokens, scripts and `$` calls from this project's event logs |
| `stem js [file]` | runs one script against `$` with no model (stdin if no file) |
| `stem mcp` | serves `$` as one MCP tool, `js`, over stdio, so another harness can use Stem as its whole tool layer |

Flags: `-m provider/model` (or `STEM_MODEL`), `-e effort` (or `STEM_EFFORT`, default `high`), `-y` / `--yolo` (or `STEM_YOLO=1`), `-C folder`.

## In the input

| Type | What happens |
|---|---|
| `/` | the command menu opens; arrows or Tab move, Enter runs |
| `/model opus` | models from models.dev that match, the ones with a key first, with name, price, context and date. Enter switches (a new conversation) |
| `/effort high` | the model's own effort levels from models.dev (`low` to `max`); for thinking-budget models `off`, `low`, `medium`, `high`, `max` set the budget |
| `/yolo` | approvals off (prompt turns amber: `stem yolo`), again to turn them back on |
| `/status` | project, runner, which language server serves which folder, model, cache, memory and CPU |
| `/cache 1h` | prompt cache TTL for Anthropic models (API key or `claude-native`): `5m` (default) or `1h`; `/ttl` works too |
| `/cost` | session cost and cache hit rate |
| `/new`, `/exit` | new conversation, quit |

`/model`, `/effort`, `/cache` and `/yolo` are saved in `~/.stem/config.json` and used the next time Stem starts, in any project. Flags and `STEM_*` variables override the saved choice for one run; if the saved model's key is gone, Stem says so and picks from the keys it has.

The right side of the prompt shows how long the prompt cache stays warm (`cache warm 4:12`, then `cache cold`).

## Approvals

Destructive steps wait for a key press: `y` once, `a` yes to everything (turns yolo on), `n` no. Destructive means: `$.remove`, and shell commands that delete files (`rm`, `find -delete`, `git rm`, `git clean`), throw away work (`git reset --hard`, `git checkout -- files`, `git restore`, `git stash`), rewrite history (`rebase`, `commit --amend`, `filter-branch`), change shared state (`git push`, `publish`, `gh ... merge/delete`, `kubectl delete`, `docker rm`), or drop data (`DROP TABLE`, `TRUNCATE`). The shell line is split into commands and words, so `echo "rm"` or `git checkout -b x` do not ask. With nobody at the keyboard (piped, MCP, `stem js`) a destructive step is refused and the answer says so, unless `--yolo` is set.

## The answer

Always the same shape, no prose: `Asked:` (the request rewritten in one line), then `Answer:` for a question, or `Done:` and `Not done:` (with why) for a task. Steps from the model's plan that were never marked done are added to `Not done:` by Stem itself.

## Cache

History is append-only: Stem never rewrites an earlier message, so every turn reuses the provider's prompt cache. When a conversation reaches 90% of the model's context Stem stops and asks for `/new` instead of trimming. Anthropic caches live 5 minutes after the last use (`/cache 1h` or `STEM_CACHE_TTL=1h` for one hour, at a higher write price); OpenAI models are estimated at 10 minutes. The hit rate is shown per turn and per session.

## Code facts (`$.sql`)

A SQLite store in `.stem/facts.db`, filled by tree-sitter (TypeScript, JavaScript, Rust, Python, Go) and git. It stays current on every query: only files whose size or time changed are parsed again. Tables: `files`, `symbols` (definitions), `refs` (calls and type uses by name), `commits`, `commit_files`, `cochange` (files changed together). On an 8,079-file TypeScript repo (1.06M lines) the first index takes 3.9 s and holds 84k symbols, 714k references and 3,000 commits; later queries take under a millisecond plus a quick change check. `$.sql` only reads.

## Growth (`$.save`)

`$.save(name, fn, {doc, check})` keeps a function made of `$` calls in `.stem/tools/`. The check runs before saving and again the first time the function is used in a session (only for functions that do not change files); a failing one is dropped. A function that fails 3 times and more often than it works is dropped, and so is one unused for 21 days. Up to 12 saved functions are listed in the system prompt with their use counts, and `stem tools` lists them all, the dropped ones with the reason. Calling a `$` function that does not exist names the closest ones and shows how to save it. When a script of the same shape (strings and numbers aside) has run 3 times in the project, its result suggests saving it. `STEM_TOOLS=off` turns all of this off; every turn logs the generation (a hash of the live functions).

## Proof before "done"

Stem does not accept a final answer while files changed since anything last ran, or when a bug fix ran no reproduction, or when a task changed and ran nothing at all. It asks once for each case (a repro script in /tmp, then the covering tests from `$.testsFor`). Scratch files under /tmp need no approval.

## Long conversations

Every step (one script and its result) ends with a short id, `[#7]`. `$.context()` lists them: id, tokens, what it called, and `in`, `leaving` or `out`, with the context use and the seconds the cache stays warm. `$.eject([ids], summary)` marks steps to leave; each goes to `.stem/offload/<session>/<id>.md` and the summary (written by the same model if left out, or `STEM_SUMMARIZER=provider/model`) stays in its place. `$.recall(id)` brings one back as a new result at the end. Ejected steps only leave when it costs nothing extra: when the cache has gone cold anyway, or once the context passes 80%. Until then they stay and the cache keeps hitting. At 95% Stem marks the oldest half itself and moves it out at once.

## Cache per provider

The request is always in the same order and byte-stable: tools, system prompt, then the messages, only ever appended to. How the cache uses that:

| Provider | How it caches | What Stem sends |
|---|---|---|
| Anthropic | explicit breakpoints on a prefix, 5 min after last use (or 1 h) | `cache_control` on the system prompt, the tool and the last message block |
| OpenAI | automatic on prompts over 1,024 tokens, routed by the first tokens | `prompt_cache_key` (a hash of the system prompt) so requests land where the prefix is cached |
| Gateways and OpenAI-compatible providers | depends on the upstream: automatic for OpenAI, DeepSeek, Kimi, Gemini; Claude only if the gateway adds breakpoints | nothing extra; the hit rate shows whether it works |

## Monorepos

Each language gets a server per sub-project root: the topmost `package.json`/`tsconfig.json` for TypeScript, `go.work` or the nearest `go.mod` for Go, the topmost `Cargo.toml` for Rust, the nearest `pyproject.toml`/`setup.py` for Python, `compile_commands.json` or the top `CMakeLists.txt` for C and C++. A question about a file goes to the server for that file's root; a question by name goes to the roots where the name appears.

## Models and providers, on demand

Nothing about models is built in. On first use Stem fetches `https://models.dev/api.json`, keeps a 250 KB index plus one small file per provider in `~/.stem/cache/models.dev/`, and revalidates with the ETag after 10 minutes (a 304 costs no parsing). Resolving a model reads the index and only that provider's file.

- No `--model`: the first provider whose key is set (direct providers before gateways), and its newest model that models.dev describes as a coding model. With an Anthropic key today that is `claude-sonnet-5-5`.
- `--model name`: any provider with a key that lists that model. `--model provider/model`: exactly that.
- The protocol comes from the provider's `npm` field: `@ai-sdk/anthropic` speaks the Anthropic Messages API, OpenAI and OpenAI-compatible providers and gateways speak chat completions. Context size, output limit, reasoning options (effort or thinking budget), tool support and prices all come from models.dev, so a new model works the day models.dev lists it.
- Gateways (LLM Gateway, OpenRouter, ...) are detected from their routed model ids. When a gateway's entry lacks limits or prices, Stem fills them from the model's origin provider through `canonical_model_id`.
- `$.ask` uses the cheapest recent model of the same provider.
- `claude-code/...` takes its models from the anthropic entry. With no key set and `claude` installed, it is the default. Scripts go in ```js blocks (no native tools), and prices show $0 since the plan pays.

## What runs where

| File | Role |
|---|---|
| `main.rs` | commands, the input (reedline), Ctrl+C handling |
| `catalog.rs` | models.dev fetch, split cache, model and provider resolution |
| `llm.rs` | streaming clients for Anthropic and OpenAI-style APIs, retries, prompt caching, thinking blocks |
| `claude_native.rs` | Claude plan sign-in in the browser (OAuth PKCE), token refresh, and the Claude Code request fingerprint |
| `claude_code.rs` | Claude plan sign-in and the `claude -p` stream-json bridge, stripped of tools and prompt |
| `agent.rs` | the loop, the system prompt, script results, context pruning, the event log per turn |
| `sandbox.rs`, `prelude.js` | QuickJS: one native bridge, `$` built in JS on top of it |
| `host.rs` | every `$` function |
| `lsp.rs`, `servers.rs` | language server client; finds or downloads tsgo, ty and rust-analyzer, uses gopls, clangd, lua-language-server, zls when installed |
| `project.rs` | runner and commands from lockfiles and manifests, npm/npx rewritten to the project's runner |
| `events.rs` | `.stem/sessions/<time>.jsonl` in the project (the folder ignores itself in git) |
| `ui.rs` | the growing cell shown while Stem works |

## While it works

Tool calls are not printed. Four lines on stderr, redrawn about 16 times a second and erased when the turn ends:

1. the current step of the model's plan (`2 of 4 · add tests`), or the current action when there is no plan
2. a progress bar from that plan, with percent and elapsed time; without a plan a light runs back and forth
3. one block per `$` call, newest on the right: `▪` under 1.5 KB of data, `■` under 15 KB, `█` under 150 KB, `██` more; blue looked, green changed, pink ran, violet asked a language server, amber asked the small model, red `✕` failed
4. scripts, calls, cache hit, warm time left, memory and CPU of Stem with its language servers and test processes, and each server's state
