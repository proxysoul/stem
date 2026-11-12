import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
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

const hello = await call("hello", { protocol: 1, host: { name: "reference" }, capabilities: ["decide"], decideMs: 2000, show: "all" });
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
answerDecide = () => ({ choice: null, confidence: 0 });

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
check("feedback on an earlier fix comes back when its file is edited", firstHabit.includes("round tax per line") && !secondHabit.includes("round tax per line"), { firstHabit, secondHabit });
check("a check command is not suggested before three turns used it", [firstHabit, secondHabit, earlyHabit].every((n) => !n.includes("checked their work")), { firstHabit, secondHabit, earlyHabit });
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
check("status reports turns, median cost and tokens per note kind", measured.result?.usage?.turns >= 5 && byNote.habits?.turns === 2 && byNote.habits.medianCost === 0.5 && byNote.habits.medianTokens === 1000 && byNote.habits.medianRepeatReads === 2 && byNote.habits.followed === 1 && byNote.trail?.turns === 1 && byNote.trail.medianSteps === 1, { habits: byNote.habits, trail: byNote.trail, turns: measured.result?.usage?.turns });
check("status lists the project's usual checks", measured.result?.habits?.[0]?.area === "src" && measured.result.habits[0].checks[0].cmd === "cd src && bun run lint <file>", measured.result?.habits);

