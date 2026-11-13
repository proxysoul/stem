import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import sharp from "sharp";
import opentype from "opentype.js";

const here = import.meta.dir;
const out = join(here, "readme");
const fontBytes = await readFile(join(here, "fonts/InstrumentSans-Bold.ttf"));
const font = opentype.parse(fontBytes.buffer.slice(fontBytes.byteOffset, fontBytes.byteOffset + fontBytes.byteLength));

type Theme = {
  name: "dark" | "light";
  bg: string;
  panel: string;
  line: string;
  text: string;
  dim: string;
  ink: string;
  eye: string;
  leaf: string;
  amber: string;
  violet: string;
};

const THEMES: readonly Theme[] = [
  { name: "dark", bg: "#0b1116", panel: "#111b22", line: "rgba(238,238,236,0.10)", text: "#eeeeec", dim: "#8fa1ac", ink: "#a6dcf0", eye: "#eca3c4", leaf: "#7fd88f", amber: "#f2c46b", violet: "#b9a2f5" },
  { name: "light", bg: "#f7f9fa", panel: "#ffffff", line: "rgba(24,36,44,0.10)", text: "#18242c", dim: "#55707d", ink: "#31697e", eye: "#a34170", leaf: "#2f8f46", amber: "#9a6a10", violet: "#5b3fc4" },
];

const SANS = "'Instrument Sans', Inter, -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif";
const MONO = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
const W = 1280;

const f = (n: number): string => String(Math.round(n * 100) / 100);
let KEYFRAMES: string[] = [];
const pct = (at: number, total: number): string => `${Math.max(0, Math.min(100, (at / total) * 100)).toFixed(2)}%`;
function shown(from: number, until: number, total: number): string {
  const name = `w${KEYFRAMES.length}`;
  KEYFRAMES.push(`@keyframes ${name}{0%,${pct(from, total)}{opacity:0}${pct(from + 0.12, total)},${pct(until, total)}{opacity:1}${pct(until + 0.12, total)},100%{opacity:0}}`);
  return `opacity:0;animation:${name} ${total}s linear infinite`;
}
function typed(text: string, x: number, y: number, charW: number, from: number, until: number, hold: number, total: number, attrs: string): string {
  return [...text]
    .map((ch, i) => {
      const at = from + ((until - from) * i) / text.length;
      return ch === " " ? "" : `<text x="${f(x + i * charW)}" y="${y}" ${attrs} style="${shown(at, hold, total)}">${esc(ch)}</text>`;
    })
    .join("");
}
function caretMove(x: number, y: number, n: number, charW: number, from: number, until: number, hold: number, total: number, attrs: string): string {
  const name = `c${KEYFRAMES.length}`;
  KEYFRAMES.push(`@keyframes ${name}{0%,${pct(from, total)}{transform:translateX(0)}${pct(until, total)},${pct(hold, total)}{transform:translateX(${f(n * charW)}px)}100%{transform:translateX(0)}}`);
  return `<g style="animation:${name} ${total}s steps(${n},end) infinite"><text x="${x}" y="${y}" ${attrs} class="caret">▍</text></g>`;
}
const esc = (s: string): string => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

async function frame(sheet: "directions" | "reactions", cell: number, size: number): Promise<string> {
  const atlas = join(here, `kit/mascot/sprout-${sheet}.webp`);
  const tile = 360;
  const left = (cell % 3) * tile;
  const top = Math.floor(cell / 3) * tile;
  const bytes = await sharp(atlas).extract({ left, top, width: tile, height: tile }).resize(size, size).webp({ quality: 82, alphaQuality: 90 }).toBuffer();
  return "data:image/webp;base64," + bytes.toString("base64");
}

const LOOK: readonly ["directions" | "reactions", number][] = [
  ["directions", 4], ["directions", 5], ["directions", 2], ["directions", 1], ["directions", 0], ["directions", 3],
  ["directions", 6], ["directions", 7], ["directions", 8], ["directions", 4], ["reactions", 0], ["directions", 4],
  ["reactions", 4], ["reactions", 8], ["reactions", 2], ["directions", 4],
];

