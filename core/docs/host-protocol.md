# Stem Host protocol

`stem host` lets an agent app use Stem's learning natively: notes before each request, rules for each tool call, test knowledge from each result, and learning when the turn ends. The app keeps its own model loop, sandbox, code index and UI. Stem keeps everything it learns on the machine, in `~/.stem/projects/<project>-<id>/` (`status` returns it as `store`), the same store the CLI, hooks and MCP server use. Nothing is written into the project.

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

- `show`: leave it out. `"all"` shows every kind of note whether it has earned it or not (see `turn.start`); it is for benchmarks.
- `capabilities`: what the app does. `decide`: it answers the `decide` callback; without it Stem uses only its own matching. `edits`: it sends `edits` and `touched` on every `turn.end`, so Stem takes no git snapshot at `turn.start` and never diffs the working tree. On a large repo that makes `turn.start` about half a second faster (measured: 500 to 1000 ms down to 1 to 150 ms).
- `decideMs`: how long Stem waits for a `decide` answer before going on without it. Default 2500.

### `learn.import`

Once per project, right after `hello`, when the app has an older store of its own to carry over.

```json
{"id": "8", "method": "learn.import", "params": {"path": "/path/to/repo/.empryo/stem/learn.json", "format": "empryo-stem-1"}}
{"id": "8", "result": {"fixes": 111, "answers": 2, "lessons": 0, "knownFailures": 0}}
```

- The result counts what was added. Running it again adds nothing.
- Only fixes still present in the code, answers whose cited files are unchanged, lessons, and tests that failed twice on a clean tree are taken.
- An unknown `format` answers with an error.

### `turn.start`

Before the model sees a request. Put the returned `note` in front of the request, as the model's context for this turn.

```json
{"id": "2", "method": "turn.start", "params": {"session": "tab-42", "prompt": "the cart total is wrong", "origin": "user"}}
{"id": "2", "result": {"note": "[stem: an earlier fix was undone. ...]", "kinds": ["regress"], "withheld": ["familiar"], "familiarity": {"score": 3.4, "priorSteps": 12}}}
```

- `session`: one id per conversation (a tab, a chat). Turns in the same session form one task.
- `origin`: `user` for the person's own request; `engine` for the app's own automatic turns (a forced continuation, a review round); `link` for messages from other tabs or agents. Only `user` turns start or steer a task. An `engine` turn continues the session's task and answers `{"note": null, "kinds": [], "continues": true}`; what it changes is learned under that task's request. When Stem has no task for the session, it takes the one in `task`. With no task at all, for `link`, and for any other origin (an app may send its own, such as `agent` for prompts written by another agent), the answer is `{"note": null, "kinds": [], "skipped": ...}`. No origin is ever rejected.
- `task`: optional, the conversation's request as the app knows it. Stem uses it when a follow-up or an `engine` turn arrives in a conversation it has not seen before.
- `note` is null when Stem has nothing useful. A short follow-up such as "continue" or "yeah do it" never brings back an old answer or fix.
- `kinds`: which kinds of note are in `note`: `familiar`, `answer`, `regress`, `resume`, `lessons`, `trail`.
- `withheld`: kinds Stem worked out for this turn and left out of `note`, because that kind has not earned being shown in this project. A kind earns it by being right: over at least 10 turns, and judged on its last 30, the turn went on to do what the note said in at least half (see `followed` under `turn.end`), whether the note was shown or not. `familiar` and `habits` start withheld. `regress`, `answer` and `resume` are shown until they have 10 turns on record, then have to hold the same rate. `lessons` at the start of a turn follow `familiar`. `trail` and the notes tied to an opened file are always shown. Work that stopped is only offered back to the session it stopped in, and not within ten minutes of stopping, when the session still has it in view. `status` reports where each kind stands.
- A short follow-up in the same session gets the task so far (`trail`): its request, the files it changed and how the last turn ended. Another session's follow-up gets nothing from it.
- `answer`, when an earlier answer fits: `{"text", "request", "files", "sure"}`. `sure` is true only when the wording is nearly identical, so the app may show the answer before running the model. A match picked through `decide` has `sure: false`.
- `familiarity`, when earlier work matches: `{"score", "priorSteps"}`. `priorSteps` is how many steps the last turn of this session's task took, or null.
- Stem takes a git snapshot here, so it can see what this turn changed.

