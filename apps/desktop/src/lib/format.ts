// Locale-aware formatting (docs/06-i18n.md §5).

import type { Platform } from './types';

export type SizeUnit = 'auto' | 'kb' | 'kib';

/** Unit system: macOS uses 1000-based KB, Linux shows KiB, Windows shows 1024-based "KB". */
export function unitSystem(unit: SizeUnit, platform: Platform): { base: number; labels: string[] } {
  const u = unit === 'auto' ? (platform === 'mac' ? 'kb' : platform === 'linux' ? 'kib' : 'winkb') : unit;
  if (u === 'kb') return { base: 1000, labels: ['B', 'KB', 'MB', 'GB', 'TB', 'PB'] };
  if (u === 'kib') return { base: 1024, labels: ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] };
  return { base: 1024, labels: ['B', 'KB', 'MB', 'GB', 'TB', 'PB'] };
}

export function formatSize(bytes: number | null | undefined, unit: SizeUnit, platform: Platform, locale: string): string {
  if (bytes == null || Number.isNaN(bytes)) return '—';
  const { base, labels } = unitSystem(unit, platform);
  let v = bytes;
  let i = 0;
  while (v >= base && i < labels.length - 1) {
    v /= base;
    i += 1;
  }
  const digits = i === 0 ? 0 : v < 10 ? 2 : v < 100 ? 1 : 0;
  const num = new Intl.NumberFormat(locale, { maximumFractionDigits: digits, minimumFractionDigits: digits }).format(v);
  return `${num} ${labels[i]}`;
}

export function formatDate(ms: number | null | undefined, locale: string): string {
  if (ms == null) return '';
  return new Intl.DateTimeFormat(locale, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(new Date(ms));
}

export function formatRelative(ms: number, locale: string, now = Date.now()): string {
  const days = Math.round((ms - now) / 86_400_000);
  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });
  if (Math.abs(days) >= 1) return rtf.format(days, 'day');
  const hours = Math.round((ms - now) / 3_600_000);
  if (Math.abs(hours) >= 1) return rtf.format(hours, 'hour');
  return rtf.format(Math.round((ms - now) / 60_000), 'minute');
}

/** Seconds as `12 s`, `3 min 05 s`, `1 h 02 min` using Intl unit formatting. */
export function formatDuration(seconds: number, locale: string): string {
  const s = Math.max(1, Math.round(seconds));
  const unit = (value: number, u: 'second' | 'minute' | 'hour') =>
    new Intl.NumberFormat(locale, { style: 'unit', unit: u, unitDisplay: 'short' }).format(value);
  if (s < 60) return unit(s, 'second');
  if (s < 3600) return `${unit(Math.floor(s / 60), 'minute')} ${unit(s % 60, 'second')}`;
  return `${unit(Math.floor(s / 3600), 'hour')} ${unit(Math.floor((s % 3600) / 60), 'minute')}`;
}

export function formatPercent(part: number, whole: number, locale: string): string {
  if (!whole) return new Intl.NumberFormat(locale, { style: 'percent' }).format(0);
  return new Intl.NumberFormat(locale, { style: 'percent', maximumFractionDigits: 0 }).format(Math.min(1, part / whole));
}

/** Extension in lower case, empty for none. */
export function extOf(name: string): string {
  const i = name.lastIndexOf('.');
  return i > 0 ? name.slice(i + 1).toLowerCase() : '';
}

/** File name without known archive suffixes: `a.tar.gz` → `a`. */
/** Columns of a default volume label (CJK characters count two), and those kept from its end. */
const LABEL_COLUMNS = 20;
const LABEL_TAIL = 8;

/**
 * A default volume label shortened in the middle with "…" (`short_label` in
 * crates/omnizip-vfs/src/archivefs.rs): Explorer shows a drive's name and letter on one line
 * above its capacity, and a longer name wraps and pushes the capacity out of the drive's tile.
 */
export function shortLabel(label: string): string {
  const width = (c: string) => {
    const n = c.codePointAt(0) ?? 0;
    const wide =
      (n >= 0x1100 && n <= 0x115f) || (n >= 0x2e80 && n <= 0xa4cf) || (n >= 0xac00 && n <= 0xd7a3) || (n >= 0xf900 && n <= 0xfaff) ||
      (n >= 0xfe30 && n <= 0xfe4f) || (n >= 0xff00 && n <= 0xff60) || (n >= 0xffe0 && n <= 0xffe6) || (n >= 0x20000 && n <= 0x3fffd);
    return wide ? 2 : 1;
  };
  const chars = Array.from(label);
  if (chars.reduce((w, c) => w + width(c), 0) <= LABEL_COLUMNS) return label;
  const tail: string[] = [];
  let used = 0;
  for (const c of [...chars].reverse()) {
    if (used + width(c) > LABEL_TAIL) break;
    used += width(c);
    tail.unshift(c);
  }
  const room = LABEL_COLUMNS - 1 - used;
  let head = '';
  used = 0;
  for (const c of chars) {
    if (used + width(c) > room) break;
    used += width(c);
    head += c;
  }
  return `${head.trimEnd()}…${tail.join('').trimStart()}`;
}

export function archiveStem(name: string): string {
  const lower = name.toLowerCase();
  const suffixes = ['.tar.gz', '.tar.bz2', '.tar.xz', '.tar.zst', '.tar.lz4', '.tar.br', '.tgz', '.tbz2', '.txz', '.tzst', '.zip', '.7z', '.rar', '.tar', '.gz', '.bz2', '.xz', '.zst', '.lz4', '.br'];
  let base = lower.replace(/\.part\d+\.rar$/, '.rar').replace(/\.\d{3}$/, '');
  for (const s of suffixes) {
    if (base.endsWith(s)) {
      base = base.slice(0, -s.length);
      break;
    }
  }
  return name.slice(0, base.length) || name;
}

export function basename(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

export function dirname(path: string): string {
  const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  if (i <= 0) return path;
  const dir = path.slice(0, i);
  // A bare "D:" means the current directory of drive D, not its root.
  return /^[A-Za-z]:$/.test(dir) ? dir + path[i] : dir;
}

export function joinPath(dir: string, name: string, sep: string): string {
  if (!dir) return name;
  return dir.endsWith('/') || dir.endsWith('\\') ? dir + name : dir + sep + name;
}
