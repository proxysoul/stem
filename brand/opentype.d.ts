declare module "opentype.js" {
  interface BoundingBox {
    x1: number;
    y1: number;
    x2: number;
    y2: number;
  }
  interface Path {
    toPathData(options: { decimalPlaces?: number; flipY?: boolean }): string;
    getBoundingBox(): BoundingBox;
  }
  interface Glyph {
    advanceWidth: number;
    getPath(x: number, y: number, fontSize: number): Path;
  }
  interface Font {
    unitsPerEm: number;
    charToGlyph(char: string): Glyph;
  }
  function parse(buffer: ArrayBuffer): Font;
  const opentype: { parse: typeof parse };
  export default opentype;
}
