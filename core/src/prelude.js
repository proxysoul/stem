globalThis.state = globalThis.state ?? {};
globalThis.__logs = [];
const __fmt = (v) => (typeof v === "string" ? v : (() => { try { return JSON.stringify(v); } catch { return String(v); } })());
globalThis.console = {
  log: (...a) => { __logs.push(a.map(__fmt).join(" ")); },
  error: (...a) => { __logs.push("error: " + a.map(__fmt).join(" ")); },
  warn: (...a) => { __logs.push("warn: " + a.map(__fmt).join(" ")); },
};
const __call = (name, args) => {
  const r = JSON.parse(__host(name, JSON.stringify(args ?? {})));
  if (r.note) __logs.push("[stem] " + r.note);
  if (!r.ok) {
    const e = new Error(r.error);
    e.stemCall = name;
    throw e;
  }
  return r.value;
};
const __opts = (o) => (o && typeof o === "object" && !Array.isArray(o) ? o : {});
const __loc = (target, line, col) => {
  if (target && typeof target === "object") return target;
  if (line !== undefined) return { file: target, line, col };
  return { name: target };
};
const __shape = (rows, what) => new Proxy(rows, {
  get(t, k, r) {
    if (typeof k === "string" && !(k in t) && isNaN(Number(k)) && !["then", "toJSON", "constructor"].includes(k)) {
      throw new Error(`${what} returns an array of {${Object.keys(t[0] ?? { file: 1, line: 1, text: 1 }).join(", ")}}; it has no .${k}. Use rows.map(r => r.file) or rows[0].${Object.keys(t[0] ?? { file: 1 })[0]}.`);
    }
    return Reflect.get(t, k, r);
  },
});
const __base = {
  read: (path, from, to) => __call("read", { path, ...(typeof from === "object" ? from : { from, to }) }),
  write: (path, text) => __call("write", { path, text: String(text) }),
  remove: (paths) => __call("remove", { paths: Array.isArray(paths) ? paths : [paths] }),
  edit: (path, a, b) => {
    if (typeof a === "function") {
      const before = __call("read", { path });
      const after = a(before);
      if (typeof after !== "string") throw new Error("$.edit callback must return the new file text as a string");
      return __call("write", { path, text: after, edit: true });
    }
    if (a instanceof RegExp) {
      const before = __call("read", { path });
      const flags = a.flags.includes("g") ? a.flags : a.flags + "g";
      const count = (before.match(new RegExp(a.source, flags)) || []).length;
      if (count === 0) throw new Error(`$.edit: ${a} matches nothing in ${path}`);
      if (count > 1 && !a.global) throw new Error(`$.edit: ${a} matches ${count} places in ${path}; add the g flag to change all, or make it unique`);
      return __call("write", { path, text: before.replace(a, b), edit: true, replaced: a.global ? count : 1 });
    }
    if (Array.isArray(a)) return __call("edit", { path, edits: a.map((e) => (Array.isArray(e) ? { old: e[0], new: e[1] } : e)) });
    if (a && typeof a === "object") return __call("edit", { path, edits: [a] });
    return __call("edit", { path, edits: [{ old: String(a), new: String(b ?? "") }] });
  },
  files: (glob, opts) => __call("files", { glob: glob ?? null, ...__opts(opts) }),
  grep: (pattern, opts) => __shape(__call("grep", { pattern: pattern instanceof RegExp ? pattern.source : String(pattern), ignoreCase: pattern instanceof RegExp ? pattern.flags.includes("i") : undefined, ...(typeof opts === "string" ? { path: opts } : __opts(opts)) }), "$.grep"),
  sh: (cmd, opts) => __call("sh", { cmd: String(cmd), ...__opts(opts) }),
  project: () => __call("project", {}),
  testsFor: (files) => __call("testsFor", { files: Array.isArray(files) ? files : [files] }),
  test: (filter, opts) => __call("test", { filter: Array.isArray(filter) ? filter.join(" ") : filter ?? null, ...__opts(opts) }),
  run: (script, opts) => __call("run", { script: String(script), ...__opts(opts) }),
  defs: (target, line, col) => __shape(__call("defs", __loc(target, line, col)), "$.defs"),
  refs: (target, line, col) => __shape(__call("refs", __loc(target, line, col)), "$.refs"),
  callers: (target, line, col) => __shape(__call("callers", __loc(target, line, col)), "$.callers"),
  impls: (target, line, col) => __shape(__call("impls", __loc(target, line, col)), "$.impls"),
  typeAt: (target, line, col) => __call("hover", __loc(target, line, col)),
  rename: (target, to, opts) => __call("rename", { ...__loc(target), to: String(to), ...__opts(opts) }),
  symbols: (query) => __shape(__call("symbols", { query: String(query ?? "") }), "$.symbols"),
  outline: (path) => __call("outline", { path }),
  diagnostics: (path) => __shape(__call("diagnostics", { path: path ?? null }), "$.diagnostics"),
  sql: (query, ...params) => __call("sql", { query: String(query), params: params.length === 1 && Array.isArray(params[0]) ? params[0] : params }),
  eject: (ids, summary) => __call("eject", Array.isArray(ids) || typeof ids !== "object" || ids === null ? { ids: Array.isArray(ids) ? ids : [ids], summary: typeof summary === "string" ? summary : summary?.summary } : ids),
  context: () => __call("context", {}),
  help: (name) => __call("help", { name: name === undefined ? null : String(name) }),
  recall: (id) => __call("recall", { id }),
  plan: (...steps) => __call("plan", { steps: steps.flat() }),
  done: (which) => __call("done", { which: which === undefined ? null : which }),
  ask: (prompt, input) => __call("ask", { prompt: String(prompt), input: input === undefined ? null : __fmt(input) }),
};
__base.hover = __base.typeAt;
__base.references = __base.refs;
__base.definition = __base.defs;
__base.find = __base.symbols;
__base.exec = __base.sh;
__base.readFile = __base.read;
__base.delete = __base.remove;
__base.rm = __base.remove;
const __distance = (a, b) => {
  const d = Array.from({ length: a.length + 1 }, (_, i) => [i, ...Array(b.length).fill(0)]);
  for (let j = 1; j <= b.length; j++) d[0][j] = j;
  for (let i = 1; i <= a.length; i++) for (let j = 1; j <= b.length; j++) d[i][j] = Math.min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + (a[i - 1].toLowerCase() === b[j - 1].toLowerCase() ? 0 : 1));
  return d[a.length][b.length];
};
const __closest = (name) => Object.keys(__base).map((k) => [k, __distance(name, k) - (k.toLowerCase().includes(name.toLowerCase()) || name.toLowerCase().includes(k.toLowerCase()) ? 2 : 0)]).sort((x, y) => x[1] - y[1]).slice(0, 4).map(([k]) => "$." + k);
const __wrapTool = (name, fn, check, effects, verified) => (...args) => {
  if (!verified) {
    verified = true;
    if (effects.length === 0) {
      let ok = false;
      let detail = "returned false";
      try { ok = !!check(fn); } catch (e) { detail = String(e?.message ?? e); }
      const note = JSON.parse(__host("tool_checked", JSON.stringify({ name, ok, detail: ok ? "" : detail }))).value;
      if (note) __logs.push(note);
    }
  }
  try {
    const value = fn(...args);
    const size = Array.isArray(value) ? value.length : value && typeof value === "object" ? Object.keys(value).length : value ? 1 : 0;
    let chars = 0;
    try { chars = JSON.stringify(value)?.length ?? 0; } catch {}
    const note = JSON.parse(__host("tool_used", JSON.stringify({ name, ok: true, size, chars }))).value;
    if (note) __logs.push(note);
    return value;
  } catch (e) {
    __host("tool_used", JSON.stringify({ name, ok: false }));
    throw e;
  }
};
const __builtin = new Set([...Object.keys(__base), "save", "tools", "forget", "writeFile"]);
__base.save = (name, fn, opts = {}) => {
  if (__builtin.has(name)) throw new Error(`$.${name} is a built-in; save under another name`);
  if (typeof fn !== "function") throw new Error("$.save(name, fn, {doc, check}): fn must be a function that uses $");
  if (typeof opts.check !== "function") throw new Error("$.save needs {check: (f) => ...}: call f on a case you know and return true when the answer is right");
  let ok = false;
  try { ok = !!opts.check(fn); } catch (e) { throw new Error(`not saved: the self-check threw ${e?.message ?? e}`); }
  if (!ok) throw new Error("not saved: the self-check returned false");
  const result = __call("save", { name, code: fn.toString(), check: opts.check.toString(), doc: opts.doc ?? "", anchors: Array.isArray(opts.anchors) ? opts.anchors.map(String) : [] });
  __base[name] = __wrapTool(name, fn, opts.check, result.effects ?? [], true);
  return result;
};
__base.tools = () => __call("toolList", {});
__base.forget = (name, reason) => {
  if (__builtin.has(name)) throw new Error(`$.${name} is a built-in and cannot be forgotten`);
  const result = __call("forget", { name, reason });
  delete __base[name];
  return result;
};
for (const t of JSON.parse(__host("tools", "{}")).value ?? []) {
  if (__builtin.has(t.name)) {
    __host("tool_checked", JSON.stringify({ name: t.name, ok: false, detail: `does not load: it would replace the built-in $.${t.name}` }));
    continue;
  }
  try {
    __base[t.name] = __wrapTool(t.name, (0, eval)("(" + t.code + ")"), (0, eval)("(" + t.check + ")"), t.effects ?? [], false);
  } catch (e) {
    __host("tool_checked", JSON.stringify({ name: t.name, ok: false, detail: "does not load: " + String(e?.message ?? e) }));
  }
}
globalThis.$ = new Proxy(__base, {
  get(target, key, receiver) {
    if (typeof key !== "string" || key in target || ["then", "toJSON", "constructor", "valueOf", "toString"].includes(key)) return Reflect.get(target, key, receiver);
    return () => {
      throw new Error(`$.${key} does not exist. Closest: ${__closest(key).join(", ")}. Write it with the core functions; if you will need it again, save it with $.save("${key}", (args) => { ... }, {doc, check}).`);
    };
  },
});
$.writeFile = $.write;
globalThis.__run = async (fn) => {
  __logs.length = 0;
  try {
    const value = await fn();
    return JSON.stringify({ ok: true, value: value === undefined ? null : value, log: __logs.slice(0, 200) }, (k, v) => (typeof v === "bigint" ? String(v) : v instanceof Map ? Object.fromEntries(v) : v instanceof Set ? [...v] : v));
  } catch (e) {
    const stack = String(e?.stack ?? "");
    const m = stack.match(/script\.js:(\d+)/);
    return JSON.stringify({ ok: false, error: String(e?.message ?? e), call: e?.stemCall ?? null, line: m ? Number(m[1]) - 1 : null, log: __logs.slice(0, 200) });
  }
};
