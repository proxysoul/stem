const req = await Bun.stdin.json();
const base = process.env.GRAFT_BASE ?? "https://api.llmgateway.io/v1";
const key = process.env.GRAFT_KEY ?? process.env.LLMGW_KEY;
const messages = [{ role: "system", content: req.system }];
for (const m of req.messages) {
  const text = m.content.filter((b) => b.type === "text").map((b) => b.text).join("\n");
  if (m.role === "assistant") {
    const calls = m.content.filter((b) => b.type === "tool_use").map((b) => ({ id: b.id, type: "function", function: { name: b.name, arguments: JSON.stringify(b.input) } }));
    messages.push({ role: "assistant", content: text || null, ...(calls.length ? { tool_calls: calls } : {}) });
  } else {
    for (const b of m.content) if (b.type === "tool_result") messages.push({ role: "tool", tool_call_id: b.tool_use_id, content: b.content });
    if (text) messages.push({ role: "user", content: text });
  }
}
const tools = req.tools.map((t) => ({ type: "function", function: { name: t.name, description: t.description, parameters: t.input_schema } }));
const res = await fetch(base + "/chat/completions", {
  method: "POST",
  headers: { "content-type": "application/json", authorization: "Bearer " + key },
  body: JSON.stringify({ model: req.model, messages, tools, max_tokens: req.max_tokens }),
});
const out = await res.json();
if (!res.ok) {
  console.log(JSON.stringify({ error: out.error?.message ?? res.statusText }));
  process.exit(0);
}
const msg = out.choices[0].message;
const content = [];
if (msg.content) content.push({ type: "text", text: msg.content });
for (const c of msg.tool_calls ?? []) content.push({ type: "tool_use", id: c.id, name: c.function.name, input: JSON.parse(c.function.arguments || "{}") });
const cached = out.usage?.prompt_tokens_details?.cached_tokens ?? 0;
console.log(JSON.stringify({
  content,
  stop_reason: msg.tool_calls?.length ? "tool_use" : out.choices[0].finish_reason === "length" ? "max_tokens" : "end_turn",
  usage: { input_tokens: (out.usage?.prompt_tokens ?? 0) - cached, output_tokens: out.usage?.completion_tokens ?? 0, cache_read_input_tokens: cached },
}));
