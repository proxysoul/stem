import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

const stem = process.argv[2] ?? "stem";
const dir = mkdtempSync(join(tmpdir(), "stem-host-"));
const git = (...args: string[]) => spawnSync("git", args, { cwd: dir, encoding: "utf8" });
const price = join(dir, "src", "price.ts");
const right = "export function total(items: { price: number; qty: number }[]): number {\n  let sum = 0\n  for (const it of items) sum += it.price * it.qty\n  return sum\n}\n";
const wrong = right.replace("it.price * it.qty", "it.price + it.qty");
spawnSync("mkdir", ["-p", join(dir, "src")]);
writeFileSync(price, wrong);
git("init", "-q");
git("add", "-A");
git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init");

const child = spawn(stem, ["host"], { cwd: dir, stdio: ["pipe", "pipe", "inherit"] });
const lines = createInterface({ input: child.stdout });
const waiting = new Map<string, (v: any) => void>();
let answerDecide: (asked: any) => any = () => undefined;
const decided: any[] = [];
lines.on("line", (line) => {
  const msg = JSON.parse(line);
  if (msg.method === "decide") {
    decided.push(msg.params);
    const result = answerDecide(msg.params);
    if (result !== undefined) child.stdin.write(JSON.stringify({ id: msg.id, result }) + "\n");
    return;
  }
  waiting.get(String(msg.id))?.(msg);
});
let next = 0;
const call = (method: string, params: object = {}) =>
  new Promise<any>((resolve) => {
    const id = String(++next);
    waiting.set(id, resolve);
    child.stdin.write(JSON.stringify({ id, method, params }) + "\n");
  });

const checks: [string, boolean, unknown?][] = [];
const check = (name: string, ok: boolean, seen?: unknown) => checks.push([name, ok, ok ? undefined : seen]);

const hello = await call("hello", { protocol: 1, host: { name: "reference" }, capabilities: ["decide"], decideMs: 2000 });
check("hello speaks protocol 1 and offers decide", hello.result?.protocol === 1 && hello.result.callbacks.includes("decide"), hello);

const first = await call("turn.start", { session: "tab-a", prompt: "the cart total adds price and qty instead of multiplying them", origin: "user" });
check("first request has nothing to recall", first.result?.note == null, first);
writeFileSync(price, right);
const ended = await call("turn.end", { session: "tab-a", answer: "Fixed total() in src/price.ts to multiply.", finished: true, tests: ["src/price.test.ts"] });
check("turn end learns the edit from the diff", ended.result?.edits === 1, ended);
git("add", "-A");
git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "fix");
writeFileSync(price, wrong);

answerDecide = (asked) => (asked.kind === "fix" ? { choice: asked.options[0].id, confidence: 0.9 } : { choice: null, confidence: 0 });
const french = await call("turn.start", { session: "tab-b", prompt: "le montant du panier est faux", origin: "user" });
check("an unworded match goes to the host's decide", decided.some((d) => d.kind === "fix"), decided);
check("a decided match brings the earlier fix back", /again has the code/.test(french.result?.note ?? ""), french);
await call("turn.end", { session: "tab-b", answer: "", finished: true, edits: [], touched: [] });

decided.length = 0;
answerDecide = () => ({ choice: null, confidence: 0 });
const declined = await call("turn.start", { session: "tab-c", prompt: "rendre le bouton plus grand", origin: "user" });
check("a declined decision adds no fix note", !/again has the code/.test(declined.result?.note ?? ""), declined);
await call("turn.end", { session: "tab-c", answer: "", finished: true, edits: [], touched: [] });

answerDecide = () => undefined;
const t0 = Date.now();
const silent = await call("turn.start", { session: "tab-d", prompt: "la somme est fausse partout", origin: "user" });
const waited = Date.now() - t0;
check("a host that never answers decide costs at most decideMs", waited < 3500 && !/again has the code/.test(silent.result?.note ?? ""), { waited, silent });
await call("turn.end", { session: "tab-d", answer: "", finished: true, edits: [], touched: [] });

const follow = await call("turn.start", { session: "tab-a", prompt: "yeah cook it", origin: "user" });
check("a short follow-up gets no recalled fix or answer", !/again has the code|earlier answer/.test(follow.result?.note ?? ""), follow);
await call("turn.end", { session: "tab-a", answer: "", finished: true, edits: [], touched: [] });

const link = await call("turn.start", { session: "tab-a", prompt: "FYI from the analytics tab: committed engine.ts", origin: "link" });
check("messages from other tabs start no task", link.result?.note == null && !!link.result?.skipped, link);
const linkEnd = await call("turn.end", { session: "tab-a", answer: "ok", origin: "link" });
check("messages from other tabs are not learned from", !!linkEnd.result?.skipped, linkEnd);

