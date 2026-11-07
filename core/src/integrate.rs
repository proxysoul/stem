use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const PROMPT_HOOK: &str = "stem hook prompt";
const STOP_HOOK: &str = "stem hook stop";
const MCP_TOOLS: &str = "js,defs,refs,callers,impls,symbols,sql,diagnostics,task,feedback";
const APPROVED: [&str; 10] = ["js", "defs", "refs", "callers", "impls", "symbols", "sql", "diagnostics", "task", "feedback"];
const DESCRIPTION: &str = "Stem: semantic code navigation (definitions, references, callers, implementations, diagnostics), SQL over symbols and git history, multi-step scripts against the project, and a cheap agent that learns this project.";

const SKILL: &str = r#"---
name: stem
description: Use Stem's tools for code navigation, project facts and cheap delegated tasks in this repository. Use when you need where a symbol is defined or used, who calls it, files that change together, compiler diagnostics, several file operations in one step, or to hand off a self-contained investigation or fix.
---

# Stem

Stem is connected as an MCP server named `stem`. It knows this project: before each request a hook gives you what Stem learned that applies (files earlier work changed, an earlier fix that was undone, feedback on a rejected fix, an earlier answer whose files are unchanged). Trust those notes: they are checked against the current code.

Pick the tool by the question:

| Question | Tool | Returns |
|---|---|---|
| Where is `X` defined? | `defs {name}` | `{rows: [{file, line, text}]}` |
| Who uses `X`? (semantic, not text) | `refs {name}` | `{rows: [{file, line, text}]}` |
| Which functions call `X`? | `callers {name}` | `{rows: [{file, line, text, caller}]}` |
| What implements this interface? | `impls {name}` | `{rows: [...]}` |
| Find symbols by name | `symbols {query}` | `{rows: [{file, line, text, kind}]}` |
| Type errors after an edit | `diagnostics {path}` | `{rows: [{file, line, text, severity}]}` |
| Files that change together, recent commits, symbol counts | `sql {query, params}` | `{rows: [...]}` |
| Several reads, searches and edits in one step | `js {code}` | the script's return value |
| A self-contained investigation or fix, done cheaply | `task {prompt}` | `{answer, files, cost}` |
| CI or review rejected the last change | `feedback {text}` | the stored lesson |

When a name matches several symbols, pass `file` and `line` to pick one.

SQL tables: `files(path, lang, size, mtime, lines)`, `symbols(file, name, kind, line, end_line, doc)`, `refs(file, name, kind, line)`, `commits(hash, date, author, subject)`, `commit_files(hash, path)`, `cochange(a, b, n)`. Example: `select b, n from cochange where a = ? order by n desc limit 5`.

`js` runs the body of an async function with one object, `$`: `$.read`, `$.grep`, `$.edit`, `$.write`, `$.sh`, `$.test`, `$.defs`, `$.refs`, `$.sql` and more. Chain calls, filter inside the script, and return only what you need. A failed call throws; catch it to go on.

Stem refuses destructive steps (deleting files, git reset, push, publishing) through these tools; do those yourself if the user asks.
"#;

const OPENCODE_PLUGIN: &str = r#"import type { Plugin } from "@opencode-ai/plugin"
import { execFileSync } from "node:child_process"

const stem = (args: string[], input: unknown, cwd: string): string => {
  try {
    return execFileSync("stem", args, { input: JSON.stringify(input), cwd, encoding: "utf8", timeout: 60_000, env: { ...process.env, STEM_YOLO: "0" } })
  } catch {
    return ""
  }
}

export const StemPlugin: Plugin = async ({ directory }) => {
  const notes = new Map<string, string>()
  return {
    "chat.message": async (input, output) => {
      const prompt = output.parts.map((p: any) => (p.type === "text" && !p.synthetic ? p.text : "")).join("\n").trim()
      if (!prompt) return
      const out = stem(["hook", "prompt"], { session_id: `opencode-${input.sessionID}`, cwd: directory, prompt }, directory)
      const note = out ? JSON.parse(out)?.hookSpecificOutput?.additionalContext : undefined
      if (note) notes.set(input.sessionID, note)
      else notes.delete(input.sessionID)
    },
    "experimental.chat.system.transform": async (input, output) => {
      const note = input.sessionID ? notes.get(input.sessionID) : undefined
      if (note) output.system.push(note)
    },
    event: async ({ event }) => {
      if (event.type !== "session.idle") return
      const id = (event as any).properties?.sessionID
      if (!id) return
      notes.delete(id)
      stem(["hook", "stop"], { session_id: `opencode-${id}`, cwd: directory }, directory)
    },
  }
}
"#;