const lookFrames = await Promise.all(LOOK.map(([sheet, cell]) => frame(sheet, cell, 240)));
const growing = await frame("reactions", 8, 140);
const sprites = {
  front: await frame("directions", 4, 120),
  love: await frame("reactions", 1, 120),
  sparkle: await frame("reactions", 2, 120),
  growing: await frame("reactions", 8, 160),
  sleepy: await frame("reactions", 6, 120),
};
const sprite = (href: string, cx: number, cy: number, size: number): string => `<image href="${href}" x="${f(cx - size / 2)}" y="${f(cy - size / 2)}" width="${size}" height="${size}"/>`;

function flipbook(id: string, frames: readonly string[], x: number, y: number, size: number, seconds: number): string {
  const step = 100 / frames.length;
  const keyframes = `@keyframes ${id}{0%{opacity:1}${step.toFixed(3)}%{opacity:0}100%{opacity:0}}`;
  const unique = [...new Set(frames)];
  const defs = unique.map((href, k) => `<image id="${id}-${k}" href="${href}" width="${size}" height="${size}"/>`).join("");
  const uses = frames
    .map((href, i) => `<use href="#${id}-${unique.indexOf(href)}" x="${x}" y="${y}" style="opacity:0;animation:${id} ${seconds}s steps(1,end) infinite;animation-delay:${(-seconds + (i * seconds) / frames.length).toFixed(3)}s"/>`)
    .join("");
  return `<defs>${defs}</defs><style>${keyframes}</style>${uses}`;
}

function leafPath(len: number, width: number): string {
  return `M0 0C${f(len * 0.25)} ${f(-width)} ${f(len * 0.75)} ${f(-width)} ${f(len)} 0C${f(len * 0.75)} ${f(width * 0.55)} ${f(len * 0.25)} ${f(width * 0.55)} 0 0Z`;
}

function wordmark(t: Theme, size: number, x0: number, baseline: number): string {
  let x = x0;
  let body = "";
  const scale = size / font.unitsPerEm;
  for (const ch of "Stem") {
    const glyph = font.charToGlyph(ch);
    const path = glyph.getPath(x, baseline, size);
    body += `<path fill="${t.text}" d="${path.toPathData({ decimalPlaces: 2, flipY: false })}"/>`;
    if (ch === "t") {
      const bb = path.getBoundingBox();
      const stemX = bb.x1 + (bb.x2 - bb.x1) * 0.36;
      const k = size / 88;
      body += `<g transform="translate(${f(stemX + 3 * k)} ${f(bb.y1 + 3 * k)})"><g class="sway"><path d="${leafPath(21 * k, 8 * k)}" fill="${t.leaf}" transform="rotate(-62)"/></g></g>`;
    }
    x += glyph.advanceWidth * scale - size * 0.02;
  }
  return body;
}

function backdrop(t: Theme, h: number, glow: string): string {
  return `<defs>
<radialGradient id="g1" cx="0.18" cy="0.5" r="0.5"><stop offset="0" stop-color="${t.leaf}" stop-opacity="${t.name === "dark" ? 0.2 : 0.14}"/><stop offset="1" stop-color="${t.leaf}" stop-opacity="0"/></radialGradient>
<radialGradient id="g2" cx="0.95" cy="0" r="0.6"><stop offset="0" stop-color="${t.ink}" stop-opacity="${t.name === "dark" ? 0.22 : 0.16}"/><stop offset="1" stop-color="${t.ink}" stop-opacity="0"/></radialGradient>
<radialGradient id="g3" cx="0.7" cy="1.1" r="0.5"><stop offset="0" stop-color="${t.eye}" stop-opacity="${t.name === "dark" ? 0.14 : 0.09}"/><stop offset="1" stop-color="${t.eye}" stop-opacity="0"/></radialGradient>
<pattern id="dots" width="24" height="24" patternUnits="userSpaceOnUse"><circle cx="2" cy="2" r="1.1" fill="${t.text}" fill-opacity="0.07"/></pattern>
<clipPath id="clip"><rect width="${W}" height="${h}" rx="28"/></clipPath>
</defs>
<g clip-path="url(#clip)"><rect width="${W}" height="${h}" fill="${t.bg}"/><rect width="${W}" height="${h}" fill="url(#dots)"/>${glow.includes("1") ? `<rect width="${W}" height="${h}" fill="url(#g1)"/>` : ""}${glow.includes("2") ? `<rect width="${W}" height="${h}" fill="url(#g2)"/>` : ""}${glow.includes("3") ? `<rect width="${W}" height="${h}" fill="url(#g3)"/>` : ""}</g>`;
}