const search = await call("rule.check", { call: "sh", args: { cmd: "git grep -n total src" } });
check("shell search is redirected to grep", search.result?.action === "redirect" && search.result.call === "grep" && search.result.args.pattern === "total" && search.result.args.path === "src", search);
const plain = await call("rule.check", { call: "sh", args: { cmd: "grep -l total src" } });
check("output-changing grep flags stay in the shell", plain.result?.action === "run" && plain.result.call === "sh", plain);

const tool = await call("tool.result", { session: "tab-a", call: "sh", args: { cmd: "ls" }, result: { out: "src" } });
check("non-test tool results pass through", JSON.stringify(tool.result) === "{}", tool);

const status = await call("status");
check("status counts what was learned", status.result?.fixes >= 1 && status.result.sessions >= 2, status);

writeFileSync(join(dir, "src", "tax.ts"), "export const tax = (n: number) => n * 0.2\n");
const sha = (file: string) => createHash("sha1").update(readFileSync(join(dir, file))).digest("hex").slice(0, 16);
const old = join(dir, "empryo-learn.json");
writeFileSync(
  old,
  JSON.stringify({
    version: 1,
    fixes: [
      { file: "src/tax.ts", before: "export const tax = (n: number) => n * 0.1\n", after: "export const tax = (n: number) => n * 0.2\n", request: "tax uses the old ten percent rate", at: 1 },
      { file: "src/tax.ts", before: "", after: "whole new file", request: "create the tax helper file", at: 2 },
      { file: "src/tax.ts", before: "x", after: "never applied here", request: "something that was undone later", at: 3 },
    ],
    answers: [
      { words: [], request: "which rate does the tax helper use", answer: "src/tax.ts uses 20%.", files: [["src/tax.ts", sha("src/tax.ts")]], at: 1 },
      { words: [], request: "yeah go on", answer: "src/tax.ts", files: [["src/tax.ts", sha("src/tax.ts")]], at: 2 },
      { words: [], request: "where is the price helper defined", answer: "src/price.ts", files: [["src/price.ts", "0000000000000000"]], at: 3 },
    ],
    lessons: [{ text: "round tax per line, not on the total", files: ["src/tax.ts"], words: ["round", "tax"], hashes: [["src/tax.ts", sha("src/tax.ts")]], session: 4, at: 1 }],
    failures: { "discount rounds": { test: "discount rounds", command: "bun test", seen: 2 }, "flaky once": { test: "flaky once", command: "bun test", seen: 1 } },
    resume: null,
    trails: { what: { "src/tax.ts": 9 } },
    last: {},
  }),
);
const imported = await call("learn.import", { path: old, format: "empryo-stem-1" });
check("learn.import keeps applied fixes, current answers, lessons and clean failures", ["fixes", "answers", "lessons", "knownFailures"].every((k) => imported.result?.[k] === 1), imported);
const again = await call("learn.import", { path: old, format: "empryo-stem-1" });
check("learn.import twice adds nothing new", ["fixes", "answers", "lessons", "knownFailures"].every((k) => again.result?.[k] === 0), again);
const unknown = await call("learn.import", { path: old, format: "other-9" });
check("learn.import refuses unknown formats", !!unknown.error?.message, unknown);
const recalled = await call("turn.start", { session: "tab-e", prompt: "which rate does the tax helper use", origin: "user" });
check("an imported answer comes back for the same question", /earlier answer/.test(recalled.result?.note ?? ""), recalled);
await call("turn.end", { session: "tab-e", answer: "", finished: true, edits: [], touched: [] });

