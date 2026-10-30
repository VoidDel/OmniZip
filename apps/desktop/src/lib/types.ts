// Types mirroring the Rust IPC payloads (apps/desktop/src-tauri/src/commands.rs).

export type FormatId =
  | 'zip' | '7z' | 'rar' | 'tar' | 'tar.gz' | 'tar.bz2' | 'tar.xz' | 'tar.zst' | 'tar.lz4' | 'tar.br'
  | 'gz' | 'bz2' | 'xz' | 'lzma' | 'zst' | 'lz4' | 'br'
  | 'iso' | 'cab' | 'cpio' | 'ar' | 'deb' | 'rpm' | 'xar' | 'arj' | 'lzh' | 'wim';

export type Platform = 'win' | 'mac' | 'linux';
export type EntryKind = 'file' | 'dir' | 'symlink' | 'hardlink' | 'other';
export type Level = 'store' | 'fastest' | 'fast' | 'normal' | 'better' | 'best' | 'ultra';
export type OverwriteMode = 'ask' | 'skip' | 'overwrite' | 'overwrite-older' | 'rename';
export type ConflictChoice = 'overwrite' | 'skip' | 'rename' | 'overwrite-older' | 'abort';
export type MountClass = 'A' | 'B' | 'C' | 'D';

export interface ErrorReport {
  code: string;
  key: string;
  params: Record<string, string>;
  message: string;
}

export interface Settings {
  schema_version: number;
  general: {
    startup: 'home' | 'last';
    extract_to: 'ask' | 'same' | 'downloads';
    close_behavior: 'exit' | 'tray';
    notify: boolean;
    open_after_extract: boolean;
  };
  appearance: {
    mode: 'system' | 'light' | 'dark';
    theme_light: string;
    theme_dark: string;
    accent: string;
    material: boolean;
    material_kind: 'mica' | 'mica-alt' | 'acrylic';
    density: 'compact' | 'comfortable' | 'spacious';
    icon_style: 'mono' | 'duotone';
    font_size: number;
    sidebar_collapsed: boolean;
    view_mode: 'table' | 'grid';
  };
  language: { ui: string; size_unit: 'auto' | 'kb' | 'kib'; default_encoding: string };
  formats: {
    default_format: string;
    default_level: Level;
    associations: Record<string, boolean>;
    /** Store files whose content is compressed already (ZIP, 7z): the compress dialog's default. */
    skip_compressed: boolean;
  };
  shell: { style: 'cascade' | 'flat'; icons: boolean; items: Record<string, boolean> };
  security: { max_ratio: number; propagate_origin: boolean; symlinks: 'inside' | 'file' | 'skip' };
  mount: {
    cache_gb: number;
    warm_solid: boolean;
    offline_attribute: boolean;
    /** Mount with a temporary writable layer (the mount dialog's default). */
    writable: boolean;
  };
  advanced: { threads: number; log_level: string };
}

export interface ThemeSummary {
  id: string;
  name: Record<string, string>;
  appearance: 'light' | 'dark';
  highContrast: boolean;
  builtin: boolean;
  colors: Record<string, string>;
  contrast: number;
}

export interface ResolvedTheme {
  id: string;
  appearance: 'light' | 'dark';
  highContrast: boolean;
  tokens: Record<string, string>;
  adjusted: string[];
}

export interface SystemInfo {
  platform: Platform;
  accentColor: string | null;
  materialSupported: boolean;
  locale: string;
  homeDir: string | null;
  downloadsDir: string | null;
  desktopDir: string | null;
  pathSeparator: string;
  appVersion: string;
  /** The project's page (source repository). */
  homepage: string;
}

export interface FormatSupport {
  format: FormatId;
  label: string;
  engine: string | null;
  capabilities: string[];
  tier: number;
  extensions: string[];
}

export type LaunchRequest =
  | { kind: 'open'; path: string }
  | { kind: 'extract'; paths: string[] }
  | { kind: 'compress'; paths: string[] }
  | { kind: 'mount'; path: string }
  | { kind: 'convert'; paths: string[] };

export interface Bootstrap {
  settings: Settings;
  themes: ThemeSummary[];
  system: SystemInfo;
  launch: LaunchRequest | null;
  formats: FormatSupport[];
  writeMethods: Record<string, string[]>;
  encodings: string[];
  languages: [string, string][];
  configPath: string | null;
}

export interface ArchiveInfo {
  path: string;
  format: FormatId;
  methods: string[];
  solid: boolean;
  encrypted: boolean;
  headerEncrypted: boolean;
  encryption: string | null;
  comment: string | null;
  volumes: number;
  encoding: string | null;
  archiveSize: number;
  sfx: boolean;
}

export interface ArchiveSummary {
  id: number;
  path: string;
  name: string;
  info: ArchiveInfo;
  totalSize: number;
  packedSize: number;
  files: number;
  dirs: number;
  writable: boolean;
  capabilities: string[];
  mountClass: MountClass;
  /** A new archive not saved yet: edits change memory, saving compresses it. */
  draft: boolean;
  entries: number;
}

export interface Row {
  node: number;
  name: string;
  isDir: boolean;
  kind: EntryKind;
  size: number;
  packed: number;
  mtime: number | null;
  encrypted: boolean;
  files: number;
  children: number;
  parent: string | null;
}

export interface ListPage {
  total: number;
  offset: number;
  rows: Row[];
}

export interface TreeItem {
  node: number;
  name: string;
  hasChildren: boolean;
  nestedArchive: boolean;
}

export interface Crumb {
  node: number;
  name: string;
}