const frameEdge = (t: Theme, h: number): string => `<rect x="0.5" y="0.5" width="${W - 1}" height="${h - 1}" rx="27.5" fill="none" stroke="${t.line}"/>`;

const BASE_STYLE = `
@keyframes bob{0%,100%{transform:translateY(0)}50%{transform:translateY(-8px)}}
@keyframes spin{to{transform:rotate(360deg)}}
@keyframes caret{0%,49%{opacity:1}50%,100%{opacity:0}}
@keyframes sway{0%,100%{transform:rotate(-7deg)}50%{transform:rotate(7deg)}}
@keyframes flow{to{stroke-dashoffset:-40}}
@keyframes pulse{0%,100%{opacity:.78}50%{opacity:1}}
.sway{transform-box:view-box;transform-origin:0 0;animation:sway 3.2s ease-in-out infinite}
.caret{animation:caret 1s steps(1,end) infinite}
.bob{animation:bob 5s ease-in-out infinite}
.flow{stroke-dasharray:6 14;animation:flow 1.1s linear infinite}
@media (prefers-reduced-motion:reduce){*{animation:none!important}}
`;

function svg(t: Theme, h: number, label: string, body: string, style = ""): string {
  const frames = KEYFRAMES.join("");
  KEYFRAMES = [];
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${h}" viewBox="0 0 ${W} ${h}" role="img" aria-label="${esc(label)}"><style>${BASE_STYLE}${style}${frames}</style>${body}</svg>`;
}

function chip(t: Theme, x: number, y: number, label: string, tone: string): { g: string; w: number } {
  const w = Math.round(label.length * 7.9 + 34);
  return {
    g: `<g transform="translate(${x} ${y})"><rect width="${w}" height="34" rx="17" fill="${tone}" fill-opacity="0.10" stroke="${tone}" stroke-opacity="0.45"/><circle cx="17" cy="17" r="4" fill="${tone}"/><text x="28" y="22" font-family="${MONO}" font-size="13" fill="${t.text}">${esc(label)}</text></g>`,
    w,
  };
}

function hero(t: Theme): string {
  const h = 420;
  const command = `stem "fix the failing test"`;
  const charW = 10.2;
  let cx = 522;
  const chips = [
    ["Rust core", t.ink],
    ["QuickJS sandbox", t.leaf],
    ["6.8 MB binary", t.amber],
    ["any model", t.eye],
    ["Claude plan", t.violet],
  ]
    .map(([label, tone]) => {
      const c = chip(t, cx, 320, label, tone);
      cx += c.w + 10;
      return c.g;
    })
    .join("");
  const body = `${backdrop(t, h, "123")}
<g clip-path="url(#clip)">
<circle cx="220" cy="210" r="168" fill="none" stroke="${t.leaf}" stroke-opacity="0.25" stroke-dasharray="2 10" style="transform-origin:220px 210px;animation:spin 50s linear infinite"/>
<circle cx="220" cy="210" r="138" fill="none" stroke="${t.ink}" stroke-opacity="0.14"/>
${flipbook("look", lookFrames, 100, 90, 240, 9.6)}
<g class="bob" style="animation-delay:-1s"><g transform="rotate(10 1190 90)">${sprite(sprites.love, 1190, 90, 84)}</g></g>
<g class="bob" style="animation-delay:-2.4s"><g transform="rotate(-8 1200 300)">${sprite(sprites.sparkle, 1200, 300, 70)}</g></g>
</g>
${frameEdge(t, h)}
<text x="522" y="98" font-family="${MONO}" font-size="15" letter-spacing="4" fill="${t.leaf}">CODING AGENT · GROWS ITS OWN TOOLS</text>
${wordmark(t, 104, 512, 196)}
<text x="522" y="244" font-family="${SANS}" font-size="25" fill="${t.dim}">Talk to it. It does the work, and gets better at your project.</text>
<text x="522" y="284" font-family="${MONO}" font-size="17" fill="${t.leaf}">~/</text>
${typed(command, 560, 284, charW, 0.6, 3.2, 6.4, 7, `font-family="${MONO}" font-size="17" fill="${t.text}"`)}
${caretMove(560, 284, command.length, charW, 0.6, 3.2, 6.4, 7, `font-family="${MONO}" font-size="17" fill="${t.leaf}"`)}`;
  return svg(t, h, "Sprout, Stem's mascot, looks around. Stem: a coding agent that grows its own tools. Rust core, QuickJS sandbox, 6.8 MB binary, any model on models.dev, your Claude plan.", body + chips);
}

