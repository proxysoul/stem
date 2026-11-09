# Stem Host protocol

`stem host` lets an agent app use Stem's learning natively: notes before each request, rules for each tool call, test knowledge from each result, and learning when the turn ends. The app keeps its own model loop, sandbox, code index and UI. Stem keeps everything it learns in `.stem/learn/` in the project, the same store the CLI, hooks and MCP server use.

Other agents can use MCP and hooks (`stem integrate`). This protocol is the deeper path: it sees every tool result, not only the final diff, and it asks the app's own models for decisions.

## Transport

- Start one process per project: `stem host`, with the project folder as its working directory. Stem finds the git root itself.
- Messages are JSON, one per line, on stdin and stdout. Stem writes nothing else to stdout. Logs go to stderr.
- A request is `{"id", "method", "params"}`. The answer is `{"id", "result"}` or `{"id", "error": {"code", "message"}}`. A request without an `id` gets no answer.
- Both sides send requests. The app calls Stem's methods, and Stem calls the app back for the callbacks the app offered in `hello`. Stem's ids start with `stem-`.
- Stem answers one request at a time, in order. While Stem waits for a callback answer, the app may keep sending requests; Stem queues them.
- The `protocol` number only goes up on a breaking change. New methods and fields are added without bumping it; unknown fields are ignored on both sides.

## Methods the app calls

### `hello`

Send it first.

```json
{"id": "1", "method": "hello", "params": {"protocol": 1, "host": {"name": "empryo", "version": "3.13.0"}, "capabilities": ["decide"], "decideMs": 2500}}
{"id": "1", "result": {"protocol": 1, "stem": "0.2.0", "root": "/path/to/repo", "methods": ["hello", "turn.start", "..."], "callbacks": ["decide"]}}
```

- `capabilities`: the callbacks the app answers. Leave out `decide` and Stem uses only its own matching.
- `decideMs`: how long Stem waits for a `decide` answer before going on without it. Default 2500.

### `turn.start`

Before the model sees a request. Put the returned `note` in front of the request, as the model's context for this turn.

```json
{"id": "2", "method": "turn.start", "params": {"session": "tab-42", "prompt": "the cart total is wrong", "origin": "user"}}
{"id": "2", "result": {"note": "[stem: an earlier fix was undone. ...]"}}
```

- `session`: one id per conversation (a tab, a chat). Turns in the same session form one task.
- `origin`: `user` for the person's own request; `link` for messages from other tabs or agents; `engine` for the app's own automatic turns. Only `user` turns start or steer a task, so the other two get `{"note": null, "skipped": ...}`.
- `note` is null when Stem has nothing useful. A short follow-up such as "continue" or "yeah do it" never brings back an old answer or fix.
- Stem takes a git snapshot here, so it can see what this turn changed.

### `rule.check`

Before a tool call runs. Apply what comes back.

```json
{"id": "3", "method": "rule.check", "params": {"call": "sh", "args": {"cmd": "git grep -n total src"}}}
{"id": "3", "result": {"action": "redirect", "call": "grep", "args": {"pattern": "total", "path": "src", "ignoreCase": false}, "note": "ran $.grep instead of a shell search: ...", "rule": {"rule": "shell-search", "source": "built-in"}}}
```

- `action`: `run` (unchanged), `redirect` (run `call` with `args` instead), or `deny` (do not run it, and show the model `reason`).
- Rules come from `.stem/rules.json` in the project, `~/.stem/rules.json`, then Stem's built-ins, in that order. They are re-read on every call.
- Call names and arguments are Stem's `$` API: `sh {cmd}`, `read {path}`, `grep {pattern, path}`, `edit {path}`, `write {path}`, `test {filter}`.

### `tool.result`

After a tool call, for test runs. Show the model any field that comes back.

```json
{"id": "4", "method": "tool.result", "params": {"session": "tab-42", "call": "sh", "args": {"cmd": "bun test src/price.test.ts"}, "result": {"out": "...", "err": "...", "code": 1}}}
{"id": "4", "result": {"knownFailures": ["discount > rounds"], "note": "1 failing test(s) also fail on a clean checkout ..."}}
```

- Stem reads `test` calls and `sh` calls that run a test runner. Anything else answers `{}`.
- `result` may be a string (treated as `out`) or an object with `out` and `err`.

### `turn.end`

When the model's reply is done.

```json
{"id": "5", "method": "turn.end", "params": {"session": "tab-42", "answer": "Fixed total() ...", "finished": true, "origin": "user", "edits": [{"file": "src/price.ts", "before": "...", "after": "..."}], "touched": ["src/price.ts"], "tests": ["src/price.test.ts"]}}
{"id": "5", "result": {"request": "the cart total is wrong", "edits": 1, "files": ["src/price.ts"]}}
```

- `finished`: false when the turn stopped early. Stem keeps it as work to resume.
- `edits` and `touched`: what this session changed. Send them when several sessions share one working tree, so each learns only its own changes. When left out, Stem reads the git diff since `turn.start`.
- `tests`: test files the turn ran.
- Turns whose `origin` is not `user` are skipped.

### `feedback`

When the person says a fix was wrong. Stem forgets that fix and keeps the correction as a lesson for similar requests.

```json
{"id": "6", "method": "feedback", "params": {"session": "tab-42", "text": "the rounding has to happen per line, not on the total"}}
```

### `status`

What Stem knows about this project, for a panel or a status chip.

```json
{"id": "7", "method": "status"}
{"id": "7", "result": {"sessions": 214, "fixes": 61, "answers": 9, "lessons": 2, "knownFailures": 1, "familiarWords": 340, "testCommand": "bun test <files>", "resume": false}}
```

### `shutdown`

Stem answers `{}` and exits. Closing stdin also ends it.

## Callbacks Stem sends

### `decide`

Stem asks this when its own matching is unsure: a request that may be about an earlier fix whose code is back, or a question that may repeat an earlier one. The app answers with its own model.

```json
{"id": "stem-1", "method": "decide", "params": {"kind": "fix", "question": "Which earlier fix, if any, is the same bug this request is about? Its code has been undone in the file.", "request": "le montant du panier est faux", "options": [{"id": "f0", "text": "the cart total adds price and qty instead of multiplying them (src/price.ts)"}]}}
{"id": "stem-1", "result": {"choice": "f0", "confidence": 0.9}}
```

- `kind`: `fix` or `answer`.
- `choice`: an option id, or null for none of them. Stem uses the choice only when `confidence` is at least 0.6.
- An answer that arrives after `decideMs` is ignored, and the turn goes on without it.

## Measured

On 28 reworded, vague, French and Arabic reports of 7 fixed hono bugs, with each bug's old code put back, and the app answering `decide` with the model shown:

| | earlier fix found | wrong fix | fix shown for an unrelated request | time per decision | cost for 35 decisions |
|---|---|---|---|---|---|
| Stem alone | 21 / 28 | 0 | 0 / 28 | none | $0 |
| `decide` with Haiku 5.5 | 28 / 28 | 0 | 0 / 28 | 1.0 s | $0.0013 |
| `decide` with Sonnet 5.5 | 28 / 28 | 0 | 0 / 28 | 1.4 s | $0.021 |
| `decide` with Opus 5.5, medium effort | 28 / 28 | 0 | 0 / 28 | 1.7 s | $0.060 |

## Reference host

`tests/host.ts` drives `stem host` end to end on a fresh repo: `bun tests/host.ts [path to stem]`. `tests/smoke.sh` runs it too. It checks every method, a `decide` that is accepted, declined and never answered, follow-ups, messages from other tabs, rules and status.
