#!/usr/bin/env bash
set -euo pipefail
STEM=${STEM:-stem}
HERE=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export STEM_HOME="$T/home"
mkdir "$T/repo"
pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; exit 1; }

cd "$T/repo"
git init -q
git config user.email smoke@stem
git config user.name smoke
mkdir src
printf 'export function add(a: number, b: number): number {\n  return a - b\n}\n' > src/math.ts
git add -A
git commit -qm init

[ "$(echo 'return $.grep("a - b", {path: "src"}).length' | "$STEM" js)" = "1" ] && pass "js runs a script against \$" || fail "js"

out=$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"js","arguments":{"code":"return $.read(\"src/math.ts\").includes(\"a - b\")"}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1999-01-01"}}}' \
  '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"sql","arguments":{"query":"select name from symbols where file = ?","params":["src/math.ts"]}}}' \
  | STEM_MCP_TOOLS= "$STEM" mcp)
[ "$(echo "$out" | wc -l | tr -d ' ')" = "6" ] && pass "mcp answers requests and skips notifications" || fail "mcp line count: $out"
echo "$out" | grep -q '"supportedVersions"' && pass "mcp server/discover (2026-07-28)" || fail "mcp discover"
for t in js defs refs callers impls symbols sql diagnostics task brief remember feedback; do echo "$out" | grep -q "\"name\":\"$t\"" || fail "mcp tool $t missing"; done && pass "mcp lists every tool"
echo "$out" | grep -q '"structuredContent":{"rows":\[{"name":"add"}\]}' && pass "mcp sql returns typed rows in structuredContent" || fail "mcp sql: $out"
[ "$(echo "$out" | grep -o '"outputSchema"' | wc -l | tr -d ' ')" = "12" ] && pass "every mcp tool declares an output schema" || fail "output schemas"
echo "$out" | grep -q '"text":"true"' && pass "mcp tools/call js" || fail "mcp js call"
echo "$out" | grep -q '"code":-32022' && pass "mcp rejects unknown protocol versions" || fail "mcp version check"

echo '{"session_id":"s1","prompt":"add() subtracts instead of adding; fix it"}' | "$STEM" hook prompt >/dev/null
printf 'export function add(a: number, b: number): number {\n  return a + b\n}\n' > src/math.ts
echo '{"session_id":"s1","last_assistant_message":"Fixed src/math.ts"}' | "$STEM" hook stop
state=$(find "$STEM_HOME/projects" -path '*/learn/state.json' | head -1)
grep -q 'return a + b' "$state" && pass "hook stop learns the fix from the git diff" || fail "hook stop"
[ ! -e .stem ] && [ -z "$(git status --porcelain --ignored | grep -v '^ M src/math.ts$')" ] && pass "nothing is written into the project" || fail "Stem wrote into the project: $(git status --porcelain --ignored)"

git commit -qam fix
printf 'export function add(a: number, b: number): number {\n  return a - b\n}\n' > src/math.ts
note=$(echo '{"session_id":"s2","prompt":"add() subtracts instead of adding again"}' | "$STEM" hook prompt)
echo "$note" | grep -q 'additionalContext' && echo "$note" | grep -q 'earlier fix' && pass "hook prompt brings back the undone fix" || fail "hook prompt: $note"
[ -z "$(echo '{"session_id":"s2","prompt":"add() subtracts instead of adding again"}' | "$STEM" hook prompt)" ] && pass "hook prompt fired twice gives context once" || fail "duplicate prompt"
git checkout -q src/math.ts
echo '{"session_id":"s2"}' | "$STEM" hook stop

"$STEM" integrate >/dev/null
for f in .claude/settings.json .mcp.json .claude/skills/stem/SKILL.md .codex/hooks.json .codex/config.toml .agents/skills/stem/SKILL.md .opencode/plugins/stem.ts .opencode/skills/stem/SKILL.md opencode.json .pi/extensions/stem.ts .empryo/hooks.json .empryo/config.json; do [ -f "$f" ] || fail "integrate did not write $f"; done
"$STEM" integrate >/dev/null
[ "$(grep -c 'stem hook prompt' .claude/settings.json)" = "1" ] && [ "$(grep -c '^\[mcp_servers.stem\]' .codex/config.toml)" = "1" ] && pass "integrate writes every harness and is idempotent" || fail "integrate duplicated hooks"
if command -v bun >/dev/null; then
  hosted=$(bun "$HERE/host.ts" "$STEM" 2>&1) && pass "stem host speaks the host protocol" || { echo "$hosted" | grep -v '^ok '; fail "stem host"; }
fi
echo "all smoke checks passed"