type Line = { at: number; text: string; color: string; x?: number; size?: number; weight?: number };

function terminal(t: Theme): string {
  const h = 450;
  const total = 16;
  const x = 64;
  const top = 92;
  const lh = 27;
  const prompt = `stem "an expired, never confirmed Stop still blocks fresh work on that tab"`;
  const blocks: [number, string, string][] = [
    [0.2, "▪", t.ink], [0.5, "▪", t.ink], [0.8, "■", t.ink], [1.1, "▪", t.violet], [1.5, "■", t.eye], [1.9, "▪", t.ink],
    [2.3, "■", t.leaf], [2.7, "▪", t.eye], [3.1, "▪", t.violet], [3.5, "■", t.eye], [3.9, "▪", t.leaf], [4.3, "■", t.eye],
  ];
  const cellStart = 2.2;
  const cellY = top + lh * 1.6;
  const bar = 28;
  const barSteps = [
    { at: cellStart, done: 0, step: "1 of 4 · reproduce in /tmp/stop-repro.ts" },
    { at: cellStart + 1.4, done: 1, step: "2 of 4 · drop stops older than six hours" },
    { at: cellStart + 2.8, done: 2, step: "3 of 4 · same repro after the fix" },
    { at: cellStart + 4.0, done: 3, step: "4 of 4 · run the covering tests" },
  ];
  const cellEnd = cellStart + 5.4;
  const steps = barSteps
    .map((s, i) => {
      const until = i + 1 < barSteps.length ? barSteps[i + 1].at : cellEnd;
      const filled = Math.round((bar * (s.done + 0.5)) / 4);
      const pct = Math.round(((s.done + 0.5) / 4) * 100);
      return `<g style="${shown(s.at, until - 0.13, total)}">
<text x="${x + 18}" y="${f(cellY)}" font-family="${MONO}" font-size="16" fill="${t.text}">${esc(s.step)}</text>
<text x="${x + 18}" y="${f(cellY + lh)}" font-family="${MONO}" font-size="16"><tspan fill="${t.leaf}">${"█".repeat(filled)}</tspan><tspan fill="${t.dim}" fill-opacity="0.35">${"░".repeat(bar - filled)}</tspan><tspan fill="${t.dim}">  ${pct}% · 0:${String(10 + i * 18).padStart(2, "0")}</tspan></text></g>`;
    })
    .join("");
  const blockRow = blocks
    .map(([at, ch, color], i) => `<text x="${x + 18 + i * 22}" y="${f(cellY + lh * 2)}" font-family="${MONO}" font-size="16" fill="${color}" style="${shown(cellStart + at, cellEnd, total)}">${ch}</text>`)
    .join("");
  const stats = `<text x="${x + 18}" y="${f(cellY + lh * 3)}" font-family="${MONO}" font-size="14" fill="${t.dim}" style="${shown(cellStart + 0.4, cellEnd, total)}">scripts 11 · cache 88% · warm 4:59 · tsgo ready</text>`;
  const answerTop = top + lh * 1.6;
  const answer: Line[] = [
    { at: cellEnd, text: "Asked: Fix the bug where a Stop older than six hours and never confirmed", color: t.text, weight: 700 },
    { at: cellEnd, text: "       stays in memory and blocks resuming fresh work on that tab.", color: t.text, weight: 700 },
    { at: cellEnd + 0.5, text: "Done:", color: t.leaf, weight: 700 },
    { at: cellEnd + 0.7, text: "- Reproduced with /tmp/stop-repro.ts: stop still returned 6h+1min later", color: t.text },
    { at: cellEnd + 0.9, text: "- stop-memory.ts: pendingStops drops in-memory stops older than six hours", color: t.text },
    { at: cellEnd + 1.1, text: "- Same repro after the fix: empty list, hasUnconfirmedStop false, \"OK\"", color: t.text },
    { at: cellEnd + 1.3, text: "- Language server: no errors · engine-client-branch-e2e: 1 pass, 0 fail", color: t.text },
    { at: cellEnd + 1.6, text: "Not done:", color: t.amber, weight: 700 },
    { at: cellEnd + 1.8, text: "- No test imports stop-memory.ts directly, so only the repro checks this path", color: t.text },
    { at: cellEnd + 2.3, text: "claude-opus-5-5 medium · 11 scripts · 111.3k in · cache 88% · 3.3k out · $0.1507 · 74 s", color: t.dim, size: 14 },
  ];
  const answerSvg = answer
    .map((l, i) => `<text x="${x + 18}" y="${f(answerTop + i * lh)}" font-family="${MONO}" font-size="${l.size ?? 16}" font-weight="${l.weight ?? 400}" fill="${l.color}" style="${shown(l.at, total - 0.6, total)}">${esc(l.text)}</text>`)
    .join("");
  const typeEnd = 1.8;
  const body = `${backdrop(t, h, "23")}
<g clip-path="url(#clip)">
<rect x="32" y="28" width="${W - 64}" height="${h - 56}" rx="18" fill="${t.panel}" stroke="${t.line}"/>
<circle cx="60" cy="56" r="6.5" fill="#ff6159"/><circle cx="82" cy="56" r="6.5" fill="#ffbd2e"/><circle cx="104" cy="56" r="6.5" fill="#28c941"/>
<text x="${W / 2}" y="61" text-anchor="middle" font-family="${MONO}" font-size="13" fill="${t.dim}">~/project · stem</text>
<line x1="32" y1="76" x2="${W - 32}" y2="76" stroke="${t.line}"/>

<text x="${x}" y="${top}" font-family="${MONO}" font-size="16" fill="${t.leaf}">❯</text>
${typed(prompt, x + 18, top, 9.63, 0.2, typeEnd, total - 0.6, total, `font-family="${MONO}" font-size="16" fill="${t.text}"`)}
${steps}${blockRow}${stats}${answerSvg}
<g transform="translate(${W - 200} ${h - 180})" style="${shown(cellEnd + 2.4, total - 0.6, total)}"><image href="${growing}" width="140" height="140"/></g>
</g>
${frameEdge(t, h)}`;
  return svg(t, h, "A real Stem run: it reproduces an expired Stop bug, fixes stop-memory.ts, checks the repro and tests, and answers with Asked, Done and Not done. 11 scripts, cache 88%, $0.15, 74 s.", body);
}

