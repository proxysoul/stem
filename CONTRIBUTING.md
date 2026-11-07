# Contributing to Stem

Thanks for wanting to help. For now Stem takes one kind of contribution: **support for another coding agent**. Everything else stays as it is.

## What we accept

- A new agent in `stem integrate`: Aider, Cursor, Zed, Gemini CLI, Goose, Cline, Continue, or any agent people use.
- Fixes to the existing agent adapters (Claude Code, Codex, OpenCode, pi, Empryo) when that agent changes its hooks, plugin or MCP format.

## What we don't accept

- Changes to the architecture, the loop, the `$` API, learning, or the idea behind Stem.
- New features outside agent support, refactors, renames, or style changes.

If you are not sure, open an issue first and ask.

## How an agent plugs in

Every agent uses the same two pieces. Your adapter only wires them into that agent's own extension points.

| Piece | What it is | What the agent does with it |
|---|---|---|
| `stem hook prompt` | reads the agent's hook JSON on stdin (`cwd`, `session_id`, `prompt`) and prints `{"hookSpecificOutput": {"additionalContext": ...}}` | runs it before each request and adds the text to the model's context |
| `stem hook stop` | reads `cwd`, `session_id`, `last_assistant_message` | runs it when the reply ends; Stem learns from the git diff |
| `stem mcp` | an MCP server over stdio with typed tools | registers it with `STEM_MCP_TOOLS=js,defs,refs,callers,impls,symbols,sql,diagnostics,task,feedback` and `STEM_YOLO=0` |
| skill | `SKILL.md`, the table of which tool answers which question | puts it where the agent reads skills |

If the agent has no hooks, use whatever runs code at those two moments (a plugin or extension event), as the OpenCode plugin and pi extension do. If it has no MCP, register a tool that runs `stem js`.

## Adding one

1. Add a target in `core/src/integrate.rs`: the files to write, merged into existing config and safe to run twice.
2. Add the agent to the table in `README.md` and the `stem integrate` row in `core/README.md`.
3. Extend `core/tests/smoke.sh` so it checks the files your target writes.
4. Prove it in the real agent. Run one request in a scratch git repo after `stem integrate <agent>`, and include in the pull request:
   - the `prompt` and `stop` lines from `.stem/hooks.log`;
   - one Stem tool call from the agent's own log or transcript;
   - the agent's version.

## Rules

- No code comments. Names and types carry the meaning.
- Keep the change to the adapter; touch nothing else.
- `cargo build --release` and `bash core/tests/smoke.sh` must pass.
- Sign off every commit (`git commit -s`): it states you wrote the change and may contribute it under the project's license.
- Commit titles are `type(scope): subject`, for example `feat(integrate): aider`.
