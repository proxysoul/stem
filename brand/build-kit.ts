import { mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { Resvg } from "@resvg/resvg-js";
import sharp from "sharp";
import opentype from "opentype.js";

const here = import.meta.dir;
const out = join(here, "kit");
const animatedOut = join(out, "animated");
const fontBytes = await readFile(join(here, "fonts/InstrumentSans-Bold.ttf"));
const font = opentype.parse(fontBytes.buffer.slice(fontBytes.byteOffset, fontBytes.byteOffset + fontBytes.byteLength));

const themes = {
  dark: { ink: "#a6dcf0", eye: "#eca3c4", leaf: "#7fd88f", text: "#eeeeec", bg: "#10171d" },
  light: { ink: "#31697e", eye: "#a34170", leaf: "#2f8f46", text: "#18242c", bg: "#f7f9fa" },
};
type Theme = (typeof themes)["dark"];
type Pose = { phase: number };

function f(n: number): string {
  return String(Math.round(n * 100) / 100);
}

function leafPath(len: number, width: number): string {
  return `M0 0C${f(len * 0.25)} ${f(-width)} ${f(len * 0.75)} ${f(-width)} ${f(len)} 0C${f(len * 0.75)} ${f(width * 0.55)} ${f(len * 0.25)} ${f(width * 0.55)} 0 0Z`;
}

const BODY = { x: 80, y: 90, r: 37 };
const EYE_R = 16;

function roots(theme: Theme, pose: Pose): string {
  return [-15, 0, 15]
    .map((dx, i) => {
      const w = Math.sin(pose.phase + i * 2.1) * 3;
      const x = BODY.x + dx;
      const top = BODY.y + BODY.r - 6 - (dx === 0 ? 0 : 3);
      const lean = dx * 0.25;
      return `<path d="M${f(x)} ${f(top)}C${f(x - 5 + lean)} ${f(top + 9)} ${f(x + 5 + lean + w)} ${f(top + 15)} ${f(x + lean * 1.6 + w * 0.6)} ${f(top + 23)}" fill="none" stroke="${theme.ink}" stroke-width="6" stroke-linecap="round"/>`;
    })
    .join("");
}

function leaves(theme: Theme, pose: Pose): string {
  const sway = Math.sin(pose.phase) * 6;
  const base = { x: BODY.x, y: BODY.y - BODY.r + 3 };
  const right = `<path d="${leafPath(36, 16)}" fill="${theme.leaf}" transform="translate(${f(base.x + 1)} ${f(base.y)}) rotate(${f(-32 + sway)})"/>`;
  const left = `<path d="${leafPath(36, 16)}" fill="${theme.leaf}" transform="translate(${f(base.x - 1)} ${f(base.y)}) scale(-1 1) rotate(${f(-32 - sway)})"/>`;
  return right + left;
}

function icon(theme: Theme, pose: Pose = { phase: 0 }, id = "stem"): string {
  const gaze = { x: Math.sin(pose.phase) * 4, y: Math.sin(pose.phase * 2) * 2.5 };
  const eye = { x: BODY.x + gaze.x, y: BODY.y + 2 + gaze.y };
  const pupil = { x: eye.x + 1 + gaze.x * 0.3, y: eye.y - 1 + gaze.y * 0.3 };
  const mask = `<mask id="${id}-cut" maskUnits="userSpaceOnUse" x="0" y="0" width="160" height="160"><rect width="160" height="160" fill="#fff"/><circle cx="${f(eye.x)}" cy="${f(eye.y)}" r="${EYE_R + 4}" fill="#000"/><circle cx="${f(eye.x)}" cy="${f(eye.y)}" r="${EYE_R}" fill="#fff"/><circle cx="${f(pupil.x)}" cy="${f(pupil.y)}" r="6.5" fill="#000"/></mask>`;
  return (
    `<defs>${mask}</defs>` +
    roots(theme, pose) +
    leaves(theme, pose) +
    `<g mask="url(#${id}-cut)"><circle cx="${BODY.x}" cy="${BODY.y}" r="${BODY.r}" fill="${theme.ink}"/><circle cx="${f(eye.x)}" cy="${f(eye.y)}" r="${EYE_R}" fill="${theme.eye}"/></g>` +
    `<circle cx="${f(pupil.x + 2.6)}" cy="${f(pupil.y - 2.6)}" r="2.3" fill="${theme.eye}"/>`
  );
}

function wordmark(theme: Theme, pose: Pose = { phase: 0 }, leaf = true): { body: string; width: number } {
  const size = 88;
  const scale = size / font.unitsPerEm;
  let x = 0;
  let body = "";
  for (const ch of "Stem") {
    const glyph = font.charToGlyph(ch);
    const path = glyph.getPath(x, 105, size);
    body += `<path fill="${theme.text}" d="${path.toPathData({ decimalPlaces: 2, flipY: false })}"/>`;
    if (leaf && ch === "t") {
      const bb = path.getBoundingBox();
      const stemX = bb.x1 + (bb.x2 - bb.x1) * 0.36;
      const sway = Math.sin(pose.phase) * 4;
      body += `<path d="${leafPath(21, 8)}" fill="${theme.leaf}" transform="translate(${f(stemX + 3)} ${f(bb.y1 + 3)}) rotate(${f(-62 + sway)})"/>`;
    }
    x += glyph.advanceWidth * scale - size * 0.02;
  }
  return { body, width: Math.ceil(x + 4) };
}

type Shape = "icon" | "wordmark" | "lockup";

function compose(shape: Shape, theme: Theme, background: "transparent" | "solid", pose: Pose = { phase: 0 }): { svg: string; width: number; height: number } {
  const word = wordmark(theme, pose, shape === "wordmark");
  const width = shape === "icon" ? 192 : shape === "wordmark" ? word.width + 48 : word.width + 216;
  const height = 192;
  const body =
    (shape === "wordmark" ? "" : `<g transform="translate(16 16)">${icon(theme, pose, `stem-${shape}`)}</g>`) +
    (shape === "icon" ? "" : `<g transform="translate(${shape === "wordmark" ? 24 : 196} 12)">${word.body}</g>`);
  const bg = background === "solid" ? `<rect width="100%" height="100%" fill="${theme.bg}"/>` : "";
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" width="${width}" height="${height}" role="img" aria-label="Stem">${bg}${body}</svg>`;
  return { svg, width, height };
}

function render(svg: string, width: number): Buffer {
  return Buffer.from(new Resvg(svg, { fitTo: { mode: "width", value: width }, font: { loadSystemFonts: false } }).render().asPng());
}

await rm(out, { recursive: true, force: true });
await mkdir(animatedOut, { recursive: true });
const manifest: { name: string; theme: string; background: string; width: number; height: number }[] = [];
for (const [themeName, theme] of Object.entries(themes)) {
  for (const shape of ["icon", "wordmark", "lockup"] as Shape[]) {
    for (const background of ["transparent", "solid"] as const) {
      const name = `stem-${shape}-${themeName}-${background}`;
      const { svg } = compose(shape, theme, background);
      if (/var\(|<text\b|<script\b/.test(svg)) throw new Error(`Nonportable SVG: ${name}`);
      const png = render(svg, shape === "icon" ? 1024 : 2048);
      const meta = await sharp(png).metadata();
      const corner = await sharp(png).extract({ left: 0, top: 0, width: 1, height: 1 }).ensureAlpha().raw().toBuffer();
      if (corner[3] !== (background === "solid" ? 255 : 0)) throw new Error(`Incorrect alpha: ${name}`);
      await writeFile(join(out, `${name}.svg`), svg);
      await writeFile(join(out, `${name}.png`), png);
      await writeFile(join(out, `${name}.webp`), await sharp(png).webp({ lossless: true }).toBuffer());
      manifest.push({ name, theme: themeName, background, width: meta.width!, height: meta.height! });
    }
  }
}

const FRAMES = 96;
const DELAY = 50;
for (const [themeName, theme] of Object.entries(themes)) {
  for (const shape of ["icon", "lockup"] as Shape[]) {
    for (const background of ["transparent", "solid"] as const) {
      const name = `stem-${shape}-${themeName}-${background}`;
      const first = compose(shape, theme, background);
      const width = shape === "icon" ? 256 : 768;
      const height = Math.round((first.height / first.width) * width);
      const frames: Buffer[] = [];
      for (let i = 0; i < FRAMES; i++) {
        const { svg } = compose(shape, theme, background, { phase: (i / FRAMES) * Math.PI * 2 });
        frames.push(Buffer.from(new Resvg(svg, { fitTo: { mode: "width", value: width }, font: { loadSystemFonts: false } }).render().pixels));
      }
      if (frames[0].equals(frames[FRAMES / 4])) throw new Error(`Static animation: ${name}`);
      const raw = Buffer.concat(frames);
      for (const format of ["gif", "webp"] as const) {
        const delays = Array(FRAMES).fill(DELAY);
        const image = sharp(raw, { raw: { width, height: height * FRAMES, channels: 4, pageHeight: height } });
        const bytes = await (format === "gif"
          ? image.gif({ loop: 0, delay: delays, effort: 3 })
          : image.webp({ loop: 0, delay: delays, lossless: true, effort: 2 })
        ).toBuffer();
        const meta = await sharp(bytes, { animated: true }).metadata();
        if (meta.pages !== FRAMES || meta.loop !== 0) throw new Error(`Invalid animation: ${name}.${format}`);
        await writeFile(join(animatedOut, `${name}.${format}`), bytes);
      }
    }
  }
}

const tiles: { input: Buffer; left: number; top: number }[] = [];
const cell = { w: 900, h: 300 };
for (const [row, theme] of Object.values(themes).entries()) {
  for (const [col, shape] of (["icon", "wordmark", "lockup"] as Shape[]).entries()) {
    const { svg, width, height } = compose(shape, theme, "solid");
    const png = render(svg, Math.round(width * Math.min((cell.w - 40) / width, (cell.h - 40) / height)));
    const size = await sharp(png).metadata();
    tiles.push({ input: png, left: col * cell.w + Math.round((cell.w - size.width!) / 2), top: row * cell.h + Math.round((cell.h - size.height!) / 2) });
  }
}
await sharp({ create: { width: cell.w * 3, height: cell.h * 2, channels: 4, background: themes.light.bg } })
  .composite([{ input: { create: { width: cell.w * 3, height: cell.h, channels: 4, background: themes.dark.bg } }, left: 0, top: 0 }, ...tiles])
  .png()
  .toFile(join(out, "preview.png"));

const mascotOut = join(out, "mascot");
const sprout = join(here, "characters/sprout");
await mkdir(mascotOut, { recursive: true });
for (const sheet of ["directions", "reactions"]) {
  const atlas = await sharp(join(sprout, `${sheet}.png`)).resize(1080, 1080).webp({ quality: 90, alphaQuality: 100 }).toBuffer();
  const corner = await sharp(atlas).extract({ left: 0, top: 0, width: 1, height: 1 }).ensureAlpha().raw().toBuffer();
  if (corner[3] !== 0) throw new Error(`Mascot sheet has no transparent background: ${sheet}`);
  await writeFile(join(mascotOut, `sprout-${sheet}.webp`), atlas);
}
const front = await sharp(join(sprout, "directions.png")).extract({ left: 341, top: 341, width: 342, height: 342 }).resize(512, 512).png().toBuffer();
await writeFile(join(mascotOut, "sprout.png"), front);
await writeFile(join(mascotOut, "sprout.webp"), await sharp(front).webp({ lossless: true }).toBuffer());

await writeFile(join(out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
console.log(`Built ${manifest.length * 3} static files and ${Object.keys(themes).length * 4 * 2} animations in ${out}`);