const habitTurn = async (session: string, file: string, prompt: string) => {
  await call("turn.start", { session, prompt, origin: "user" });
  const before = await call("rule.check", { session, call: "edit", args: { path: file } });
  await call("tool.result", { session, call: "sh", args: { cmd: "ls src" }, result: { code: 0 } });
  await call("tool.result", { session, call: "sh", args: { cmd: `cd src && bun run lint ${file.split("/").pop()} 2>&1 | tail -5` }, result: { code: 0 } });
  await call("tool.result", { session, call: "sh", args: { cmd: "git status --short" }, result: { code: 0 } });
  await call("turn.end", { session, answer: "done", finished: true, edits: [], touched: [file], usage: { model: "m", cost: 0.5, inputTokens: 900, outputTokens: 100, steps: 7, repeatReads: 2 } });
  return String(before.result?.note ?? "");
};
const firstHabit = await habitTurn("tab-h1", "src/tax.ts", "change the tax helper rounding rule");
const secondHabit = await habitTurn("tab-h2", "src/price.ts", "make the price helper handle negative quantities");
const earlyHabit = await habitTurn("tab-h0", "src/price.ts", "price helper should round to two decimals");
const thirdHabit = await habitTurn("tab-h3", "src/tax.ts", "tax helper should accept a custom rate");
check("a check command is not suggested before three turns used it", firstHabit === "" && secondHabit === "" && earlyHabit === "", { firstHabit, secondHabit, earlyHabit });
check("after three turns the usual check is offered on the first edit there", thirdHabit.includes("cd src && bun run lint <file>") && !thirdHabit.includes("git status") && !thirdHabit.includes("ls src"), thirdHabit);
await call("turn.start", { session: "tab-h4", prompt: "tax helper should reject negative amounts", origin: "user" });
const once = await call("rule.check", { session: "tab-h4", call: "edit", args: { path: "src/tax.ts" } });
const twice = await call("rule.check", { session: "tab-h4", call: "write", args: { path: "src/price.ts" } });
check("the usual checks are offered once per area per turn", !!once.result?.note && !twice.result?.note, { once, twice });
await call("turn.end", { session: "tab-h4", answer: "", finished: true, edits: [], touched: [] });

const followUp = await call("turn.start", { session: "tab-h3", prompt: "continue", origin: "user" });
check("a follow-up gets the task so far: request, files changed, last result", /continues the task: tax helper should accept a custom rate/.test(followUp.result?.note ?? "") && /src\/tax\.ts/.test(followUp.result.note) && followUp.result.kinds.includes("trail"), followUp);
await call("turn.end", { session: "tab-h3", answer: "", finished: true, edits: [], touched: [], usage: { cost: 0.1, inputTokens: 100, outputTokens: 10, steps: 1 } });
const otherTab = await call("turn.start", { session: "tab-h9", prompt: "continue", origin: "user" });
check("another tab's follow-up does not get this tab's task", !/continues the task/.test(otherTab.result?.note ?? ""), otherTab);
await call("turn.end", { session: "tab-h9", answer: "", finished: true, edits: [], touched: [] });

const sameQuestion = await call("turn.start", { session: "tab-q", prompt: "which rate does the tax helper use", origin: "user" });
check("a sure earlier answer also comes as its own field", sameQuestion.result?.answer?.sure === true && sameQuestion.result.answer.files.includes("src/tax.ts") && sameQuestion.result.kinds.includes("answer"), sameQuestion);
await call("turn.end", { session: "tab-q", answer: "", finished: true, edits: [], touched: [] });

const failing = "(fail) discount > rounds half up [1.00ms]\n 1 fail";
const clean1 = await call("tool.result", { session: "tab-q", call: "sh", args: { cmd: "bun test src" }, result: { out: failing, code: 1 }, clean: true });
const clean2 = await call("tool.result", { session: "tab-q", call: "sh", args: { cmd: "bun test src" }, result: { out: failing, code: 1 }, clean: true });
const dirty = await call("tool.result", { session: "tab-q", call: "sh", args: { cmd: "bun test src" }, result: { out: failing, code: 1 } });
check("a test that fails twice on a clean tree is then reported as not caused by the edit", !clean1.result?.knownFailures && clean2.result?.knownFailures?.length === 1 && dirty.result?.knownFailures?.[0] === "discount > rounds half up" && /clean checkout/.test(dirty.result.note ?? ""), { clean1, clean2, dirty });

const measured = await call("status");
const byNote = measured.result?.usage?.byNote ?? {};
check("status reports turns, median cost and tokens per note kind", measured.result?.usage?.turns === 5 && byNote.none?.turns === 1 && byNote.none.medianCost === 0.5 && byNote.none.medianTokens === 1000 && byNote.none.medianRepeatReads === 2 && byNote.habits?.turns === 1 && byNote.trail?.turns === 1 && byNote.trail.medianSteps === 1, measured.result?.usage);
check("status lists the project's usual checks", measured.result?.habits?.[0]?.area === "src" && measured.result.habits[0].checks[0].cmd === "cd src && bun run lint <file>", measured.result?.habits);

const bad = await call("nope");
check("unknown methods return an error, not a crash", !!bad.error?.message, bad);

await call("shutdown");
const failed = checks.filter(([, ok]) => !ok);
for (const [name, ok, seen] of checks) console.log(`${ok ? "ok  " : "FAIL"} ${name}${seen ? " " + JSON.stringify(seen).slice(0, 400) : ""}`);
console.log(`${checks.length - failed.length}/${checks.length} passed`);
process.exit(failed.length ? 1 : 0);