function card(t: Theme, x: number, y: number, w: number, h: number, title: string, sub: string, tone: string, extra = ""): string {
  return `<g transform="translate(${x} ${y})" ${extra}><rect width="${w}" height="${h}" rx="16" fill="${t.panel}" stroke="${tone}" stroke-opacity="0.55"/><rect width="${w}" height="${h}" rx="16" fill="${tone}" fill-opacity="0.06"/>
<text x="${w / 2}" y="${h / 2 - 4}" text-anchor="middle" font-family="${SANS}" font-size="20" font-weight="700" fill="${t.text}">${esc(title)}</text>
<text x="${w / 2}" y="${h / 2 + 20}" text-anchor="middle" font-family="${MONO}" font-size="12" fill="${t.dim}">${esc(sub)}</text></g>`;
}

function loop(t: Theme): string {
  const h = 400;
  const flow = (d: string, tone: string, delay = 0): string =>
    `<path d="${d}" fill="none" stroke="${tone}" stroke-opacity="0.22" stroke-width="2"/><path d="${d}" fill="none" stroke="${tone}" stroke-width="2.4" stroke-linecap="round" class="flow" style="animation-delay:${delay}s"/>`;
  const label = (x: number, y: number, text: string, tone: string): string =>
    `<g><rect x="${x - text.length * 4.1 - 12}" y="${y - 17}" width="${text.length * 8.2 + 24}" height="26" rx="13" fill="${t.bg}" stroke="${tone}" stroke-opacity="0.4"/><text x="${x}" y="${y + 1}" text-anchor="middle" font-family="${MONO}" font-size="13" fill="${tone}">${esc(text)}</text></g>`;
  const tools = ["edit", "rename", "test", "sh", "sql", "refs", "testsFor", "ask"];
  let tx = 712;
  let ty = 178;
  const toolChips = tools
    .map((name, i) => {
      const w = Math.round((name.length + 2) * 7.3 + 16);
      if (tx + w > 972) {
        tx = 712;
        ty += 34;
      }
      const g = `<g transform="translate(${tx} ${ty})" style="animation:pulse 2.4s ease-in-out ${(i * 0.3).toFixed(1)}s infinite"><rect width="${w}" height="26" rx="8" fill="${t.leaf}" fill-opacity="0.10" stroke="${t.leaf}" stroke-opacity="0.45"/><text x="${w / 2}" y="18" text-anchor="middle" font-family="${MONO}" font-size="12" fill="${t.text}">$.${name}</text></g>`;
      tx += w + 8;
      return g;
    })
    .join("");
  const body = `${backdrop(t, h, "12")}
<g clip-path="url(#clip)">
<text x="64" y="64" font-family="${MONO}" font-size="15" letter-spacing="4" fill="${t.leaf}">ONE LOOP</text>
<text x="64" y="98" font-family="${SANS}" font-size="28" font-weight="700" fill="${t.text}">The model writes one script. Only its result comes back.</text>
${flow("M232 250 H340", t.ink)}
${flow("M520 250 H690", t.eye, -0.4)}
${flow("M990 187 H1040", t.violet, -0.2)}
${flow("M990 277 H1040", t.amber, -0.6)}
${flow("M840 300 C840 370 430 370 430 300", t.leaf, -0.8)}
${card(t, 64, 200, 168, 100, "you", "one task, plain words", t.ink)}
${card(t, 340, 200, 180, 100, "model", "text ends the turn", t.eye)}
<g transform="translate(690 140)"><rect width="300" height="160" rx="18" fill="${t.panel}" stroke="${t.leaf}" stroke-opacity="0.7"/><text x="22" y="28" font-family="${SANS}" font-size="18" font-weight="700" fill="${t.text}">QuickJS sandbox</text><text x="278" y="28" text-anchor="end" font-family="${MONO}" font-size="12" fill="${t.dim}">only $ inside</text></g>
${toolChips}
${card(t, 1040, 150, 176, 74, "code facts", "SQLite · git", t.violet)}
${card(t, 1040, 240, 176, 74, "language servers", "tsgo · ty · r-a", t.amber)}
${label(605, 234, "js block", t.eye)}
${label(635, 372, "return value only", t.leaf)}
${label(286, 234, "task", t.ink)}
</g>
${frameEdge(t, h)}`;
  return svg(t, h, "The loop: you give a task, the model answers in text or one JavaScript block, the block runs in a QuickJS sandbox that only sees $ (edit, rename, test, sh, sql, refs, testsFor, ask), which reads code facts and language servers, and only the return value goes back to the model.", body);
}

