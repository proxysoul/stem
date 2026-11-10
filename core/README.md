# Stem core

One Rust binary (6.8 MB release build) with QuickJS inside. It works in the folder you run it in.

## Install and start

Prebuilt for macOS and Linux (arm64 and x64). Windows is not supported yet.

```sh
curl -fsSL https://raw.githubusercontent.com/proxysoul/stem/main/install.sh | sh   # prebuilt, checksum-checked
cargo install --path core          # or from source, into ~/.cargo/bin
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
| `stem learn` | between sessions: runs the project's candidate test commands on a clean checkout next to recently changed code, keeps the one that passes and records failures that exist before any change. `stem learn feedback <text>` stores what was wrong with the last fix; `stem learn <request>` shows the start note |
| `stem grafts` | your own providers and agents from `~/.stem/grafts` and `.stem/grafts`, with key status and skipped files |
| `stem doctor` | which model, key, API, project commands and language servers Stem picks, and how long each server takes to be ready |
| `stem stats` | cost, tokens, scripts and `$` calls from this project's event logs |
| `stem js [file]` | runs one script against `$` with no model (stdin if no file) |
| `stem mcp` | serves Stem over MCP (stdio; spec 2026-07-28 with `server/discover`, and older clients through `initialize`). Typed tools, each with `inputSchema`, `outputSchema`, annotations and `structuredContent`: `defs`, `refs`, `callers`, `impls`, `symbols`, `diagnostics`, `sql`, `js`, `task` (Stem runs a whole task with its own model and returns `{answer, files, cost, steps, seconds}`), `brief`, `remember`, `feedback`. `STEM_MCP_TOOLS=a,b` limits the list |
| `stem integrate [harness]` | for `claude`, `codex`, `opencode`, `pi`, `empryo` (default: all), writes into this project the MCP entry (Codex: tools pre-approved; Empryo: resident; pi: registered by its extension and exposed directly), the lifecycle hooks or plugin, and a `stem` skill that tells the agent which tool answers which question. Running it again replaces Stem's entries and changes nothing else |
| `stem hook prompt\|stop` | the lifecycle hook those harnesses call, with their JSON on stdin. `prompt` prints what Stem knows as `additionalContext` and snapshots the working tree; `stop` learns from the diff since that snapshot and the last answer. It never fails the host: problems go to `.stem/hooks.log` |
| `stem host` | Stem's learning for an agent app that runs its own model loop, such as Empryo: notes before each request, rules per tool call, test knowledge per result, learning at turn end, and `decide` callbacks to the app's own model. JSON lines on stdio; spec in [docs/host-protocol.md](docs/host-protocol.md), reference host in `tests/host.ts`. |

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

## Learning (`.stem/learn`)

Stem learns from its own finished work in this project, with nothing written by hand. Everything lives in `.stem/learn/state.json`, and it reaches the model once, as a short note at the start of a request; nothing is added in the middle of a task.

- **Resume.** A session that stops without an answer stores its request, edits, scratch scripts and last result. A later request with the same words gets them back with the earlier request quoted, and each edit marked as still in the files, undone, or changed since.
- **Regressions.** Every string edit a finished task kept is stored as before/after. When every edit of one earlier fix is undone in its file and the new request shares words with that fix's request, the start note shows the fix and asks to re-apply it. Fixes that got negative feedback are dropped.
- **Answers.** A finished answer that cites files is stored with their content hashes. A later question whose words (from the earlier question and its answer) overlap gets it back only when those files are unchanged and a small-model judge, shown the earlier answers, says one fully answers it. The judge runs alongside the first model call: a "no" adds no wait, a "yes" stops that call and the model answers from the earlier answer.
- **Usual checks.** Stem watches which commands follow edits in each part of the project (two folders deep), in any language and with any tool: a type check here, a lint there, a code generator after touching translations. Looking around, one-off scripts and `/tmp` work are ignored. Once three turns used a command and at least 30% of the turns in that area did, the first edit there gets it back with the result of the edit, once per turn. Replayed on 97 real editing turns from a large mixed-language project, 43 of the 87 commands Stem offered were then run in that same turn.
- **The task so far.** With an agent app or hooks, each conversation keeps its own short trail: the request, the files changed and how the last turn ended. A follow-up like "continue" gets that trail and nothing from other conversations.
- **Feedback.** `stem learn feedback "<what failed>"` (a CI failure, a review comment) is stored with the files the last task changed, and comes back with that code or a similar request.
- **Familiarity.** When a task ends with changes, Stem links the words of the request to the files it changed. A later request with overlapping words starts with those files, and runs at low reasoning effort when the match is strong.
- **Clean-checkout baseline.** Between sessions `stem learn` runs the candidate test commands (Stem's default and the runners in `package.json`) on the tests the project's fixes used, on a clean checkout (it skips when there are uncommitted changes). It keeps the command with the fewest failures there, the fastest if several tie, and `$.test` uses it. Tests that fail on a clean checkout are reported as pre-existing.
- **Nothing stale gets through.** Each item is checked against the code when it is used, never trusted from memory: answers only while the files they cite hash the same; regression fixes only while every "before" is back and every "after" gone (and Stem only re-applies one itself if no commit removed it, since that may have been on purpose); resume edits are re-checked one by one, and the whole resume expires after a day; feedback is marked when its code changed since, and dropped once it is 100 sessions old and changed; familiar files fade with a half-life of 60 sessions and drop when the file is gone. The test runner and known failures are measured again by `stem learn` on a clean checkout. The facts store re-parses any file whose size or time changed before every query.
- **Tasks, not turns.** A short follow-up like "continue" or "yes" joins the task it continues: Stem matches and stores it under the task's real request, so a chain of "continue" turns teaches as much as one request. Secrets, dotfiles and `tmp-`/`scratch-` files are never learned, and one task keeps at most 40 fixes. Generated files, lockfiles and locale files are never suggested; a file linked to more than a quarter of all request words in a large project counts as a hub and is skipped; a turn that changed more than 10 files only links the request to files whose path shares its words; files whose path matches the request rank higher.
- `stem learn <request>` prints the note Stem would give for that request. It only shows; it never changes files.

Measured on a sequence of bug fixes and questions in the hono repo, with Haiku 5.5 and Sonnet 5.5, each idea where it applies: continuing a stopped session costs 60% less, a regression 36% less (with the learned test runner), a question asked again in other words 59% less, a new bug in familiar code 31% less (with the runner), all at the same fix rate and faster; a rejected fix asked again passes 6/6 instead of 1/6. A new feature in familiar code costs 39% less but passed 22/24 against 24/24. Ideas that cost more than they saved were removed.

`$.save` still keeps project functions with anchors, versions and stale marks; Stem no longer asks for them on its own.

## Rules

Every `$` call can pass through your own rules first: `.stem/rules.json` in the project, then `~/.stem/rules.json`. The first rule that matches wins.

```json
{"rules": [
  {"name": "no-dist-writes", "match": {"call": ["write", "edit"], "pattern": "^dist/"}, "action": "deny", "reason": "dist/ is build output"},
  {"name": "grep-via-index", "match": {"call": "sh", "pattern": "^\\s*grep -rn (\\S+) (\\S+)\\s*$"}, "action": "redirect", "redirect": {"call": "grep", "args": {"pattern": "$1", "path": "$2"}}},
  {"name": "npm-to-bun", "match": {"call": "sh", "pattern": "^npm test"}, "action": "rewrite", "rewrite": {"from": "^npm test", "to": "bun test"}}
]}
```

- `match.call` is one call name, a list, or `"*"`.
- `pattern` is a regex over the command (`sh`), the path (`read`, `write`, `edit`, `remove`), the pattern (`grep`), the filter (`test`), the script (`run`) or the query (`sql`).
- Actions:
  - `deny` refuses the call with the reason;
  - `rewrite` replaces the matched text, and `$1` works;
  - `redirect` runs another `$` call instead, filling `$1`, `$2` from the pattern;
  - `allow` stops later rules.
- A rewritten or redirected call adds a `[stem]` line to the script's log.
- A project's own rules cannot `allow`: a cloned repo must not be able to loosen anything. Allow rules live only in `~/.stem/rules.json`.
- The built-in checks (destructive steps, scratch-only deletes) still run after the rules.
- Bad rules are reported when Stem starts and skipped.

## Scripts, not single calls

Each step is one script that can chain any number of `$` calls and use each result in the next. The system prompt lists the return shape of every call, so the model knows what comes back before it runs anything. A failed call throws an Error the script can catch and handle; if it doesn't, the script stops there and reports which call failed, and edits made before that are kept.

How to check a change is the model's call, or the user's: Stem never forces a reproduction or a test run before "done". The answer says what was checked.

## Long conversations

Every step (one script and its result) starts with a short id, `[#7]`. `$.context()` returns `{used, limit, percent, cacheWarmSeconds, steps: [{id, tokens, what, state}], outCount}`, where `state` is `in` or `out`. Steps leave the context only at compaction: when the context passes 80%, or when you type `/compact`, Stem asks the model once which finished steps to eject, by id, each with an optional one-line summary, and the board shows it is choosing. Each ejected step is written to `.stem/offload/<session>/<id>.md`, and a line with its id, what it called and the summary stays in its place. `$.recall(id)` brings one back as a new result at the end. At 95% Stem ejects the oldest half itself. A script cannot eject: `$.eject` is refused with that explanation, so the cache keeps hitting between compactions.

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
- The protocol comes from the provider's `npm` field: `@ai-sdk/anthropic` speaks the Anthropic Messages API, `@ai-sdk/openai` speaks the OpenAI Responses API (reasoning carried between steps as encrypted items, `store: false`), and OpenAI-compatible providers and gateways speak chat completions. `STEM_PROTOCOL=anthropic|chat|responses` overrides it. Context size, output limit, reasoning options (effort or thinking budget), tool support and prices all come from models.dev, so a new model works the day models.dev lists it.
- Gateways (LLM Gateway, OpenRouter, ...) are detected from their routed model ids. When a gateway's entry lacks limits or prices, Stem fills them from the model's origin provider through `canonical_model_id`.
- `$.ask` uses the cheapest recent model of the same provider.
- `claude-code/...` takes its models from the anthropic entry. With no key set and `claude` installed, it is the default. Scripts go in ```js blocks (no native tools), and prices show $0 since the plan pays.

## Grafts: your own providers and agents

A graft is one JSON file in `~/.stem/grafts/` (yours) or `.stem/grafts/` (this project's; it wins on the same name). The file name is the provider id, so `.stem/grafts/gw.json` gives `stem -m gw/<model>`. `stem grafts` lists them, which key each needs, and why a file was skipped.

```json
{ "protocol": "responses", "api": "https://api.llmgateway.io/v1", "env": ["LLMGW_KEY"], "models": ["gpt-5-mini"] }
```

- `protocol`: `anthropic`, `chat`, `responses` or `command`. The first three need `api`; `env` names the key variables (leave it out for no key).
- `models`: ids, or an object of models.dev-shaped entries. Limits and prices left out are filled from any models.dev provider that lists the same id.
- `command`: Stem runs the program once per model call, writes one JSON request to stdin (`model`, `system`, `messages` and `tools` in Anthropic Messages shape, `max_tokens`, `effort`) and reads the last JSON line of stdout: `{"content": [...], "stop_reason": "end_turn"|"tool_use"|"max_tokens", "usage": {...}}` or `{"error": "..."}`. Anything can sit behind it: a vendor SDK, a local model, or another coding agent. `grafts/openai-chat.ts` is a 40-line example that forwards to any chat-completions endpoint; `./` paths in `command` are relative to the graft file.

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