export interface NodeInfo {
  node: number;
  name: string;
  path: string;
  isDir: boolean;
  kind: EntryKind;
  size: number;
  packed: number;
  files: number;
  mtime: number | null;
  crc32: number | null;
  method: string | null;
  encrypted: boolean;
  linkTarget: string | null;
  comment: string | null;
}

export type Preview =
  | { kind: 'text'; text: string; truncated: boolean; encoding: string; language: string | null }
  | { kind: 'image'; dataUrl: string; width: number; height: number }
  | { kind: 'binary'; hex: string; truncated: boolean }
  | { kind: 'unsupported' };

export interface Progress {
  phase: string;
  bytesDone: number;
  bytesTotal: number;
  filesDone: number;
  filesTotal: number;
  current: string | null;
}

export interface Warning {
  code: string;
  path: string;
  detail: string | null;
}

export interface Report {
  files: number;
  dirs: number;
  links: number;
  bytes: number;
  skipped: number;
  warnings: Warning[];
  outputs: string[];
}

export type JobKind = 'extract' | 'compress' | 'convert' | 'test' | 'add' | 'delete' | 'rename' | 'move' | 'copy' | 'clipcopy' | 'clipcut' | 'mountSave';
export type JobState = 'running' | 'paused' | 'done' | 'failed' | 'cancelled';

export interface JobInfo {
  id: number;
  kind: JobKind;
  title: string;
  state: JobState;
  progress: Progress;
  speed: number;
  eta: number | null;
  started: number;
  finished: number | null;
  report: Report | null;
  error: ErrorReport | null;
  output: string | null;
  window: string | null;
  archiveId: number | null;
}

export interface PromptEvent {
  jobId: number;
  window: string | null;
  promptId: number;
  kind: 'password' | 'conflict' | 'confirm';
  data: Record<string, unknown>;
}

export type PromptAnswer =
  | { kind: 'password'; value: string | null }
  | { kind: 'conflict'; choice: ConflictChoice; applyToAll: boolean }
  | { kind: 'confirm'; yes: boolean };

export interface ExtractRequest {
  archiveId?: number;
  paths?: string[];
  nodes?: number[];
  relativeTo?: number;
  destination: string;
  pathMode: 'smart' | 'full' | 'flat';
  overwrite: OverwriteMode;
  password?: string | null;
  encoding?: string | null;
  openAfter?: boolean;
  deleteAfter?: boolean;
  keepBroken?: boolean;
  eachToFolder?: boolean;
}

export interface CompressRequest {
  paths: string[];
  destination: string;
  format: FormatId;
  level: Level;
  method?: string | null;
  password?: string | null;
  encryptNames?: boolean;
  solid?: boolean;
  volumeSize?: number | null;
  comment?: string | null;
  exclude?: string[];
  deleteSource?: boolean;
  verify?: boolean;
  openAfter?: boolean;
  /** Save this new archive instead of collecting `paths`. */
  draft?: number | null;
  /** Store files whose content is compressed already (photos, video, archives). */
  skipCompressed?: boolean;
}

/** Archives on disk written again in another format, each to the path shown in the dialog. */
export interface ConvertRequest {
  items: { source: string; destination: string }[];
  format: FormatId;
  level: Level;
  verify?: boolean;
  deleteSource?: boolean;
  openAfter?: boolean;
}

export interface RecentItem {
  path: string;
  name: string;
  format: string;
  size: number;
  opened: number;
}

export interface ShellStatus {
  supported: boolean;
  /** The classic menu. */
  registered: boolean;
  dll: string | null;
  stale: boolean;
  /** The Windows 11 top-level menu: no package next to the app, not registered, registered. */
  win11: 'none' | 'off' | 'on';
}

export interface PathStats {
  bytes: number;
  files: number;
  commonParent: string | null;
  suggestedName: string;
}

export interface MountProbe {
  kind: 'win-fsp' | 'fuse' | 'fs-kit' | 'nfs-loopback';
  health: 'ready' | 'not-installed' | 'needs-enable' | 'version-mismatch' | 'unsupported';
  detail: string | null;
  built: boolean;
}

/** A mounted archive. */
export interface MountInfo {
  id: number;
  archive: string;
  /** File name of the archive. */
  name: string;
  label: string;
  /** `Z:` or a folder. */
  mountPoint: string;
  format: string;
  /** Bytes decoded into the cache. */
  cacheBytes: number;
  /** A solid archive is being decoded into the cache. */
  warming: boolean;
  /** Mounted as administrator (by the elevated mount daemon). */
  elevated: boolean;
  /** A drive visible to every session (registered with the Mount Manager). */
  global: boolean;
  /** Writes go to a temporary layer. */
  writable: boolean;
  /** The layer holds changes not saved into the archive. */
  dirty: boolean;
  /** The changes can be saved into the archive (not RAR, not split, not self-extracting). */
  saveable: boolean;
}

export interface MountRequest {
  path: string;
  /** The archive open in the browser, whose password and name encoding are reused. */
  archiveId?: number | null;
  /** `Z:` or a folder; the next free drive letter when absent. */
  mountPoint?: string | null;
  label?: string | null;
  warm?: boolean | null;
  password?: string | null;
  /** Mount through the elevated daemon (asks for administrator rights). */
  elevated?: boolean;
  /** With a temporary writable layer (the setting when absent). */
  writable?: boolean;
}

/** The archive a mount dialog is for. */
export interface MountTarget {
  path: string;
  name: string;
  format: FormatId;
  mountClass: MountClass;
  /** A password is needed to mount it. */
  encrypted: boolean;
  /** The archive open in the browser, whose password and name encoding are reused. */
  archiveId?: number;
}

/** Items copied or cut from an open archive. */
export interface Clip {
  archive: number;
  paths: string[];
  cut: boolean;
}
