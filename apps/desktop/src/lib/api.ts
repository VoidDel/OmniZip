// Thin typed wrappers over Tauri IPC. All failures reject with an ErrorReport.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  ArchiveSummary,
  Bootstrap,
  CompressRequest,
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
  MountProbe,
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
  test: (archiveId: number | null, paths: string[] = [], password: string | null = null) =>
    invoke<number>('job_test', { archiveId, paths, password }),
  add: (id: number, paths: string[], dir: number) => invoke<number>('job_add', { id, paths, dir }),
  remove: (id: number, nodes: number[]) => invoke<number>('job_delete', { id, nodes }),
  rename: (id: number, node: number, name: string) => invoke<number>('job_rename', { id, node, name }),
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
  openDefaultApps: () => invoke<void>('open_default_apps'),
  shellStatus: () => invoke<ShellStatus>('shell_status'),
  mountProbe: () => invoke<MountProbe>('mount_probe'),
  shellRegister: () => invoke<ShellStatus>('shell_register'),
  shellUnregister: () => invoke<ShellStatus>('shell_unregister'),
};

export const events = {
  onJob: (f: (j: JobInfo) => void): Promise<UnlistenFn> => listen<JobInfo>('job://update', (e) => f(e.payload)),
  onPrompt: (f: (p: PromptEvent) => void): Promise<UnlistenFn> => listen<PromptEvent>('job://prompt', (e) => f(e.payload)),
  onArchiveChanged: (f: (id: number) => void): Promise<UnlistenFn> => listen<number>('archive://changed', (e) => f(e.payload)),
  onLaunch: (f: (r: LaunchRequest) => void): Promise<UnlistenFn> => listen<LaunchRequest>('app://launch', (e) => f(e.payload)),
};