const usageRows = () => readFileSync(join(dir, ".stem", "learn", "usage.jsonl"), "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
const middle = (values: number[]) => {
  const sorted = values.filter((v) => typeof v === "number").sort((x, y) => x - y);
  if (!sorted.length) return null;
  return sorted.length % 2 ? sorted[(sorted.length - 1) / 2] : (sorted[sorted.length / 2 - 1] + sorted[sorted.length / 2]) / 2;
};

const fixesBefore = (await call("status")).result.fixes;
await call("turn.start", { session: "tab-e1", prompt: "tax helper should use the new quarter rate", origin: "user" });
await call("turn.end", { session: "tab-e1", answer: "Looked at src/tax.ts, not changed yet.", finished: true, edits: [], touched: [], usage: { cost: 1, steps: 3, inputTokens: 1000, cacheRead: 900, outputTokens: 50 } });
const engineStart = await call("turn.start", { session: "tab-e1", prompt: "System notice: your answer listed work that is not done. Continue.", origin: "engine" });
const oldTax = "export const tax = (n: number) => n * 0.2\n";
const newTax = "export const tax = (n: number) => n * 0.25\n";
writeFileSync(join(dir, "src", "tax.ts"), newTax);
const engineEnd = await call("turn.end", { session: "tab-e1", origin: "engine", answer: "Changed the rate.", finished: true, edits: [{ file: "src/tax.ts", before: oldTax, after: newTax }], touched: ["src/tax.ts"], usage: { cost: 2, steps: 5, inputTokens: 3000, cacheRead: 2500, outputTokens: 100 } });
const fixesAfter = (await call("status")).result.fixes;
check("the app's own continuation of a task is learned under that task", engineStart.result?.continues === true && engineEnd.result?.edits === 1 && engineEnd.result.usageStored === true && fixesAfter === fixesBefore + 1, { engineStart, engineEnd, fixesBefore, fixesAfter });
const linkEnd2 = await call("turn.end", { session: "tab-e1", origin: "link", answer: "noted", shown: false, usage: { cost: 0.5, steps: 1 } });
const taskRows = usageRows().filter((r) => r.session === "tab-e1");
check("every turn's usage is stored with its task, whoever started the turn", !!linkEnd2.result?.skipped && linkEnd2.result.usageStored === true && taskRows.length === 3 && taskRows.map((r) => r.origin).join() === "user,engine,link" && new Set(taskRows.map((r) => r.task)).size === 1 && taskRows[0].task > 0 && taskRows[0].at > 1e12 && taskRows[2].shown === false && taskRows[0].shown === true, taskRows);
const orphan = await call("turn.start", { session: "tab-e2", prompt: "System notice: continue", origin: "engine" });
check("a continuation with no task behind it starts nothing", !!orphan.result?.skipped && !orphan.result.continues, orphan);

const withheldStart = await call("turn.start", { session: "tab-s1", prompt: "tax helper should accept a custom rate", origin: "user" });
const withheldKind = withheldStart.result?.kinds?.[0];
const shownBefore = (await call("status")).result.usage.byNote[withheldKind]?.turns ?? 0;
await call("turn.end", { session: "tab-s1", answer: "", finished: true, edits: [], touched: [], shown: false, usage: { cost: 9, steps: 4 } });
const afterWithheld = (await call("status")).result.usage;
check("a note the app did not show is counted apart from shown notes", !!withheldKind && afterWithheld.notShown[withheldKind]?.turns === 1 && afterWithheld.notShown[withheldKind].medianCost === 9 && (afterWithheld.byNote[withheldKind]?.turns ?? 0) === shownBefore, { withheldStart, afterWithheld });

const recount: Record<string, any[]> = {};
for (const row of usageRows()) {
  const where = row.kinds.length === 0 ? ["none"] : row.shown === false ? [] : row.kinds;
  for (const kind of where) (recount[kind] ??= []).push(row);
}
const reported = afterWithheld.byNote;
const agrees = Object.entries(recount).every(([kind, rows]) => {
  const s = reported[kind];
  return (
    s?.turns === rows.length &&
    s.medianCost === middle(rows.map((r) => r.cost)) &&
    s.medianTokens === middle(rows.filter((r) => r.inputTokens != null).map((r) => r.inputTokens + (r.outputTokens ?? 0))) &&
    s.medianFreshTokens === middle(rows.filter((r) => r.inputTokens != null).map((r) => Math.max(0, r.inputTokens - (r.cacheRead ?? 0)) + (r.outputTokens ?? 0))) &&
    s.medianSteps === middle(rows.map((r) => r.steps))
  );
});
check("status usage equals an independent recount of the usage log", agrees && Object.keys(reported).length === Object.keys(recount).length && afterWithheld.turns === usageRows().length, { reported, recount: Object.fromEntries(Object.entries(recount).map(([k, r]) => [k, r.length])) });
const e1 = afterWithheld.tasks;
check("status also reports cost per task", e1.count >= 2 && typeof e1.byNote === "object" && Object.values(e1.byNote).every((s: any) => s.tasks > 0), e1);

answerDecide = () => ({ choice: null, confidence: 0 });
writeFileSync(join(dir, "src", "tax.ts"), oldTax);
const elsewhere = await call("turn.start", { session: "tab-r1", prompt: "rendre le bouton plus grand", origin: "user" });
const onRead = await call("rule.check", { session: "tab-r1", call: "read", args: { path: "src/tax.ts" } });
const onSecondRead = await call("rule.check", { session: "tab-r1", call: "read", args: { path: "src/tax.ts" } });
writeFileSync(join(dir, "src", "tax.ts"), newTax);
const reapplied = await call("turn.end", { session: "tab-r1", answer: "", finished: true, edits: [], touched: ["src/tax.ts"], usage: { cost: 1, steps: 2 } });
check("an undone fix is pointed out when its file is opened, once, whatever the request says", !/again has/.test(elsewhere.result?.note ?? "") && /src\/tax\.ts:1 again has code that an earlier fix replaced/.test(onRead.result?.note ?? "") && onRead.result.action === "run" && !onSecondRead.result?.note, { elsewhere, onRead, onSecondRead });
check("turn end says which notes were acted on", reapplied.result?.kinds?.includes("regress") && reapplied.result.followed?.includes("regress"), reapplied);

await call("turn.start", { session: "tab-t1", prompt: "continue", origin: "user", task: "Make the tax helper configurable per country" });
await call("turn.end", { session: "tab-t1", answer: "Did the first half.", finished: true, edits: [], touched: ["src/tax.ts"] });
const hinted = await call("turn.start", { session: "tab-t1", prompt: "continue", origin: "user" });
check("a follow-up in a conversation Stem has not seen takes the app's task for it", /continues the task: Make the tax helper configurable per country/.test(hinted.result?.note ?? ""), hinted);
await call("turn.end", { session: "tab-t1", answer: "", finished: true, edits: [], touched: [] });

const probed = async (session: string, file: string) => {
  await call("turn.start", { session, prompt: `rework the parser module for case ${session}`, origin: "user" });
  await call("tool.result", { session, call: "sh", args: { cmd: "sysprobe --memory" }, result: { code: 0 } });
  const first = await call("rule.check", { session, call: "edit", args: { path: file } });
  await call("tool.result", { session, call: "sh", args: { cmd: "sysprobe --memory" }, result: { code: 0 } });
  await call("tool.result", { session, call: "sh", args: { cmd: "make parser-check" }, result: { code: 0 } });
  await call("turn.end", { session, answer: "done", finished: true, edits: [], touched: [file] });
  return String(first.result?.note ?? "");
};
for (const session of ["tab-p1", "tab-p2", "tab-p3"]) await probed(session, "lib/parser/tokens.ts");
const afterProbes = await probed("tab-p4", "lib/parser/tokens.ts");
check("a command run as often before an edit as after it is not a usual check", afterProbes.includes("make parser-check") && !afterProbes.includes("sysprobe"), afterProbes);

await call("turn.start", { session: "tab-u1", prompt: "migrate the billing exporter to the streaming writer", origin: "user" });
await call("turn.end", { session: "tab-u1", answer: "", finished: false, edits: [], touched: ["src/price.ts"] });
const otherConversation = await call("turn.start", { session: "tab-u2", prompt: "migrate the billing exporter to the streaming writer too", origin: "user" });
check("work stopped in one conversation is not offered to another", !otherConversation.result?.kinds?.includes("resume") && !/stopped/i.test(otherConversation.result?.note ?? ""), otherConversation);
await call("turn.end", { session: "tab-u2", answer: "", finished: true, edits: [], touched: [] });

const bad = await call("nope");
check("unknown methods return an error, not a crash", !!bad.error?.message, bad);

await call("shutdown");

const second = spawn(stem, ["host"], { cwd: dir, stdio: ["pipe", "pipe", "inherit"] });
const secondWaiting = new Map<string, (v: any) => void>();
createInterface({ input: second.stdout }).on("line", (line) => {
  const msg = JSON.parse(line);
  secondWaiting.get(String(msg.id))?.(msg);
});
const call2 = (method: string, params: object = {}) =>
  new Promise<any>((resolve) => {
    const id = String(++next);
    secondWaiting.set(id, resolve);
    second.stdin.write(JSON.stringify({ id, method, params }) + "\n");
  });
await call2("hello", { protocol: 1, capabilities: ["edits"] });
await call2("turn.start", { session: "tab-x1", prompt: "price helper should round half up everywhere", origin: "user" });
const pending = JSON.parse(readFileSync(join(dir, ".stem", "learn", "pending", "tab-x1.json"), "utf8"));
writeFileSync(join(dir, "src", "tax.ts"), "export const tax = (n: number) => n * 0.3\n");
const ownOnly = await call2("turn.end", { session: "tab-x1", answer: "", finished: true });
check("an app that reports its own edits gets no git snapshot and no diff of other work", pending.base === null && ownOnly.result?.edits === 0 && ownOnly.result.files.length === 0, { base: pending.base, ownOnly });
await call2("shutdown");

const third = spawn(stem, ["host"], { cwd: dir, stdio: ["pipe", "pipe", "inherit"] });
const thirdWaiting = new Map<string, (v: any) => void>();
createInterface({ input: third.stdout }).on("line", (line) => {
  const msg = JSON.parse(line);
  thirdWaiting.get(String(msg.id))?.(msg);
});
const call3 = (method: string, params: object = {}) =>
  new Promise<any>((resolve) => {
    const id = String(++next);
    thirdWaiting.set(id, resolve);
    third.stdin.write(JSON.stringify({ id, method, params }) + "\n");
  });
await call3("hello", { protocol: 1 });
const earning = (await call3("status")).result.usage.kinds;
check("status says which kinds of note have earned being shown", earning.familiar?.state === "probation" && earning.habits?.state === "probation" && earning.regress?.state === "shown until judged" && earning.answer?.state === "shown until judged" && earning.familiar.needsTurns === 10, earning);
const gated = await call3("turn.start", { session: "tab-g1", prompt: "tax helper should accept a custom rate", origin: "user" });
const gatedEdit = await call3("rule.check", { session: "tab-g1", call: "edit", args: { path: "src/price.ts" } });
const gatedEnd = await call3("turn.end", { session: "tab-g1", answer: "", finished: true, edits: [], touched: [] });
check(
  "a kind that has not earned it is worked out and withheld, at the start and at the edit",
  gated.result?.withheld?.includes("familiar") && !gated.result.kinds.includes("familiar") && !/earlier work in this project/.test(gated.result.note ?? "") && !/checked their work/.test(gatedEdit.result?.note ?? "") && gatedEnd.result?.withheld?.includes("habits") && gatedEnd.result.withheld.includes("familiar"),
  { gated, gatedEdit, gatedEnd },
);
const usageFile = join(dir, ".stem", "learn", "usage.jsonl");
for (let i = 0; i < 10; i++) appendFileSync(usageFile, JSON.stringify({ at: Date.now(), session: `earned-${i}`, task: null, origin: "user", kinds: [], withheld: ["habits"], followed: ["habits"], shown: true, finished: true }) + "\n");
const earned = (await call3("status")).result.usage.kinds.habits;
await call3("turn.start", { session: "tab-g2", prompt: "tax helper should reject negative amounts", origin: "user" });
const shownEdit = await call3("rule.check", { session: "tab-g2", call: "edit", args: { path: "src/price.ts" } });
const shownEnd = await call3("turn.end", { session: "tab-g2", answer: "", finished: true, edits: [], touched: [] });
check("a kind that was right in at least half of ten turns is shown from then on", earned.state === "proven" && /checked their work/.test(shownEdit.result?.note ?? "") && shownEnd.result?.kinds?.includes("habits") && !shownEnd.result.withheld.includes("habits"), { earned, shownEdit, shownEnd });
await call3("turn.start", { session: "tab-g3", prompt: "System notice: continue the work", origin: "engine", task: "Add a discount helper next to the tax helper" });
writeFileSync(join(dir, "src", "discount.ts"), "export const discount = (n: number) => n * 0.9\n");
const hintedEngine = await call3("turn.end", { session: "tab-g3", origin: "engine", answer: "Added it.", finished: true, edits: [], touched: ["src/discount.ts"] });
const afterEngine = await call3("turn.start", { session: "tab-g3", prompt: "continue", origin: "user" });
check("the app's continuation in a conversation Stem has not seen is learned under the app's task", hintedEngine.result?.files?.includes("src/discount.ts") && !hintedEngine.result.skipped && /continues the task: Add a discount helper/.test(afterEngine.result?.note ?? ""), { hintedEngine, afterEngine });
await call3("shutdown");
const failed = checks.filter(([, ok]) => !ok);
for (const [name, ok, seen] of checks) console.log(`${ok ? "ok  " : "FAIL"} ${name}${seen ? " " + JSON.stringify(seen).slice(0, 400) : ""}`);
console.log(`${checks.length - failed.length}/${checks.length} passed`);
process.exit(failed.length ? 1 : 0);