### `rule.check`

Before a tool call runs. Apply what comes back.

```json
{"id": "3", "method": "rule.check", "params": {"session": "tab-42", "call": "sh", "args": {"cmd": "git grep -n total src"}}}
{"id": "3", "result": {"action": "redirect", "call": "grep", "args": {"pattern": "total", "path": "src", "ignoreCase": false}, "note": "ran $.grep instead of a shell search: ...", "rule": {"rule": "shell-search", "source": "built-in"}}}
```

- `action`: `run` (unchanged), `redirect` (run `call` with `args` instead), or `deny` (do not run it, and show the model `reason`).
- Rules come from `.stem/rules.json` in the project, `~/.stem/rules.json`, then Stem's built-ins, in that order. They are re-read on every call.
- A project's own rules can only `deny` a call. Redirects and rewrites belong in `~/.stem/rules.json`, so a cloned repo can block a call but never change one.
- `note` can also come with `run`. Show it to the model with the tool's result. These notes are tied to the file being opened, not to the wording of the request, and each comes once per turn:
  - `read` or `edit` of a file where an earlier fix has been undone: the fix, and the request it was for.
  - `edit` or `write` of a file that has feedback on an earlier fix: that feedback.
  - the first `edit` or `write` in an area of the project: that area's usual checks (below).
- Usual checks: the commands earlier turns ran after changing files there, such as `cd packages/web && npm run typecheck` (9 of 21 turns). A command is offered once at least three turns used it and at least 30% of the recent turns in that area did (older turns fade, so a busy area keeps up with how it is checked now), and only if it ran at least twice as often after an edit as before one, which leaves out probes that run at any time.
- Call names and arguments are Stem's `$` API: `sh {cmd}`, `read {path}`, `grep {pattern, path}`, `edit {path}`, `write {path}`, `test {filter}`.

### `tool.result`

After a tool call. Show the model any field that comes back.

```json
{"id": "4", "method": "tool.result", "params": {"session": "tab-42", "call": "sh", "args": {"cmd": "bun test src/price.test.ts"}, "result": {"out": "...", "err": "...", "code": 1}}}
{"id": "4", "result": {"knownFailures": ["discount > rounds"], "note": "1 failing test(s) also fail on a clean checkout ..."}}
```

- Send every `sh` call with its `session`, `args.cmd` and `result.code`; the output is only needed for test runs. Stem learns which commands follow edits in which part of the project. Looking around (`ls`, `git`, `grep`), one-off scripts and anything in `/tmp` are ignored.
- For `test` calls and `sh` calls that run a test runner, Stem reads the output. With `"clean": true` (this session has changed nothing in the working tree) Stem learns which tests fail before any change; a test that failed on two clean runs is reported as `knownFailures` in later runs. Anything else answers `{}`.
- `result` may be a string (treated as `out`) or an object with `out` and `err`.

### `turn.end`

When the model's reply is done.

```json
{"id": "5", "method": "turn.end", "params": {"session": "tab-42", "answer": "Fixed total() ...", "finished": true, "origin": "user", "edits": [{"file": "src/price.ts", "before": "...", "after": "..."}], "touched": ["src/price.ts"], "tests": ["src/price.test.ts"], "usage": {"model": "claude-sonnet-5-5", "cost": 0.42, "inputTokens": 81000, "outputTokens": 2300, "cacheRead": 64000, "cacheWrite": 9000, "steps": 9, "toolOutputChars": 30400, "repeatReads": 3, "durationMs": 61000}}}
{"id": "5", "result": {"request": "the cart total is wrong", "edits": 1, "files": ["src/price.ts"], "kinds": ["regress"], "withheld": [], "followed": ["regress"], "usageStored": true}}
```

