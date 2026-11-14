<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/hero-dark.svg"><img alt="Sprout, Stem's mascot, looks around. Stem: a coding agent that grows its own tools. Rust core, QuickJS sandbox, 6.8 MB binary, any model, your Claude plan." src="brand/readme/hero-light.svg" width="100%"></picture>

<div align="center">

![Rust](https://img.shields.io/badge/Rust-core-31697e?style=for-the-badge&logo=rust&logoColor=white)
![QuickJS](https://img.shields.io/badge/QuickJS-sandbox-2f8f46?style=for-the-badge&logo=javascript&logoColor=white)
![models.dev](https://img.shields.io/badge/models.dev-any%20model-a34170?style=for-the-badge)
![Claude](https://img.shields.io/badge/Claude-Pro%20%2F%20Max-5b3fc4?style=for-the-badge&logo=claude&logoColor=white)

</div>

<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/terminal-dark.svg"><img alt="A real Stem run: it reproduces an expired Stop bug, fixes stop-memory.ts, checks the repro and the tests, and answers with Asked, Done and Not done. 11 scripts, cache 88%, $0.15, 74 s." src="brand/readme/terminal-light.svg" width="100%"></picture>

```sh
curl -fsSL https://raw.githubusercontent.com/proxysoul/stem/main/install.sh | sh   # or: cargo install --path core
stem login claude-n          # your Claude plan, or any key in ~/.stem/.env
cd your/project && stem
```

Prebuilt for macOS, Linux and Windows, on x64 and arm64 (Windows from 0.4.0). On Windows, in PowerShell: `irm https://raw.githubusercontent.com/proxysoul/stem/main/install.ps1 | iex`. MIT licensed.

<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/loop-dark.svg"><img alt="The loop: you give a task, the model answers in text or one JavaScript block, the block runs in a QuickJS sandbox that only sees $, and only the return value goes back to the model." src="brand/readme/loop-light.svg" width="100%"></picture>

- **One object.** Scripts see only `$`. No files, network or processes beyond it, so no approval pop-ups for reads and edits.
- **Small context.** A script can read a thousand files and return three lines. Only those three lines cost tokens.
- **Prompt cache first.** History is append-only, so every turn reuses the cache. `/cache 1h` for long sessions.

<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/learn-dark.svg"><img alt="Learning: Stem records what each turn does: the files it wrote, the commands it ran after editing, the fixes that stayed. From that it makes notes, such as where to start, which checks this part of the project usually runs, and which earlier fix was undone. Every note is scored against what the turn then did, and a kind of note is shown only once it has been right in at least half of ten turns." src="brand/readme/learn-light.svg" width="100%"></picture>

<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/facts-dark.svg"><img alt="Code facts: tree-sitter for TypeScript, JavaScript, Rust, Python and Go, git history and co-change, and language servers. 8,079 files indexed in 3.9 seconds, then under a millisecond per query." src="brand/readme/facts-light.svg" width="100%"></picture>

<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/proof-dark.svg"><img alt="Measured on six open bugs in a 1M-line TypeScript repo: five fixed and proven, one correctly not reproduced, 64 scripts with 0 failures, 87% of the data never reached the model, $1.15 for all six." src="brand/readme/proof-light.svg" width="100%"></picture>

## Commands

| | |
|---|---|
| `stem` | the input; type `/` for the menu |
| `stem "task"` | one task, answer on stdout |
| `stem login claude-native` | Claude plan in the browser |
| `stem models [filter]` | models you can use now |
| `stem doctor` | model, key, project, servers |
| `stem stats` | cost, tokens, scripts |
| `stem js [file]` | run a script against `$`, no model |
| `stem mcp` | serve Stem over MCP: `js`, `brief`, `remember`, `feedback` |
| `stem integrate [harness]` | plug Stem into Claude Code, Codex, OpenCode, pi, Empryo |

| In the input | |
|---|---|
| `/model opus` | switch model |
| `/effort high` | reasoning effort |
| `/cache 1h` | prompt cache TTL: `5m` or `1h` |
| `/yolo` | approvals off or on |
| `/status` · `/cost` · `/new` | state, spend, fresh start |

## Inside other agents

`stem integrate` (or one of `claude`, `codex`, `opencode`, `pi`, `empryo`) plugs Stem into the agent you already use, for this project, through each agent's own extension points:

| Harness | Tools (MCP, typed) | Before / after each request | Skill |
|---|---|---|---|
| Claude Code | `.mcp.json` | hooks in `.claude/settings.json` | `.claude/skills/stem` |
| Codex | `.codex/config.toml`, pre-approved | `.codex/hooks.json` (trust once with `/hooks`) | `.agents/skills/stem` |
| OpenCode | `opencode.json` | plugin `.opencode/plugins/stem.ts` | `.opencode/skills/stem` |
| pi | registered by the extension, exposed directly | extension `.pi/extensions/stem.ts` | `.agents/skills/stem` |
| Empryo | `.empryo/config.json`, resident | `.empryo/hooks.json` | `.agents/skills/stem` |

The agent gets real tools, each with an input and output schema: `defs`, `refs`, `callers`, `impls` (language servers), `symbols`, `diagnostics`, `sql` over symbols and git history, `js` (a script against `$`), `task` (Stem does a whole task with its own cheap model and returns the answer, changed files and cost) and `feedback`. Before each request it gets what Stem learned that applies; after each reply Stem learns from the git diff and the answer, whichever agent made the change.

Want Stem in another agent? That is the one kind of contribution we take for now: see [CONTRIBUTING.md](CONTRIBUTING.md).

## Safety

| Asks first | Never asks |
|---|---|
| deleting files, `git reset --hard`, `stash`, `rebase`, `push`, `publish`, `DROP TABLE` | reads, edits, tests, scratch files in `/tmp` |

## More

[core/README.md](core/README.md) engine, flags, file map

<div align="center">
<br>
<img src="brand/readme/sprout-love.webp" width="96" alt="Sprout, Stem's mascot, with a heart">

Sprout says hi.

100% native human brain idea + vibe coded code by clanker.
</div>