function learn(t: Theme): string {
  const h = 380;
  const total = 12;
  const stages = [
    { x: 140, title: "each turn", sub: "files, commands, fixes", tone: t.ink },
    { x: 440, title: "a note", sub: "start here, run this check", tone: t.leaf },
    { x: 740, title: "scored", sub: "did the turn do it?", tone: t.violet },
    { x: 1040, title: "right too rarely", sub: "withheld, still scored", tone: t.amber },
  ];
  const sprouts = stages
    .map((s, i) => {
      const at = 0.6 + i * 2.6;
      const plant =
        i === 0
          ? [0, 1, 2].map((k) => `<rect x="${s.x - 54 + k * 38}" y="166" width="30" height="20" rx="5" fill="${t.ink}" fill-opacity="0.2" stroke="${t.ink}" style="${shown(at + k * 0.5, total - 0.6, total)}"/>`).join("")
          : i === 1
            ? `<g style="${shown(at, total - 0.6, total)}">${sprite(sprites.front, s.x, 168, 70)}</g>`
            : i === 2
              ? `<g style="${shown(at, total - 0.6, total)}">${sprite(sprites.growing, s.x, 160, 96)}</g>`
              : `<g style="${shown(at, total - 0.6, total)}"><g style="transform-origin:${s.x}px 170px;animation:fall 3s ease-in ${at}s infinite">${sprite(sprites.sleepy, s.x, 166, 76)}</g></g>`;
      const arrow = i < 3 ? `<path d="M${s.x + 110} 176 H${s.x + 190}" stroke="${t.dim}" stroke-opacity="0.5" stroke-width="2" stroke-dasharray="4 6"/><path d="M${s.x + 184} 170 l8 6 -8 6" fill="none" stroke="${t.dim}" stroke-opacity="0.7" stroke-width="2"/>` : "";
      return `${plant}${arrow}
<text x="${s.x}" y="256" text-anchor="middle" font-family="${MONO}" font-size="15" fill="${s.tone}">${esc(s.title)}</text>
<text x="${s.x}" y="282" text-anchor="middle" font-family="${SANS}" font-size="16" fill="${t.dim}">${esc(s.sub)}</text>`;
    })
    .join("");
  const body = `${backdrop(t, h, "13")}
<g clip-path="url(#clip)">
<line x1="64" y1="212" x2="${W - 64}" y2="212" stroke="${t.leaf}" stroke-opacity="0.18" stroke-width="2"/>
${sprouts}
<text x="64" y="64" font-family="${MONO}" font-size="15" letter-spacing="4" fill="${t.leaf}">LEARNING</text>
<text x="64" y="98" font-family="${SANS}" font-size="28" font-weight="700" fill="${t.text}">Stem learns from what each turn does. Notes have to earn being shown.</text>
<text x="64" y="334" font-family="${MONO}" font-size="14" fill="${t.dim}">.stem/learn/ · stays on your machine · a kind of note is shown once right in 5 of 10 turns</text>
</g>
${frameEdge(t, h)}`;
  return svg(t, h, "Learning: Stem records what each turn does: the files it wrote, the commands it ran after editing, the fixes that stayed. From that it makes notes, such as where to start, which checks this part of the project usually runs, and which earlier fix was undone. Every note is scored against what the turn then did, and a kind of note is shown only once it has been right in at least half of ten turns.", body, "@keyframes fall{0%,40%{transform:translate(0,0) rotate(0)}90%{transform:translate(40px,60px) rotate(40deg);opacity:0}100%{opacity:0}}");
}

