// Line icons (24×24 grid, 1.6px stroke), drawn for OmniZip in the style of Lucide.

import appIcon from '@icons/app-icon.svg';

const FILE = 'M6.5 3.5h7l4 4V19a1.5 1.5 0 0 1-1.5 1.5H6.5A1.5 1.5 0 0 1 5 19V5a1.5 1.5 0 0 1 1.5-1.5z M13.5 3.5v4h4';
// `tone:` parts are the filled second tone of duotone file icons (docs/05-theming.md §7),
// drawn under the outline and invisible everywhere else.
const FILE_TONE = 'tone:M6.5 3.5h7l4 4V19a1.5 1.5 0 0 1-1.5 1.5H6.5A1.5 1.5 0 0 1 5 19V5a1.5 1.5 0 0 1 1.5-1.5z';
const EYE = 'M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6z';

const PATHS: Record<string, string[]> = {
  home: ['M4 11l8-7 8 7v9a1 1 0 0 1-1 1h-5v-6h-4v6H5a1 1 0 0 1-1-1z'],
  clock: ['M12 7.5V12l3 2', 'circle:12,12,8.5'],
  folder: [
    'tone:M3.5 7.5A1.5 1.5 0 0 1 5 6h4.2l2 2H19a1.5 1.5 0 0 1 1.5 1.5V18A1.5 1.5 0 0 1 19 19.5H5A1.5 1.5 0 0 1 3.5 18z',
    'M3.5 7.5A1.5 1.5 0 0 1 5 6h4.2l2 2H19a1.5 1.5 0 0 1 1.5 1.5V18A1.5 1.5 0 0 1 19 19.5H5A1.5 1.5 0 0 1 3.5 18z',
  ],
  folderOpen: [
    'tone:M3.8 19.5h14.6a1 1 0 0 0 .95-.7l2-6.3a.8.8 0 0 0-.77-1H7.3a1 1 0 0 0-.95.7L3.5 19z',
    'M3.5 7.5A1.5 1.5 0 0 1 5 6h4.2l2 2H18a1.5 1.5 0 0 1 1.5 1.5V11',
    'M3.8 19.5h14.6a1 1 0 0 0 .95-.7l2-6.3a.8.8 0 0 0-.77-1H7.3a1 1 0 0 0-.95.7L3.5 19',
  ],
  file: [FILE_TONE, FILE],
  fileText: [FILE_TONE, FILE, 'M8.5 12h7M8.5 15.5h7'],
  fileCode: [FILE_TONE, FILE, 'M10 11.5l-2 2 2 2M14 11.5l2 2-2 2'],
  fileImage: [FILE_TONE, FILE, 'circle:9.5,11.5,1.2', 'M7 18l3.5-3.5 2 2 2-2.5L17 18'],
  fileVideo: [FILE_TONE, FILE, 'M10 11.5v5l4.5-2.5z'],
  fileFont: [FILE_TONE, FILE, 'M9 17l3-7 3 7M10 15h4'],
  archive: ['tone:M5 5h14a1 1 0 0 1 1 1v2a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1zM5.5 9h13v9a1.5 1.5 0 0 1-1.5 1.5H7A1.5 1.5 0 0 1 5.5 18z', 'rect:4,5,16,4,1', 'M5.5 9v9A1.5 1.5 0 0 0 7 19.5h10a1.5 1.5 0 0 0 1.5-1.5V9', 'M10 13h4'],
  lock: ['rect:5.5,10.5,13,9.5,1.5', 'M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5'],
  link: ['M10 14a4 4 0 0 0 5.66 0l3-3a4 4 0 0 0-5.66-5.66l-1 1', 'M14 10a4 4 0 0 0-5.66 0l-3 3a4 4 0 0 0 5.66 5.66l1-1'],
  search: ['circle:11,11,6', 'M15.5 15.5L20 20'],
  plus: ['M12 5v14M5 12h14'],
  extract: ['M12 4v11', 'M8 11l4 4 4-4', 'M4.5 16.5V19a1 1 0 0 0 1 1h13a1 1 0 0 0 1-1v-2.5'],
  upload: ['M12 16V5M8 9l4-4 4 4', 'M4.5 16.5V19a1 1 0 0 0 1 1h13a1 1 0 0 0 1-1v-2.5'],
  check: ['M5 12.5l4.5 4.5L19 7.5'],
  checkCircle: ['circle:12,12,8.5', 'M8.5 12.5l2.5 2.5 4.5-5'],
  trash: ['M4.5 7h15M9.5 7V4.5h5V7M7 7l.8 12.1a1 1 0 0 0 1 .9h6.4a1 1 0 0 0 1-.9L17 7', 'M10 11v5M14 11v5'],
  info: ['circle:12,12,8.5', 'M12 11v5M12 8h.01'],
  chevronLeft: ['M14.5 6l-6 6 6 6'],
  chevronRight: ['M9.5 6l6 6-6 6'],
  chevronUp: ['M6 14.5l6-6 6 6'],
  chevronDown: ['M6 9.5l6 6 6-6'],
  arrowUp: ['M12 19V5M6 11l6-6 6 6'],
  grid: ['rect:4,4,7,7,1', 'rect:13,4,7,7,1', 'rect:4,13,7,7,1', 'rect:13,13,7,7,1'],
  list: ['M9 6h11M9 12h11M9 18h11', 'M4.5 6h.01M4.5 12h.01M4.5 18h.01'],
  x: ['M6 6l12 12M18 6L6 18'],
  minus: ['M5 12h14'],
  square: ['rect:5.5,5.5,13,13,1'],
  restore: ['rect:5.5,8.5,10,10,1', 'M8.5 8.5V6.5a1 1 0 0 1 1-1h8a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1h-2'],
  drive: ['rect:3.5,8,17,8,2', 'M7 12h.01M10 12h.01'],
  eject: ['M5.5 14.5h13L12 6z', 'M5.5 18h13'],
  play: ['M8 5.5v13l10-6.5z'],
  pause: ['M8 5.5v13M16 5.5v13'],
  sliders: ['M5 7h14M5 12h14M5 17h14', 'circle-fill:9,7,2', 'circle-fill:15,12,2', 'circle-fill:10,17,2'],
  moon: ['M19 14.5A7.5 7.5 0 0 1 9.5 5a7.5 7.5 0 1 0 9.5 9.5z'],
  monitor: ['M5 4.5h14A1.5 1.5 0 0 1 20.5 6v9a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 15V6A1.5 1.5 0 0 1 5 4.5z', 'M8.5 20h7M12 16.5V20'],
  sun: ['circle:12,12,4', 'M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4L7 17M17 7l1.4-1.4'],
  globe: ['circle:12,12,8.5', 'M3.5 12h17M12 3.5c3 3 3 14 0 17M12 3.5c-3 3-3 14 0 17'],
  palette: [
    'M12 3.5a8.5 8.5 0 1 0 0 17c1.2 0 2-.8 2-1.8 0-1.4-1.5-1.7-1.5-3 0-1 .8-1.7 1.8-1.7H16a4.5 4.5 0 0 0 4.5-4.5c0-3.4-3.8-6-8.5-6z',
    'circle:8,10,1', 'circle:11,7,1', 'circle:15,8,1',
  ],
  terminal: ['M5 7l5 5-5 5M12 17h7'],
  more: ['M6 12h.01M12 12h.01M18 12h.01'],
  menu: ['M4 7h16M4 12h16M4 17h16'],
  key: ['circle:8,14,3.5', 'M10.5 11.5L19 3M15 7l2 2M17.5 4.5l2 2'],
  shield: ['M12 3.5l7 2.5v5.5c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6z'],
  eye: [EYE, 'circle:12,12,2.5'],
  eyeOff: [EYE, 'circle:12,12,2.5', 'M4 4l16 16'],
  refresh: ['M20 12a8 8 0 0 1-14.5 4.6M4 12a8 8 0 0 1 14.5-4.6', 'M20 4v4.5h-4.5M4 20v-4.5h4.5'],
  alert: ['M12 4l9 16H3z', 'M12 10v4M12 17h.01'],
  copy: ['rect:9,9,11,11,1.5', 'M5 15V5a1 1 0 0 1 1-1h10'],
  edit: ['M4 20h4l11-11-4-4L4 16z', 'M13 7l4 4'],
  external: ['M14 4h6v6M20 4l-9 9', 'M19 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1h5'],
  package: ['M12 3l8 4.5v9L12 21l-8-4.5v-9z', 'M4 7.5l8 4.5 8-4.5M12 12v9'],
  cpu: ['rect:6,6,12,12,1.5', 'rect:9.5,9.5,5,5,0', 'M9 3v3M15 3v3M9 18v3M15 18v3M3 9h3M3 15h3M18 9h3M18 15h3'],
  layout: ['rect:3.5,4,17,16,1.5', 'M9 4v16M3.5 9h17'],
  download: ['M12 4v11M8 11l4 4 4-4', 'M4.5 16.5V19a1 1 0 0 0 1 1h13a1 1 0 0 0 1-1v-2.5'],
  history: ['M4 12a8 8 0 1 1 2.3 5.7', 'M4 7v5h5', 'M12 8v4l3 2'],
  scissors: ['circle:6,6,2.5', 'circle:6,18,2.5', 'M20 4L8.1 15.9', 'M14.5 14.5L20 20', 'M8.1 8.1L12 12'],
  clipboard: ['rect:8.5,3,7,3.5,1', 'M15.5 5h2A1.5 1.5 0 0 1 19 6.5v12a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 5 18.5v-12A1.5 1.5 0 0 1 6.5 5h2'],
  sidebar: ['rect:3.5,4.5,17,15,1.5', 'M9.5 4.5v15'],
  folderPlus: ['M3.5 7.5A1.5 1.5 0 0 1 5 6h4.2l2 2H19a1.5 1.5 0 0 1 1.5 1.5V18A1.5 1.5 0 0 1 19 19.5H5A1.5 1.5 0 0 1 3.5 18z', 'M12 11v5M9.5 13.5h5'],
};