- `finished`: false when the turn stopped early. Stem keeps it as work to resume.
- `edits` and `touched`: what this session changed. Send them when several sessions share one working tree, so each learns only its own changes. When left out, Stem reads the git diff since `turn.start`.
- `tests`: test files the turn ran.
- `usage`: what the turn cost. Every field is optional. A row is stored for every `turn.end`, whatever its `origin` and even without `usage`, with the session's task and the kinds of note the turn got or had withheld; `usageStored` says so. `inputTokens` counts everything sent to the model over the turn, cache reads and cache writes included. `cost` is the list price of those tokens, not money paid: on a subscription nothing is charged per turn. Leave `cost` out when the price is unknown; a missing cost is left out of the medians, a cost of 0 counts as 0.
- `shown`: false when the app did not give Stem's note to the model (a shadow run, a withheld note). Leave it out when it did.
- `kinds` in the answer: every kind of note this turn got, at the start or with a tool result. `withheld`: the kinds Stem worked out and held back. `followed`: the ones, shown or withheld, that the turn bore out:
  - `familiar`: the session wrote one of the files the note named.
  - `regress`: the earlier fix is back in the file at turn end.
  - `answer`: the turn changed no file and either finished in two steps or fewer, or its answer cites a file the earlier answer cited.
  - `habits`: one of the offered commands was run later in the turn.
  - `resume`: the session wrote a file the stopped work had touched (or, when it had touched none, the turn finished). `trail`: the turn finished with `repeatReads` 0.
  - `lessons` and `known` have no such test.
- An `engine` turn that continues a task is learned from like the user's own, except that it never stores an answer. A turn of any other origin, and an `engine` turn with no task, answer `{"skipped": ..., "usageStored": ...}`: nothing is learned, the usage is still stored.

### `feedback`

When the person says a fix was wrong. Stem forgets that fix and keeps the correction as a lesson for similar requests.

```json
{"id": "6", "method": "feedback", "params": {"session": "tab-42", "text": "the rounding has to happen per line, not on the total"}}
```

### `status`

What Stem knows about this project, for a panel or a status chip.

```json
{"id": "7", "method": "status"}
{"id": "7", "result": {"sessions": 214, "fixes": 61, "answers": 9, "lessons": 2, "knownFailures": 1, "familiarWords": 340, "testCommand": "bun test <files>", "resume": false,
  "habits": [{"area": "packages/web", "turns": 59, "checks": [{"cmd": "npm run lint", "runs": 14, "passed": 11}]}],
  "usage": {
    "turns": 180,
    "byNote": {"none": {"turns": 120, "medianCost": 0.9, "medianTokens": 52000, "medianFreshTokens": 6100, "medianSteps": 5, "medianRepeatReads": 1},
               "familiar": {"turns": 41, "medianCost": 1.4, "medianTokens": 70000, "medianFreshTokens": 8800, "medianSteps": 9, "medianRepeatReads": 2, "followed": 17}},
    "notShown": {"familiar": {"turns": 12, "medianCost": 1.6, "medianTokens": 74000, "medianFreshTokens": 9100, "medianSteps": 10, "medianRepeatReads": 3, "followed": 0}},
    "kinds": {"familiar": {"turns": 30, "followed": 5, "state": "withheld", "needsTurns": 10, "needsRate": 0.5}, "habits": {"turns": 22, "followed": 17, "state": "proven", "needsTurns": 10, "needsRate": 0.5},
              "regress": {"turns": 0, "followed": 0, "state": "shown until judged", "needsTurns": 10, "needsRate": 0.5}, "answer": {"turns": 0, "followed": 0, "state": "shown until judged", "needsTurns": 10, "needsRate": 0.5}},
    "tasks": {"count": 64, "unassigned": {"turns": 20, "cost": 31.4}, "byNote": {"none": {"tasks": 40, "medianCost": 2.1, "medianTurns": 2, "medianSteps": 11}}, "notShown": {}}}}}
```

