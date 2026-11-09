import { spawn, spawnSync } from "node:child_process";
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

const bad = await call("nope");
check("unknown methods return an error, not a crash", !!bad.error?.message, bad);

await call("shutdown");
const failed = checks.filter(([, ok]) => !ok);
for (const [name, ok, seen] of checks) console.log(`${ok ? "ok  " : "FAIL"} ${name}${seen ? " " + JSON.stringify(seen).slice(0, 400) : ""}`);
console.log(`${checks.length - failed.length}/${checks.length} passed`);
process.exit(failed.length ? 1 : 0);
