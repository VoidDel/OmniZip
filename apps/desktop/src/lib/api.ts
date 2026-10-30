// Thin typed wrappers over Tauri IPC. All failures reject with an ErrorReport.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  ArchiveSummary,
  Bootstrap,
  Clip,
  CompressRequest,
  ConvertRequest,
  Crumb,
  ErrorReport,
  ExtractRequest,
  JobInfo,
  LaunchRequest,
  ListPage,
  NodeInfo,
  PathStats,
  Preview,
  PromptAnswer,
  PromptEvent,
  RecentItem,
  ResolvedTheme,
  Settings,
  MountInfo,
  MountProbe,
  MountRequest,
  MountTarget,
  ShellStatus,
  ThemeSummary,
  TreeItem,
} from './types';

export function isErrorReport(e: unknown): e is ErrorReport {
  return typeof e === 'object' && e !== null && 'code' in e && 'key' in e;
}

/** Normalizes anything thrown by invoke into an ErrorReport. */
export function asError(e: unknown): ErrorReport {
  if (isErrorReport(e)) return e;
  return { code: 'E1901', key: 'internal', params: { detail: String(e) }, message: String(e) };
}

export const api = {
  bootstrap: () => invoke<Bootstrap>('app_bootstrap'),
  takeLaunch: () => invoke<LaunchRequest | null>('app_take_launch'),
  /** Request of a standalone dialog window opened from the file manager. */
  shellRequest: () => invoke<LaunchRequest | null>('shell_request'),
  setSettings: (settings: Settings) => invoke<Settings>('settings_set', { settings }),
  resolveTheme: (id: string, options: { accent: string; systemAccent: string | null; material: boolean }) =>
    invoke<ResolvedTheme>('theme_resolve', { id, options }),
  reloadThemes: () => invoke<ThemeSummary[]>('themes_reload'),
  importTheme: (path: string) => invoke<ThemeSummary[]>('theme_import', { path }),
  openThemesFolder: () => invoke<void>('themes_open_folder'),

  openArchive: (path: string, password?: string | null, encoding?: string | null) =>
    invoke<ArchiveSummary>('archive_open', { path, password: password ?? null, encoding: encoding ?? null }),
  reencode: (id: number, encoding: string) => invoke<ArchiveSummary>('archive_reencode', { id, encoding }),
  summary: (id: number) => invoke<ArchiveSummary>('archive_summary', { id }),
  closeArchive: (id: number) => invoke<void>('archive_close', { id }),
  list: (id: number, dir: number, sort: string, desc: boolean, offset: number, limit: number, query: string | null) =>
    invoke<ListPage>('archive_list', { id, dir, sort, desc, offset, limit, query }),
  dirs: (id: number, node: number) => invoke<TreeItem[]>('archive_dirs', { id, node }),
  ancestors: (id: number, node: number) => invoke<Crumb[]>('archive_ancestors', { id, node }),
  node: (id: number, node: number) => invoke<NodeInfo>('archive_node', { id, node }),
  selectionSize: (id: number, nodes: number[]) => invoke<[number, number]>('archive_selection_size', { id, nodes }),
  preview: (id: number, node: number) => invoke<Preview>('archive_preview', { id, node }),
  setArchivePassword: (id: number, password: string) => invoke<void>('archive_set_password', { id, password }),
  openExternal: (id: number, node: number) => invoke<string>('archive_open_external', { id, node }),

  extract: (request: ExtractRequest, window?: string) => invoke<number>('job_extract', { request, window: window ?? null }),
  compress: (request: CompressRequest, window?: string) => invoke<number>('job_compress', { request, window: window ?? null }),
  convert: (request: ConvertRequest, window?: string) => invoke<number>('job_convert', { request, window: window ?? null }),
  test: (archiveId: number | null, paths: string[] = [], password: string | null = null) =>
    invoke<number>('job_test', { archiveId, paths, password }),
  add: (id: number, paths: string[], dir: number) => invoke<number>('job_add', { id, paths, dir }),
  remove: (id: number, nodes: number[]) => invoke<number>('job_delete', { id, nodes }),
  rename: (id: number, node: number, name: string) => invoke<number>('job_rename', { id, node, name }),
  /** Copies items into a folder of an open archive, or moves them into another one. */
  transfer: (from: number, nodes: number[], to: number, dir: number, copy: boolean) =>
    invoke<number>('job_transfer', { from, nodes, to, dir, copy }),
  find: (id: number, path: string) => invoke<number | null>('archive_find', { id, path }),
  /**
   * Drags items out of the window as files, extracted where they are dropped; resolves when the
   * drag ends, to whether they were dropped somewhere.
   */
  dragOut: (id: number, nodes: number[], image?: { png: string; x: number; y: number }) =>
    invoke<boolean>('drag_out', { id, nodes, image: image ?? null }),
  /** Copies (or cuts) items to paste elsewhere; returns the job putting them on the system clipboard (0 if none). */
  clipCopy: (id: number, nodes: number[], cut: boolean) => invoke<number>('clipboard_copy', { id, nodes, cut }),
  /** Pastes what was copied or cut into the folder `dir` of an open archive. */
  clipPaste: (id: number, dir: number) => invoke<number>('clipboard_paste', { id, dir }),
  /** Moves items into the folder `dir` (drag and drop in the browser). */
  move: (id: number, nodes: number[], dir: number) => invoke<number>('job_move', { id, nodes, dir }),
  /** Starts a new archive, optionally with files and folders in it. */
  draftNew: (name: string, paths: string[]) => invoke<ArchiveSummary>('draft_new', { name, paths }),
  mkdir: (id: number, dir: number, name: string) => invoke<void>('draft_mkdir', { id, dir, name }),
  cancelJob: (id: number) => invoke<void>('job_cancel', { id }),
  pauseJob: (id: number) => invoke<void>('job_pause', { id }),
  resumeJob: (id: number) => invoke<void>('job_resume', { id }),
  jobs: () => invoke<JobInfo[]>('job_list'),
  clearJobs: () => invoke<void>('job_clear'),
  prompts: () => invoke<PromptEvent[]>('job_prompts'),
  answer: (jobId: number, promptId: number, answer: PromptAnswer) =>
    invoke<void>('job_answer', { jobId, promptId, answer }),

  classify: (paths: string[]) => invoke<{ archives: string[]; others: string[] }>('paths_classify', { paths }),
  pathStats: (paths: string[]) => invoke<PathStats>('paths_stats', { paths }),
  reveal: (path: string) => invoke<void>('path_reveal', { path }),
  exists: (path: string) => invoke<boolean>('path_exists', { path }),
  recent: () => invoke<RecentItem[]>('recent_list'),
  removeRecent: (path: string) => invoke<void>('recent_remove', { path }),
  clearRecent: () => invoke<void>('recent_clear'),
  windowReady: () => invoke<void>('window_ready'),
  /** Focuses the window and its page (needed for transparent material windows). */
  focusWindow: () => invoke<void>('window_focus'),
  openDefaultApps: () => invoke<void>('open_default_apps'),
  openHomepage: () => invoke<void>('open_homepage'),
  shellStatus: () => invoke<ShellStatus>('shell_status'),
  mountProbe: () => invoke<MountProbe>('mount_probe'),
  mounts: () => invoke<MountInfo[]>('mount_list'),
  /** Drive letters free for a mount (`Z:`, `Y:`, …). */
  freeLetters: () => invoke<string[]>('mount_free_letters'),
  /** Format and mount class of an archive that is not open, for the mount dialog. */
  mountTarget: (path: string) => invoke<MountTarget>('mount_target', { path }),
  mount: (request: MountRequest) => invoke<MountInfo>('mount_start', { request }),
  /** Unmounts; a writable mount's changes are saved into the archive by the job whose id comes
   * back (`save`), owned by `window`, or discarded. `0` when there was nothing to save. */
  unmount: (id: number, save = false, window?: string) => invoke<number>('mount_stop', { id, save, window: window ?? null }),
  openMountDriverPage: () => invoke<void>('open_mount_driver_page'),
  shellRegister: () => invoke<ShellStatus>('shell_register'),
  shellUnregister: () => invoke<ShellStatus>('shell_unregister'),
};

export const events = {
  onJob: (f: (j: JobInfo) => void): Promise<UnlistenFn> => listen<JobInfo>('job://update', (e) => f(e.payload)),
  onPrompt: (f: (p: PromptEvent) => void): Promise<UnlistenFn> => listen<PromptEvent>('job://prompt', (e) => f(e.payload)),
  onArchiveChanged: (f: (id: number) => void): Promise<UnlistenFn> => listen<number>('archive://changed', (e) => f(e.payload)),
  onLaunch: (f: (r: LaunchRequest) => void): Promise<UnlistenFn> => listen<LaunchRequest>('app://launch', (e) => f(e.payload)),
  onClipboard: (f: (c: Clip | null) => void): Promise<UnlistenFn> => listen<Clip | null>('clipboard://changed', (e) => f(e.payload)),
  onMounts: (f: () => void): Promise<UnlistenFn> => listen('mounts://changed', () => f()),
};