const PI_EXTENSION: &str = r#"import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import { execFileSync } from "node:child_process"

const hook = (event: string, input: object, cwd: string): string => {
  try {
    return execFileSync("stem", ["hook", event], { input: JSON.stringify(input), cwd, encoding: "utf8", timeout: 60_000, env: { ...process.env, STEM_YOLO: "0" } })
  } catch {
    return ""
  }
}

export default function (pi: ExtensionAPI) {
  const session = `pi-${process.pid}-${Date.now()}`
  pi.registerMcpServer("stem", {
    command: "stem",
    args: ["mcp"],
    env: { STEM_MCP_TOOLS: "MCP_TOOLS_LIST", STEM_YOLO: "0" },
    exposure: "direct",
    description: "DESCRIPTION_TEXT",
  })
  pi.on("before_agent_start", async (event, ctx) => {
    if (event.messageType !== "user") return
    const out = hook("prompt", { session_id: session, cwd: ctx.cwd, prompt: event.prompt }, ctx.cwd)
    const note = out ? JSON.parse(out)?.hookSpecificOutput?.additionalContext : undefined
    if (note) return { systemPrompt: `${event.systemPrompt}\n\n${note}` }
  })
  pi.on("agent_end", async (event, ctx) => {
    const last = [...event.messages].reverse().find((m: any) => m.role === "assistant") as any
    const answer = Array.isArray(last?.content) ? last.content.filter((c: any) => c.type === "text").map((c: any) => c.text).join("\n") : ""
    hook("stop", { session_id: session, cwd: ctx.cwd, last_assistant_message: answer }, ctx.cwd)
  })
}
"#;

fn hook_entry(event: &str) -> Value {
    let (command, timeout) = if event == "UserPromptSubmit" { (PROMPT_HOOK, 30) } else { (STOP_HOOK, 60) };
    json!({"hooks": [{"type": "command", "command": command, "timeout": timeout}]})
}

fn read_json(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Ok(json!({})),
        Ok(t) => serde_json::from_str(&t).map_err(|e| format!("{} is not plain JSON ({e}); add Stem by hand", path.display())),
        Err(_) => Ok(json!({})),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(value).unwrap_or_default())).map_err(|e| e.to_string())
}

fn add_hooks(path: &Path) -> Result<String, String> {
    let mut v = read_json(path)?;
    if !v.is_object() {
        return Err(format!("{} is not a JSON object", path.display()));
    }
    let hooks = v.as_object_mut().unwrap().entry("hooks").or_insert_with(|| json!({}));
    let mut added = Vec::new();
    for event in ["UserPromptSubmit", "Stop"] {
        let list = hooks.as_object_mut().ok_or("hooks is not an object")?.entry(event).or_insert_with(|| json!([]));
        let present = list.to_string().contains("stem hook");
        if !present {
            list.as_array_mut().ok_or("hook list is not an array")?.push(hook_entry(event));
            added.push(event);
        }
    }
    write_json(path, &v)?;
    Ok(if added.is_empty() { format!("{}: hooks already there", path.display()) } else { format!("{}: {} hooks", path.display(), added.join(" and ")) })
}

fn add_mcp_object(path: &Path, key: &str, entry: Value) -> Result<String, String> {
    let mut v = read_json(path)?;
    let servers = v.as_object_mut().ok_or("not a JSON object")?.entry(key).or_insert_with(|| json!({}));
    match servers {
        Value::Object(map) => {
            map.insert("stem".into(), entry);
        }
        Value::Array(list) => {
            list.retain(|s| s["name"].as_str() != Some("stem"));
            let mut e = entry;
            e["name"] = json!("stem");
            list.push(e);
        }
        _ => return Err(format!("{key} in {} has an unexpected shape", path.display())),
    }
    write_json(path, &v)?;
    Ok(format!("{}: MCP server stem", path.display()))
}

fn write_file(path: &Path, text: &str) -> Result<String, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())?;
    Ok(format!("{}: written", path.display()))
}

