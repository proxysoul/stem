<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/hero-dark.svg"><img alt="Sprout, Stem's mascot, looks around. Stem: a coding agent that grows its own tools. Rust core, QuickJS sandbox, 6.8 MB binary, any model, your Claude plan." src="brand/readme/hero-light.svg" width="100%"></picture>

<div align="center">

![Rust](https://img.shields.io/badge/Rust-core-31697e?style=for-the-badge&logo=rust&logoColor=white)
![QuickJS](https://img.shields.io/badge/QuickJS-sandbox-2f8f46?style=for-the-badge&logo=javascript&logoColor=white)
![models.dev](https://img.shields.io/badge/models.dev-any%20model-a34170?style=for-the-badge)
![Claude](https://img.shields.io/badge/Claude-Pro%20%2F%20Max-5b3fc4?style=for-the-badge&logo=claude&logoColor=white)

</div>

<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/terminal-dark.svg"><img alt="A real Stem run: it reproduces an expired Stop bug, fixes stop-memory.ts, checks the repro and the tests, and answers with Asked, Done and Not done. 11 scripts, cache 88%, $0.15, 74 s." src="brand/readme/terminal-light.svg" width="100%"></picture>

```sh
cargo install --path core
stem login claude-n          # your Claude plan, or any key in ~/.stem/.env
cd your/project && stem
```

<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/loop-dark.svg"><img alt="The loop: you give a task, the model answers in text or one JavaScript block, the block runs in a QuickJS sandbox that only sees $, and only the return value goes back to the model." src="brand/readme/loop-light.svg" width="100%"></picture>

- **One object.** Scripts see only `$`. No files, network or processes beyond it, so no approval pop-ups for reads and edits.
- **Small context.** A script can read a thousand files and return three lines. Only those three lines cost tokens.
- **Prompt cache first.** History is append-only, so every turn reuses the cache. `/cache 1h` for long sessions.

<picture><source media="(prefers-color-scheme: dark)" srcset="brand/readme/grow-dark.svg"><img alt="Growth: a script shape that ran three times is saved as a named function with a self-check, listed in the prompt with its use count, and dropped when unused for 21 days or failing more than it works." src="brand/readme/grow-light.svg" width="100%"></picture>

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
| `stem mcp` | serve `$` as one MCP tool |

| In the input | |
|---|---|
| `/model opus` | switch model |
| `/effort high` | reasoning effort |
| `/cache 1h` | prompt cache TTL: `5m` or `1h` |
| `/yolo` | approvals off or on |
| `/status` · `/cost` · `/new` | state, spend, fresh start |

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
</div>
