// Icons come from omni-ui (shared with the other Omni apps). This module adds what is OmniZip's
// own: the app icon and the badge colours of archive formats.

import appIcon from '@icons/app-icon.svg';
import { badgeStyle } from 'omni-ui';

export { Icon, FileIcon, fileIcon, GitHubMark, type IconName } from 'omni-ui';

/** The app icon: the same artwork as the exe and package icons. */
export function Logo({ size = 22 }: { size?: number }) {
  return <img className="logo" src={appIcon} width={size} height={size} alt="" draggable={false} />;
}

/** Badge colors per format (stable across themes, docs/05-theming.md §7). */
export function formatBadgeStyle(format: string, dark: boolean): React.CSSProperties {
  // The colours of the file-type icons (scripts/gen-filetype-icons.mjs), by format family.
  const base: Record<string, string> = { zip: '#2F6FED', zipx: '#2F6FED', '7z': '#7A5AF8', rar: '#C0392B', iso: '#0B7F72' };
  const stream = ['gz', 'bz2', 'xz', 'lzma', 'zst', 'lz4', 'br'];
  const c = base[format] ?? (format.startsWith('tar') ? '#8A6D3B' : stream.includes(format) ? '#4B5B73' : '#64748B');
  return badgeStyle(c, dark);
}
