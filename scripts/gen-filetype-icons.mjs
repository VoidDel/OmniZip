// Draws the file-type icons of archives (docs/07-shell-integration.md §3.4): a white page with a
// folded corner, a zipper down its middle and, centred below it, a block in the colour of the
// format family with the format's name. Writes icons/filetypes/<ext>.ico (16–256 px), <ext>.icns
// (16–1024 px, the macOS document icons) and <ext>.svg (the 256 px drawing). The installer gives
// OmniZip's file types these icons.
//
//   node scripts/gen-filetype-icons.mjs
//
// Run on Windows after changing the drawing or the colours: the names are set in Segoe UI Bold
// (Arial Bold where it is missing).
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { icns, ico, png } from './lib/icons.mjs';

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

/** Pixel sizes in each .ico: up to 40 drawn on the pixel grid (`small`), from 48 the 256 px drawing scaled. */
const SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

/** Element types of each .icns and their pixel sizes (16@2x is the 32 px drawing, and so on). */
const ICNS = [
  ['icp4', 16],
  ['ic11', 32],
  ['icp5', 32],
  ['ic12', 64],
  ['ic07', 128],
  ['ic13', 256],
  ['ic08', 256],
  ['ic14', 512],
  ['ic09', 512],
  ['ic10', 1024],
];

/** Font of the names: Segoe UI on Windows, Arial Bold elsewhere. */
const FONT = "'Segoe UI', Arial";

/**
 * The names are capitals, which sit on the baseline and reach about 0.7 of the font size up
 * (Segoe UI): the baseline goes half that below the middle of the block.
 */
const CAP = 0.35;

const PAGE = '#FFFFFF';
const FOLD = '#E4E9F0';
const EDGE = '#C5CDD8';

/** The 256 px drawing. */
function large(label, color) {
  const fs = label.length <= 3 ? 78 : 66;
  const teeth = [22, 34, 46, 58, 70, 82, 94]
    .map((y, i) => `<rect x="${i % 2 ? 136 : 124}" y="${y}" width="12" height="8" rx="3"/>`)
    .join('');
  const text = `<text x="136" y="${(188 + CAP * fs).toFixed(1)}" text-anchor="middle" font-family="${FONT}" font-size="${fs}" font-weight="700" letter-spacing="1" fill="#FFFFFF">${label}</text>`;
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

/**
 * Letters of the format names for the small sizes, 3 × 5 pixels each (rows top to bottom, `1` a
 * pixel): a font would blur at these sizes.
 */
const GLYPHS = {
  A: ['010', '101', '111', '101', '101'],
  B: ['110', '101', '110', '101', '110'],
  G: ['011', '100', '101', '101', '011'],
  I: ['111', '010', '010', '010', '111'],
  L: ['100', '100', '100', '100', '111'],
  O: ['010', '101', '101', '101', '010'],
  P: ['110', '101', '110', '100', '100'],
  R: ['110', '101', '110', '101', '101'],
  S: ['011', '100', '010', '001', '110'],
  T: ['111', '010', '010', '010', '010'],
  X: ['101', '101', '010', '101', '101'],
  Z: ['111', '001', '010', '100', '111'],
  2: ['110', '001', '010', '100', '111'],
  4: ['101', '101', '111', '001', '001'],
  7: ['111', '001', '010', '010', '010'],
};

/**
 * Pixel grid of each small size: the page's left and right edge, its fold, the name block
 * (x, y, width, height) and the scale of the letters. The block is as wide as the icon allows,
 * so that four letters fit; the letters sit on whole pixels.
 */
const GRIDS = {
  16: { page: [2, 14], fold: 4, block: [0, 8, 16, 7], scale: 1 },
  20: { page: [3, 17], fold: 5, block: [1, 10, 18, 9], scale: 1 },
  24: { page: [4, 20], fold: 6, block: [1, 13, 22, 9], scale: 1 },
  32: { page: [5, 27], fold: 8, block: [0, 16, 32, 14], scale: 2 },
  40: { page: [7, 33], fold: 10, block: [1, 21, 38, 16], scale: 2 },
};

/**
 * The drawing for 40 px and below, on the pixel grid of `size`. Up to 24 px the name is in pixel
 * letters; 32 and 40 px have room for the font (larger than the scaled 256 px drawing would
 * make it) and for the zipper.
 */
function small(label, color, size) {
  const { page, fold, block, scale: s } = GRIDS[size];
  const [left, right] = [page[0] + 0.5, page[1] - 0.5];
  const [top, bottom] = [0.5, size - 0.5];
  const [bx, by, bw, bh] = block;
  let name;
  if (s === 1) {
    const width = label.length * 4 - 1;
    const x0 = bx + Math.floor((bw - width) / 2);
    const y0 = by + Math.floor((bh - 5) / 2);
    const pixels = [...label]
      .flatMap((ch, i) =>
        GLYPHS[ch].flatMap((row, y) =>
          [...row].flatMap((bit, x) => (bit === '1' ? [`<rect x="${x0 + i * 4 + x}" y="${y0 + y}" width="1" height="1"/>`] : [])),
        ),
      )
      .join('');
    name = `<g fill="#FFFFFF" shape-rendering="crispEdges">${pixels}</g>`;
  } else {
    const fs = bh * (label.length <= 3 ? 0.86 : 0.72);
    name = `<text x="${bx + bw / 2}" y="${(by + bh / 2 + CAP * fs).toFixed(1)}" text-anchor="middle" font-family="${FONT}" font-size="${fs.toFixed(1)}" font-weight="700" fill="#FFFFFF">${label}</text>`;
  }
  const cx = Math.floor((page[0] + page[1]) / 2) - 1;
  const teeth = [];
  for (let y = 2, i = 0; y + 2 <= by - 6; y += 3, i++) {
    teeth.push(`<rect x="${cx + (i % 2 ? 2 : 0)}" y="${y}" width="2" height="2"/>`);
  }
  const zipper =
    s > 1
      ? `<g fill="${color}" opacity="0.6" shape-rendering="crispEdges">${teeth.join('')}</g><rect x="${cx}" y="${by - 5}" width="4" height="4" rx="1" fill="${color}"/>`
      : '';
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">
  <path d="M${left} ${top} H${right - fold} L${right} ${top + fold} V${bottom} H${left} Z" fill="${PAGE}" stroke="#8E9AAB" stroke-width="1" shape-rendering="crispEdges"/>
  <path d="M${right - fold} ${top} V${top + fold} H${right} Z" fill="${FOLD}" stroke="#8E9AAB" stroke-width="1"/>
  ${zipper}
  <rect x="${bx}" y="${by}" width="${bw}" height="${bh}" rx="${s}" fill="${color}" shape-rendering="crispEdges"/>
  ${name}
</svg>
`;
}


mkdirSync(out, { recursive: true });
for (const [ext, label, color] of FORMATS) {
  const drawn = new Map();
  const draw = (size) => {
    if (!drawn.has(size)) drawn.set(size, png(size <= 40 ? small(label, color, size) : large(label, color), size));
    return drawn.get(size);
  };
  writeFileSync(path.join(out, `${ext}.ico`), ico(SIZES.map((size) => ({ size, data: draw(size) }))));
  writeFileSync(path.join(out, `${ext}.icns`), icns(ICNS.map(([type, size]) => ({ type, data: draw(size) }))));
  writeFileSync(path.join(out, `${ext}.svg`), large(label, color));
  console.log(`icons/filetypes/${ext}.ico, .icns`);
}
