// Rendering SVG drawings into .ico and .icns files, for the icon generators
// (gen-filetype-icons.mjs, gen-menu-icons.mjs).
import { Resvg } from '@resvg/resvg-js';

/** `svg` as a PNG `size` pixels wide; text is set in system fonts (Segoe UI on Windows). */
export function png(svg, size) {
  const r = new Resvg(svg, {
    fitTo: { mode: 'width', value: size },
    font: { loadSystemFonts: true, defaultFontFamily: 'Segoe UI' },
  });
  return r.render().asPng();
}

/** An .ico of PNG images (Windows Vista and later read PNG at every size). */
export function ico(images) {
  const head = Buffer.alloc(6 + 16 * images.length);
  head.writeUInt16LE(0, 0);
  head.writeUInt16LE(1, 2);
  head.writeUInt16LE(images.length, 4);
  let offset = head.length;
  images.forEach(({ size, data }, i) => {
    const at = 6 + 16 * i;
    head.writeUInt8(size >= 256 ? 0 : size, at);
    head.writeUInt8(size >= 256 ? 0 : size, at + 1);
    head.writeUInt16LE(1, at + 4);
    head.writeUInt16LE(32, at + 6);
    head.writeUInt32LE(data.length, at + 8);
    head.writeUInt32LE(offset, at + 12);
    offset += data.length;
  });
  return Buffer.concat([head, ...images.map((i) => i.data)]);
}

/**
 * An .icns of PNG images, each `{ type, data }` with the element type for its pixel size and scale
 * (`icp4` 16, `ic11` 16@2x, `icp5` 32, `ic12` 32@2x, `ic07` 128, `ic13` 128@2x, `ic08` 256,
 * `ic14` 256@2x, `ic09` 512, `ic10` 512@2x).
 */
export function icns(images) {
  const parts = images.map(({ type, data }) => {
    const head = Buffer.alloc(8);
    head.write(type, 0, 'latin1');
    head.writeUInt32BE(8 + data.length, 4);
    return Buffer.concat([head, data]);
  });
  const head = Buffer.alloc(8);
  head.write('icns', 0, 'latin1');
  head.writeUInt32BE(8 + parts.reduce((n, p) => n + p.length, 0), 4);
  return Buffer.concat([head, ...parts]);
}
