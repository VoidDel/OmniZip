// Draws the icons of the context-menu items (docs/07-shell-integration.md §3.2): line glyphs in
// the style of the app's icons (Icon.tsx: 24 px grid, 1.6 px strokes), one set for light and one
// for dark menus. Writes icons/menu/<verb>-light.ico and <verb>-dark.ico (16–48 px, for 100–300 %
// scaling); the menu extension picks the set by the apps theme. "Open with OmniZip" and the
// submenu itself carry the app icon.
//
//   node scripts/gen-menu-icons.mjs
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { ico, png } from './lib/icons.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const out = path.join(root, 'icons/menu');

const FOLDER = 'M3.5 7.5A1.5 1.5 0 0 1 5 6h4.2l2 2H19a1.5 1.5 0 0 1 1.5 1.5V18A1.5 1.5 0 0 1 19 19.5H5A1.5 1.5 0 0 1 3.5 18z';
const TRAY = 'M4.5 16.5V19a1 1 0 0 0 1 1h13a1 1 0 0 0 1-1v-2.5';
const BOX = 'M12 3l8 4.5v9L12 21l-8-4.5v-9zM4 7.5l8 4.5 8-4.5M12 12v9';

/** Verb (`--shell` value) → SVG shapes on the 24 px grid. */
const GLYPHS = {
  'extract-here': [`<path d="M12 4v11M8 11l4 4 4-4"/>`, `<path d="${TRAY}"/>`],
  'extract-smart': [`<path d="${FOLDER}"/>`, `<path d="M12 10.5v6M9.5 14l2.5 2.5 2.5-2.5"/>`],
  'extract-to': [
    `<path d="M3.5 7.5A1.5 1.5 0 0 1 5 6h4.2l2 2H18a1.5 1.5 0 0 1 1.5 1.5v1"/>`,
    `<path d="M3.5 7.5V18a1.5 1.5 0 0 0 1.5 1.5h12.6a1.5 1.5 0 0 0 1.4-1l2-6a1 1 0 0 0-.9-1.3H7.3a1.5 1.5 0 0 0-1.4 1L3.5 18"/>`,
  ],
  'extract-each': [`<rect x="8.5" y="8.5" width="11" height="11" rx="1.5"/>`, `<path d="M5 15V5.5A1 1 0 0 1 6 4.5h9.5"/>`],
  mount: [`<rect x="3.5" y="8" width="17" height="8" rx="2"/>`, `<path d="M7 12h.01M10 12h.01"/>`],
  test: [`<circle cx="12" cy="12" r="8.5"/>`, `<path d="M8.5 12.5l2.5 2.5 4.5-5"/>`],
  convert: [`<path d="M20 12a8 8 0 0 1-14.5 4.6M4 12a8 8 0 0 1 14.5-4.6"/>`, `<path d="M20 4v4.5h-4.5M4 20v-4.5h4.5"/>`],
  'add-zip': [`<path d="${BOX}"/>`],
  'add-7z': [`<path d="${BOX}"/>`],
  add: [`<path d="${BOX}"/>`, `<circle cx="18" cy="18" r="4.5" fill="#BG#" stroke="none"/>`, `<path d="M18 15.8v4.4M15.8 18h4.4"/>`],
};

/** Glyph colour on a light / dark menu, and the menu colour behind a badge. */
const THEMES = { light: ['#202020', '#F9F9F9'], dark: ['#F3F3F3', '#2C2C2C'] };

/** Pixel sizes: 16 px menus at 100–300 % scaling. */
const SIZES = [16, 20, 24, 32, 40, 48];

mkdirSync(out, { recursive: true });
for (const [verb, shapes] of Object.entries(GLYPHS)) {
  for (const [theme, [ink, menu]] of Object.entries(THEMES)) {
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="${ink}" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">${shapes.join('').replaceAll('#BG#', menu)}</svg>`;
    const images = SIZES.map((size) => ({ size, data: png(svg, size) }));
    writeFileSync(path.join(out, `${verb}-${theme}.ico`), ico(images));
  }
  console.log(`icons/menu/${verb}-{light,dark}.ico`);
}