- `usage.byNote`: turns whose note reached the model, under every kind they got (`familiar`, `answer`, `regress`, `resume`, `lessons`, `trail`, `habits`, `known`). `none`: turns where Stem had nothing to say.
- `usage.notShown`: turns where Stem had a note of that kind and it was not shown, by the app (`shown: false`) or by Stem (`withheld`). Same kind of turn, no note: the fair side to compare `byNote` with.
- `usage.kinds`: the record of each kind that has to earn being shown. `state` is `probation` (withheld, fewer than `needsTurns`), `withheld` (judged, right less often than `needsRate`), `shown until judged`, or `proven`.
- `usage.tasks.unassigned`: turns that belong to no task, such as messages into a conversation where the user never asked anything.
- `ruleHits`: how many calls a rule redirected or blocked in those turns.
- `medianTokens` is input plus output. `medianFreshTokens` leaves cache reads out: input minus `cacheRead`, plus output. Medians of an even count are the mean of the two middle values. `followed` is a count of turns.
- `usage.tasks`: the same split per task. A task is one request in one session with every turn that followed it, the app's own continuations included; its cost is the sum of its turns.
- Costs are list prices (see `turn.end`), so read them as size, not as a bill.

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

On 97 real editing turns from one large mixed-language project, replayed in order through `stem host` (the first 30% only for learning): Stem offered usual checks on 36 of 68 turns, and in 22 of those 36 the turn went on to run at least one of them; 43 of the 87 commands offered were run. The log those turns come from cuts long scripts short, so the real numbers are higher.

On 141 real requests from that project, asked after Stem had seen 398 earlier ones and with its recent fixes reverted in the code: 4 got an "earlier fix was undone" note at the start (3 were the fix's own request again, 1 named the fixed file), 2 got an earlier answer (both the same question again), none got feedback or a familiar-files note. The first version of these rules gave 19 fix notes and 22 feedback notes on the same requests, nearly all wrong.

Two days of real use in that project, with every note withheld, is why kinds have to earn being shown: the usual checks were borne out in 12 of 17 turns, the familiar-files note in 4 of 27.

## Replaying recorded turns

`tools/replay.ts` feeds turns an app recorded earlier through `stem host`, in order, into a fresh store on a throwaway clone of the project, and prints how often each kind of note was borne out. Nothing reaches a model and the project is not touched, so an app can learn where each kind stands before any note is shown: `bun tools/replay.ts <project> <turns.jsonl> [path to stem]`.

One JSON object per line, oldest first. Only `session` and `prompt` are required:

```json
{"session": "tab-42", "origin": "user", "prompt": "the cart total is wrong", "task": "the cart total is wrong", "files": ["src/price.ts"], "reads": ["src/price.ts", "src/cart.ts"], "commands": [{"cmd": "npm test", "code": 0, "afterFirstWrite": true}], "edits": [{"file": "src/price.ts", "before": "...", "after": "..."}], "answer": "Fixed total().", "finished": true, "steps": 9, "cost": 0.42, "inputTokens": 81000, "cacheRead": 64000, "outputTokens": 2300, "repeatReads": 3}
```

- `files`: what the session wrote, in order of first write. `reads`: what it read. `commands`: what it ran, with `afterFirstWrite` true for those that came after its first write.
- `commandsPartial: true` marks a turn whose commands could not all be recorded; it is left out of the usual-checks rate. `stem` (any label, such as `off` or `on`) splits the rates by that label.
- Without `edits`, earlier fixes cannot be learned, so the undone-fix note never fires in the replay.

## Reference host

`tests/host.ts` drives `stem host` end to end on a fresh repo: `bun tests/host.ts [path to stem]`. `tests/smoke.sh` runs it too. It checks every method, a `decide` that is accepted, declined and never answered, follow-ups and the task so far, the app's own continuations, messages from other tabs, rules, notes on opening a file, usual checks, known failures, import, usage against an independent recount, an app that reports its own edits, and kinds being withheld until they earn being shown.