function facts(t: Theme): string {
  const h = 360;
  const tiles: [string, string, string][] = [
    ["TypeScript", "tree-sitter", t.ink], ["JavaScript", "tree-sitter", t.ink], ["Rust", "tree-sitter", t.ink], ["Python", "tree-sitter", t.ink], ["Go", "tree-sitter", t.ink],
    ["git", "commits · co-change", t.violet], ["tsgo", "language server", t.amber], ["ty", "language server", t.amber], ["rust-analyzer", "language server", t.amber], ["gopls · clangd", "when installed", t.amber],
  ];
  const tw = 220;
  const th = 64;
  const gx = 14;
  const startX = (W - (5 * tw + 4 * gx)) / 2;
  const hop = 10;
  const tilesSvg = tiles
    .map(([name, sub, tone], i) => {
      const col = i % 5;
      const row = Math.floor(i / 5);
      const x = startX + col * (tw + gx);
      const y = 150 + row * (th + 14);
      return `<g transform="translate(${f(x)} ${y})"><g style="animation:squash ${hop}s ease-in-out ${(-hop + (i * hop) / tiles.length).toFixed(2)}s infinite;transform-origin:${tw / 2}px ${th}px"><rect width="${tw}" height="${th}" rx="14" fill="${t.panel}" stroke="${tone}" stroke-opacity="0.5"/><rect width="${tw}" height="${th}" rx="14" fill="${tone}" fill-opacity="0.06"/><circle cx="24" cy="${th / 2}" r="6" fill="${tone}"/><text x="42" y="28" font-family="${SANS}" font-size="18" font-weight="700" fill="${t.text}">${esc(name)}</text><text x="42" y="48" font-family="${MONO}" font-size="12" fill="${t.dim}">${esc(sub)}</text></g></g>`;
    })
    .join("");
  const body = `${backdrop(t, h, "23")}
<g clip-path="url(#clip)">
<text x="64" y="64" font-family="${MONO}" font-size="15" letter-spacing="4" fill="${t.leaf}">CODE FACTS · $.sql</text>
<text x="64" y="98" font-family="${SANS}" font-size="28" font-weight="700" fill="${t.text}">It knows your repo before it reads a file.</text>
<text x="${W - 64}" y="98" text-anchor="end" font-family="${MONO}" font-size="15" fill="${t.dim}">8,079 files in 3.9 s · then &lt; 1 ms a query</text>
${tilesSvg}
</g>
${frameEdge(t, h)}`;
  return svg(t, h, "Code facts: tree-sitter for TypeScript, JavaScript, Rust, Python and Go, git history and co-change, and language servers tsgo, ty, rust-analyzer, gopls and clangd. 8,079 files indexed in 3.9 seconds, then under a millisecond per query.", body, "@keyframes squash{0%,8%,100%{transform:translateY(0) scale(1,1)}3%{transform:translateY(-8px) scale(1,1)}6%{transform:translateY(0) scale(1.03,.94)}}");
}