function shape(spec: string, i: number) {
  const [kind, args] = spec.includes(':') ? spec.split(':') : ['path', spec];
  const n = (args ?? '').split(',').map(Number);
  switch (kind) {
    case 'circle':
      return <circle key={i} cx={n[0]} cy={n[1]} r={n[2]} />;
    case 'circle-fill':
      return <circle key={i} cx={n[0]} cy={n[1]} r={n[2]} fill="var(--bg-elevated)" />;
    case 'rect':
      return <rect key={i} x={n[0]} y={n[1]} width={n[2]} height={n[3]} rx={n[4]} />;
    case 'tone':
      return <path key={i} className="tone" d={args} />;
    default:
      return <path key={i} d={spec} />;
  }
}

export type IconName = keyof typeof PATHS;

export function Icon({ name, className, title }: { name: IconName | string; className?: string; title?: string }) {
  const parts = PATHS[name] ?? PATHS.file;
  return (
    <svg
      className={`ic${className ? ` ${className}` : ''}`}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden={title ? undefined : true}
      role={title ? 'img' : undefined}
    >
      {title && <title>{title}</title>}
      {parts.map(shape)}
    </svg>
  );
}

/** The app icon: the same artwork as the exe and package icons. */
export function Logo({ size = 22 }: { size?: number }) {
  return <img className="logo" src={appIcon} width={size} height={size} alt="" draggable={false} />;
}