fn codex_toml(path: &Path) -> Result<String, String> {
    let mut current = String::new();
    let mut skipping = false;
    for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
        let header = line.trim_start().starts_with('[');
        if header {
            skipping = line.trim() == "[mcp_servers.stem]" || line.trim().starts_with("[mcp_servers.stem.");
        }
        if !skipping {
            current.push_str(line);
            current.push('\n');
        }
    }
    let approvals: String = APPROVED.iter().map(|t| format!("\n[mcp_servers.stem.tools.{t}]\napproval_mode = \"approve\"\n")).collect();
    let block = format!("\n[mcp_servers.stem]\ncommand = \"stem\"\nargs = [\"mcp\"]\nenv = {{ STEM_MCP_TOOLS = \"{MCP_TOOLS}\", STEM_YOLO = \"0\" }}\n{approvals}");
    write_file(path, &format!("{}{block}", current.trim_end_matches('\n').to_string() + if current.is_empty() { "" } else { "\n" }))?;
    Ok(format!("{}: MCP server stem", path.display()))
}

fn stdio_entry() -> Value {
    json!({"command": "stem", "args": ["mcp"], "env": {"STEM_MCP_TOOLS": MCP_TOOLS, "STEM_YOLO": "0"}})
}

fn target(root: &Path, name: &str) -> Vec<Result<String, String>> {
    let p = |rel: &str| -> PathBuf { root.join(rel) };
    match name {
        "claude" => vec![add_hooks(&p(".claude/settings.json")), add_mcp_object(&p(".mcp.json"), "mcpServers", stdio_entry()), write_file(&p(".claude/skills/stem/SKILL.md"), SKILL)],
        "codex" => vec![add_hooks(&p(".codex/hooks.json")), codex_toml(&p(".codex/config.toml")), write_file(&p(".agents/skills/stem/SKILL.md"), SKILL)],
        "empryo" => {
            let mut entry = stdio_entry();
            entry["resident"] = json!(true);
            vec![add_hooks(&p(".empryo/hooks.json")), add_mcp_object(&p(".empryo/config.json"), "mcpServers", entry), write_file(&p(".agents/skills/stem/SKILL.md"), SKILL)]
        }
        "opencode" => {
            let config = if p("opencode.jsonc").is_file() { p("opencode.jsonc") } else { p("opencode.json") };
            vec![
                write_file(&p(".opencode/plugins/stem.ts"), OPENCODE_PLUGIN),
                write_file(&p(".opencode/skills/stem/SKILL.md"), SKILL),
                add_mcp_object(&config, "mcp", json!({"type": "local", "command": ["stem", "mcp"], "environment": {"STEM_MCP_TOOLS": MCP_TOOLS, "STEM_YOLO": "0"}, "enabled": true})),
            ]
        }
        "pi" => vec![
            write_file(&p(".pi/extensions/stem.ts"), &PI_EXTENSION.replace("MCP_TOOLS_LIST", MCP_TOOLS).replace("DESCRIPTION_TEXT", DESCRIPTION)),
            write_file(&p(".agents/skills/stem/SKILL.md"), SKILL),
        ],
        other => vec![Err(format!("unknown harness {other}; use claude, codex, opencode, pi, empryo or all"))],
    }
}

pub fn run(rest: &[String], root: &Path) -> i32 {
    let names: Vec<&str> = match rest.first().map(String::as_str) {
        None | Some("all") => vec!["claude", "codex", "opencode", "pi", "empryo"],
        Some(_) => rest.iter().map(String::as_str).collect(),
    };
    let mut failed = false;
    for name in names {
        for r in target(root, name) {
            match r {
                Ok(line) => println!("{name:<9} {}", line.replace(&format!("{}/", root.display()), "")),
                Err(e) => {
                    failed = true;
                    eprintln!("{name:<9} {e}");
                }
            }
        }
    }
    println!("\nEach harness now asks Stem what it knows when you send a request, and Stem learns from the git diff when the reply ends. Codex runs project hooks only after you trust them once with /hooks, and pi loads project extensions only after you trust the project. Stem's js tool is pre-approved in Codex because, with no one at Stem's keyboard, Stem refuses destructive steps (deleting files, git reset, push, publishing) instead of running them.");
    i32::from(failed)
}
