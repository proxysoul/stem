import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

type Command = { cmd: string; code?: number; afterFirstWrite?: boolean };
type Turn = {
  session: string;
  origin?: string;
  prompt: string;
  task?: string;
  files?: string[];
  reads?: string[];
  commands?: Command[];
  edits?: { file: string; before: string; after: string }[];
  answer?: string;
  finished?: boolean;
  steps?: number;
  cost?: number;
  inputTokens?: number;
  cacheRead?: number;
  outputTokens?: number;
  repeatReads?: number;
};

const [project, turnsFile, stem = "stem"] = process.argv.slice(2);
if (!project || !turnsFile) {
  console.error("usage: bun core/tools/replay.ts <project> <turns.jsonl> [stem]\nReplays recorded turns, in order, into a fresh Stem store on a throwaway clone of <project>, and prints how often each kind of note was borne out by what the turn then did. Nothing is shown to a model and <project> is not touched.");
  process.exit(2);
}
const READS_PER_TURN = 40;
const turns: Turn[] = readFileSync(turnsFile, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
const dir = join(mkdtempSync(join(tmpdir(), "stem-replay-")), "repo");
const cloned = spawnSync("git", ["clone", "-q", "--shared", project, dir]);
if (cloned.status !== 0) {
  console.error(`could not clone ${project}: ${cloned.stderr}`);
  process.exit(1);
}

const child = spawn(stem, ["host"], { cwd: dir, stdio: ["pipe", "pipe", "inherit"] });
const waiting = new Map<string, (message: { result?: any }) => void>();
createInterface({ input: child.stdout }).on("line", (line) => {
  const message = JSON.parse(line);
  waiting.get(String(message.id))?.(message);
});
let next = 0;
const call = (method: string, params: object = {}) =>
  new Promise<any>((resolve) => {
    const id = String(++next);
    waiting.set(id, (message) => resolve(message.result ?? {}));
    child.stdin.write(JSON.stringify({ id, method, params }) + "\n");
  });

await call("hello", { protocol: 1, capabilities: ["edits"] });
const offered: Record<string, number> = {};
const count = (kinds: string[] = []) => kinds.forEach((kind) => (offered[kind] = (offered[kind] ?? 0) + 1));
for (const turn of turns) {
  const { session, origin = "user" } = turn;
  const commands = turn.commands ?? [];
  await call("turn.start", { session, prompt: turn.prompt, origin, task: turn.task });
  const ran = (command: Command) => call("tool.result", { session, call: "sh", args: { cmd: command.cmd }, result: { code: command.code } });
  for (const command of commands.filter((c) => !c.afterFirstWrite)) await ran(command);
  for (const path of [...new Set(turn.reads ?? [])].slice(0, READS_PER_TURN)) await call("rule.check", { session, call: "read", args: { path } });
  for (const path of turn.files ?? []) await call("rule.check", { session, call: "edit", args: { path } });
  for (const command of commands.filter((c) => c.afterFirstWrite)) await ran(command);
  const usage = { steps: turn.steps, cost: turn.cost, inputTokens: turn.inputTokens, cacheRead: turn.cacheRead, outputTokens: turn.outputTokens, repeatReads: turn.repeatReads };
  const ended = await call("turn.end", { session, origin, answer: turn.answer ?? "", finished: turn.finished ?? true, shown: false, touched: turn.files ?? [], edits: turn.edits ?? [], usage });
  count([...(ended.kinds ?? []), ...(ended.withheld ?? [])]);
}
const status = await call("status");
await call("shutdown");
rmSync(join(dir, ".."), { recursive: true, force: true });

const records: Record<string, { turns: number; followed: number; state: string }> = status.usage?.kinds ?? {};
const measured = Object.entries(status.usage?.notShown ?? {}).map(([kind, row]: [string, any]) => ({
  kind,
  turns: row.turns,
  borneOut: row.followed ?? null,
  rate: row.followed == null || !row.turns ? null : Math.round((row.followed / row.turns) * 100) / 100,
  state: records[kind]?.state ?? "always shown",
}));
console.log(JSON.stringify({ turns: turns.length, learned: { fixes: status.fixes, answers: status.answers, lessons: status.lessons, usualCheckAreas: (status.habits ?? []).length }, kinds: measured.sort((a, b) => b.turns - a.turns), usualChecks: (status.habits ?? []).slice(0, 5) }, null, 1));
