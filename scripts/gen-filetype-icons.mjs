// Draws the file-type icons of archives (docs/07-shell-integration.md §3.4): a white page with a
// folded corner, a zipper down its middle and, centred below it, a block in the colour of the
// format family with the format's name. Writes icons/filetypes/<ext>.ico (16–256 px) and
// <ext>.svg (the 256 px drawing). The installer gives OmniZip's file types these icons.
//
//   node scripts/gen-filetype-icons.mjs
//
// Run on Windows after changing the drawing or the colours: the names are set in Segoe UI Bold.
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { Resvg } from '@resvg/resvg-js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const out = path.join(root, 'icons/filetypes');

// Format families; the format badges of the app use the same colours (Icon.tsx).
const ZIP = '#2F6FED';
const SEVEN_Z = '#7A5AF8';
const RAR = '#C0392B';
const TAR = '#8A6D3B';
const STREAM = '#4B5B73';
const IMAGE = '#0B7F72';

/** Extension, name on the icon, colour. */
const FORMATS = [
  ['zip', 'ZIP', ZIP],
  ['zipx', 'ZIPX', ZIP],
  ['7z', '7Z', SEVEN_Z],
  ['rar', 'RAR', RAR],
  ['tar', 'TAR', TAR],
  ['tgz', 'TGZ', TAR],
  ['tbz2', 'TBZ2', TAR],
  ['txz', 'TXZ', TAR],
  ['tzst', 'TZST', TAR],
  ['gz', 'GZ', STREAM],
  ['bz2', 'BZ2', STREAM],
  ['xz', 'XZ', STREAM],
  ['zst', 'ZST', STREAM],
  ['lz4', 'LZ4', STREAM],
  ['br', 'BR', STREAM],
  ['iso', 'ISO', IMAGE],
];

/** Pixel sizes in each .ico: 24 and below are redrawn for the pixel grid, 32 and 40 go without the name. */
const SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

const PAGE = '#FFFFFF';
const FOLD = '#E4E9F0';
const EDGE = '#C5CDD8';

/** The 256 px drawing; `name` false leaves the name out. */
function large(label, color, name = true) {
  const fs = label.length <= 2 ? 70 : label.length === 3 ? 62 : 50;
  const teeth = [22, 34, 46, 58, 70, 82, 94]
    .map((y, i) => `<rect x="${i % 2 ? 136 : 124}" y="${y}" width="12" height="8" rx="3"/>`)
    .join('');
  const text = name
    ? `<text x="136" y="189" text-anchor="middle" dominant-baseline="central" font-family="Segoe UI" font-size="${fs}" font-weight="700" letter-spacing="1" fill="#FFFFFF">${label}</text>`
    : '';
  return `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
  <defs><filter id="s" x="-20%" y="-20%" width="140%" height="140%"><feDropShadow dx="0" dy="3" stdDeviation="3" flood-color="#0F172A" flood-opacity="0.16"/></filter></defs>
  <path d="M68 12 H166 L218 64 V230 Q218 244 204 244 H68 Q54 244 54 230 V26 Q54 12 68 12 Z" fill="${PAGE}" stroke="${EDGE}" stroke-width="4" filter="url(#s)"/>
  <path d="M166 12 V50 Q166 64 180 64 H218 Z" fill="${FOLD}" stroke="${EDGE}" stroke-width="4" stroke-linejoin="round"/>
  <g fill="${color}" opacity="0.55">${teeth}</g>
  <rect x="124" y="106" width="24" height="28" rx="7" fill="${color}"/>
  <rect x="131" y="114" width="10" height="10" rx="3" fill="#FFFFFF"/>
  <rect x="40" y="146" width="192" height="84" rx="16" fill="${color}"/>
  <path d="M40 162 Q40 146 56 146 H216 Q232 146 232 162 V188 H40 Z" fill="#FFFFFF" opacity="0.13"/>
  ${text}
</svg>
`;
}

/** The drawing for 24 px and below, on a 24 px grid. */
function small(color) {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">
  <path d="M6 1.5 H15 L19.5 6 V21.5 Q19.5 22.5 18.5 22.5 H6 Q5 22.5 5 21.5 V2.5 Q5 1.5 6 1.5 Z" fill="${PAGE}" stroke="#8E9AAB" stroke-width="1"/>
  <path d="M15 1.5 V5 Q15 6 16 6 H19.5 Z" fill="${FOLD}" stroke="#8E9AAB" stroke-width="1" stroke-linejoin="round"/>
  <rect x="11.25" y="3" width="2.5" height="6.5" rx="0.6" fill="${color}" opacity="0.55"/>
  <rect x="3" y="13" width="18.5" height="8.5" rx="2" fill="${color}"/>
</svg>
`;
}

function png(svg, size) {
  const r = new Resvg(svg, {
    fitTo: { mode: 'width', value: size },
    font: { loadSystemFonts: true, defaultFontFamily: 'Segoe UI' },
  });
  return r.render().asPng();
}

/** An .ico of PNG images (Windows Vista and later read PNG at every size). */
function ico(images) {
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

mkdirSync(out, { recursive: true });
for (const [ext, label, color] of FORMATS) {
  const images = SIZES.map((size) => {
    const svg = size <= 24 ? small(color) : large(label, color, size >= 48);
    return { size, data: png(svg, size) };
  });
  writeFileSync(path.join(out, `${ext}.ico`), ico(images));
  writeFileSync(path.join(out, `${ext}.svg`), large(label, color));
  console.log(`icons/filetypes/${ext}.ico`);
}