const IMAGE = /^(jpe?g|png|gif|webp|svg|bmp|heic|ico|icns|tiff?|avif)$/;
const MEDIA = /^(mp4|mkv|mov|avi|webm|mp3|flac|wav|m4a|ogg|aac)$/;
const FONT = /^(woff2?|ttf|otf|pf2)$/;
const CODE = /^(ts|tsx|js|jsx|mjs|rs|c|h|cc|cpp|hpp|py|go|java|kt|swift|cs|css|scss|html|vue|json|toml|ya?ml|lock|cfg|ini|sh|ps1|bat|xml|plist|sql|gradle)$/;
const TEXT = /^(md|txt|env|srt|log|csv|rst|tex|rtf|license)$/;
const ARCHIVE = /^(zip|7z|rar|tar|gz|tgz|bz2|xz|zst|lz4|br|iso|img|cab|deb|rpm|jar|apk|squashfs|dmg|wim)$/;

/** `[icon, colour class]` for a file name. */
export function fileIcon(name: string, isDir: boolean, kind?: string): [IconName, string] {
  if (isDir) return ['folder', 'folder'];
  if (kind === 'symlink' || kind === 'hardlink') return ['link', 'text'];
  const i = name.lastIndexOf('.');
  const e = i > 0 ? name.slice(i + 1).toLowerCase() : '';
  if (IMAGE.test(e)) return ['fileImage', 'image'];
  if (MEDIA.test(e)) return ['fileVideo', 'video'];
  if (FONT.test(e)) return ['fileFont', 'font'];
  if (ARCHIVE.test(e)) return ['archive', 'archive'];
  if (CODE.test(e)) return ['fileCode', 'code'];
  if (TEXT.test(e) || !e) return ['fileText', 'text'];
  return ['file', 'file'];
}

export function FileIcon({ name, isDir, kind, big }: { name: string; isDir: boolean; kind?: string; big?: boolean }) {
  const [ic, cls] = fileIcon(name, isDir, kind);
  return <Icon name={ic} className={`fi ${cls}${big ? ' big' : ''}`} />;
}

/** Badge colors per format (stable across themes, docs/05-theming.md §7). */
export function formatBadgeStyle(format: string, dark: boolean): React.CSSProperties {
  const base: Record<string, string> = { zip: '#2F6FED', '7z': '#7A5AF8', rar: '#C0392B', iso: '#0E9F8E' };
  const c = base[format] ?? (format.startsWith('tar') ? '#8A6D3B' : '#64748B');
  const hex = (h: string) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  const [r, g, b] = hex(c);
  const mix = (t: number, to: number) => [r, g, b].map((v) => Math.round(v + (to - v) * t));
  const fg = dark ? mix(0.35, 255) : mix(0.15, 0);
  return {
    ['--badge-bg' as string]: `rgba(${r},${g},${b},${dark ? 0.22 : 0.14})`,
    ['--badge-fg' as string]: `rgb(${fg.join(',')})`,
  };
}