function proof(t: Theme): string {
  const h = 300;
  const stats: [string, string, string][] = [
    ["6", "open bugs in a 1M-line TypeScript repo", t.ink],
    ["5 + 1", "fixed and proven · one correctly not reproduced", t.leaf],
    ["64 · 0", "scripts run · scripts failed", t.violet],
    ["87%", "of the data scripts touched never reached the model", t.amber],
    ["$1.15", "for all six, Opus 5.5", t.eye],
  ];
  const cw = 222;
  const gx = 14;
  const x0 = (W - (5 * cw + 4 * gx)) / 2;
  const cards = stats
    .map(([big, small, tone], i) => {
      const x = x0 + i * (cw + gx);
      const words = small.split(" ");
      const lines: string[] = [];
      let cur = "";
      for (const w of words) {
        if ((cur + " " + w).trim().length > 24) {
          lines.push(cur.trim());
          cur = w;
        } else cur += " " + w;
      }
      lines.push(cur.trim());
      return `<g transform="translate(${f(x)} 120)" style="animation:pulse 4s ease-in-out ${(i * 0.5).toFixed(1)}s infinite"><rect width="${cw}" height="140" rx="16" fill="${t.panel}" stroke="${tone}" stroke-opacity="0.5"/><text x="20" y="54" font-family="${SANS}" font-size="38" font-weight="700" fill="${tone}">${esc(big)}</text>${lines.map((l, k) => `<text x="20" y="${84 + k * 19}" font-family="${SANS}" font-size="14.5" fill="${t.dim}">${esc(l)}</text>`).join("")}</g>`;
    })
    .join("");
  const body = `${backdrop(t, h, "12")}
<g clip-path="url(#clip)">
<text x="64" y="64" font-family="${MONO}" font-size="15" letter-spacing="4" fill="${t.leaf}">MEASURED, NOT FELT</text>
<text x="64" y="98" font-family="${SANS}" font-size="28" font-weight="700" fill="${t.text}">Every run reproduces first, then proves the fix.</text>
${cards}
</g>
${frameEdge(t, h)}`;
  return svg(t, h, "Measured on six open bugs in a 1M-line TypeScript repo: five fixed and proven, one correctly not reproduced, 64 scripts with 0 failures, 87% of the data scripts touched never reached the model, $1.15 for all six with Opus 5.5.", body);
}

const builders: Record<string, (t: Theme) => string> = { hero, terminal, loop, learn, facts, proof };
await mkdir(out, { recursive: true });
for (const t of THEMES) {
  for (const [name, build] of Object.entries(builders)) {
    const text = build(t);
    if (/<script\b|var\(/.test(text)) throw new Error(`Nonportable SVG: ${name}`);
    await writeFile(join(out, `${name}-${t.name}.svg`), text);
    console.log(`${name}-${t.name}.svg ${(text.length / 1024).toFixed(0)} KB`);
  }
}
const footer = await sharp(join(here, "kit/mascot/sprout-reactions.webp")).extract({ left: 360, top: 0, width: 360, height: 360 }).resize(160, 160).webp({ quality: 90, alphaQuality: 100 }).toBuffer();
await writeFile(join(out, "sprout-love.webp"), footer);
