// Application state (zustand). Data that can be large (directory listings) stays in Rust and is
// fetched page by page; the store only keeps what the UI needs to render the current view.

import { create } from 'zustand';

import i18n from '../i18n';
import { api, asError } from '../lib/api';
import type {
  ArchiveSummary,
  Bootstrap,
  ErrorReport,
  JobInfo,
  ResolvedTheme,
  Settings,
  ThemeSummary,
} from '../lib/types';

export type View = 'home' | 'browser' | 'settings';
export type SortKey = 'name' | 'size' | 'packed' | 'mtime' | 'type';
export type SettingsTab =
  | 'general' | 'appearance' | 'language' | 'formats' | 'shell' | 'security' | 'mount' | 'advanced' | 'about';

/** Modal dialogs. Each carries its own props; the host renders the top-most one. */
export type DialogSpec =
  | { kind: 'extract'; paths?: string[]; nodes?: number[]; relativeTo?: number }
  /** `draft`: save that new archive (its items) instead of compressing `paths`. */
  | { kind: 'compress'; paths: string[]; draft?: number }
  | { kind: 'openPassword'; path: string; wrong: boolean }
  | { kind: 'jobPassword'; jobId: number; promptId: number; archive: string; attempt: number }
  | { kind: 'conflict'; jobId: number; promptId: number; data: Record<string, unknown> }
  | { kind: 'confirm'; jobId: number; promptId: number; data: Record<string, unknown> }
  | { kind: 'props'; node: number }
  | { kind: 'preview'; node: number }
  /** `mkdir`: create a folder named `name` in the folder `node` of a new archive instead. */
  | { kind: 'rename'; node: number; name: string; mkdir?: boolean }
  | { kind: 'mount' }
  | { kind: 'ask'; title: string; message: string; confirm: string; danger?: boolean; onConfirm: () => void; onCancel?: () => void };

export type Dialog = DialogSpec & { id: number };

export interface Toast {
  id: number;
  tone: 'success' | 'info' | 'warn' | 'error';
  text: string;
  action?: { label: string; run: () => void };
  sticky?: boolean;
}

interface State {
  boot: Bootstrap | null;
  settings: Settings | null;
  themes: ThemeSummary[];
  theme: ResolvedTheme | null;
  view: View;
  settingsTab: SettingsTab;
  archive: ArchiveSummary | null;
  cwd: number;
  history: number[];
  histIdx: number;
  sort: { key: SortKey; desc: boolean };
  query: string;
  selection: Set<number>;
  /** Uncompressed bytes of the selection (status bar). */
  selectionBytes: number;
  focus: number | null;
  anchor: number | null;
  listVersion: number;
  expanded: Set<number>;
  jobs: Record<number, JobInfo>;
  drawer: boolean;
  sidebarOpen: boolean;
  dialogs: Dialog[];
  /** Label of this window when it is a standalone dialog opened from the file manager. */
  shellLabel: string | null;
  /** Job started from that dialog; the window then shows its progress. */
  shellJob: number | null;
  toasts: Toast[];
  /** Jobs saving a new archive: when one succeeds, the saved archive replaces the draft. */
  draftSaves: number[];
}

interface Actions {
  setBoot(b: Bootstrap): void;
  setTheme(t: ResolvedTheme): void;
  setThemes(t: ThemeSummary[]): void;
  saveSettings(next: Settings): Promise<void>;
  updateSettings(f: (s: Settings) => void): Promise<void>;
  setView(v: View): void;
  setSettingsTab(t: SettingsTab): void;
  /** `force`: replace a new archive with content without asking (it has been saved). */
  openArchive(path: string, password?: string | null, force?: boolean): Promise<boolean>;
  /** Starts a new archive (optionally with files and folders) and shows it. */
  newArchive(name: string, paths?: string[]): Promise<void>;
  setArchive(a: ArchiveSummary | null): void;
  closeArchive(): void;
  savingDraft(jobId: number): void;
  navigate(node: number, push?: boolean): void;
  back(): void;
  forward(): void;
  up(parent: number | null): void;
  setSort(key: SortKey): void;
  setQuery(q: string): void;
  setSelection(sel: Set<number>, focus?: number | null, anchor?: number | null, bytes?: number): void;
  clearSelection(): void;
  refreshList(): void;
  toggleExpanded(node: number, open?: boolean): void;
  upsertJob(j: JobInfo): void;
  setJobs(j: JobInfo[]): void;
  setDrawer(open: boolean): void;
  setSidebarOpen(open: boolean): void;
  openDialog(d: DialogSpec): number;
  closeDialog(id: number): void;
  toast(t: Omit<Toast, 'id'>): number;
  dismissToast(id: number): void;
  reportError(e: unknown, title?: string): void;
}

let seq = 1;
const nextId = () => seq++;

/** Error reports are rendered by the toast host with `errorText`. */
export const errorHandler: { render: (e: ErrorReport) => string } = { render: (e) => e.message };

/** State for showing an archive (or a new one) from its root. */
const shown = (a: ArchiveSummary, listVersion: number): Partial<State> => ({
  archive: a,
  cwd: 0,
  history: [0],
  histIdx: 0,
  query: '',
  selection: new Set(),
  focus: null,
  anchor: null,
  expanded: new Set([0]),
  view: 'browser',
  listVersion: listVersion + 1,
});

/** Asks before a new archive with something in it is thrown away; resolves to whether to go on. */
function discardDraft(): Promise<boolean> {
  const s = useApp.getState();
  const a = s.archive;
  if (!a?.draft || a.entries === 0) return Promise.resolve(true);
  return new Promise((resolve) => {
    s.openDialog({
      kind: 'ask',
      title: i18n.t('draft.discardTitle'),
      message: i18n.t('draft.discardMessage', { name: a.name, count: a.entries }),
      confirm: i18n.t('draft.discard'),
      danger: true,
      onConfirm: () => resolve(true),
      onCancel: () => resolve(false),
    });
  });
}

export const useApp = create<State & Actions>()((set, get) => ({
  boot: null,
  settings: null,
  themes: [],
  theme: null,
  view: 'home',
  settingsTab: 'appearance',
  archive: null,
  cwd: 0,
  history: [],
  histIdx: -1,
  sort: { key: 'name', desc: false },
  query: '',
  selection: new Set(),
  selectionBytes: 0,
  focus: null,
  anchor: null,
  listVersion: 0,
  expanded: new Set([0]),
  jobs: {},
  drawer: false,
  sidebarOpen: false,
  dialogs: [],
  shellLabel: null,
  shellJob: null,
  toasts: [],
  draftSaves: [],

  setBoot: (b) => set({ boot: b, settings: b.settings, themes: b.themes }),
  setTheme: (t) => set({ theme: t }),
  setThemes: (t) => set({ themes: t }),

  saveSettings: async (next) => {
    set({ settings: next });
    try {
      const saved = await api.setSettings(next);
      set({ settings: saved });
    } catch (e) {
      get().reportError(e);
    }
  },
  updateSettings: async (f) => {
    const cur = get().settings;
    if (!cur) return;
    const next: Settings = structuredClone(cur);
    f(next);
    await get().saveSettings(next);
  },

  setView: (v) => set({ view: v, sidebarOpen: false }),
  setSettingsTab: (t) => set({ settingsTab: t, view: 'settings', sidebarOpen: false }),

  openArchive: async (path, password, force) => {
    if (!force && !(await discardDraft())) return false;
    try {
      const a = await api.openArchive(path, password ?? null, null);
      const prev = get().archive;
      if (prev && prev.id !== a.id) void api.closeArchive(prev.id);
      set(shown(a, get().listVersion));
      return true;
    } catch (e) {
      const err = asError(e);
      if (err.key === 'password-required' || err.key === 'wrong-password') {
        get().openDialog({ kind: 'openPassword', path, wrong: err.key === 'wrong-password' && !!password });
      } else {
        get().reportError(err);
      }
      return false;
    }
  },
  newArchive: async (name, paths = []) => {
    if (!(await discardDraft())) return;
    try {
      const a = await api.draftNew(name, paths);
      const prev = get().archive;
      if (prev) void api.closeArchive(prev.id);
      set(shown(a, get().listVersion));
    } catch (e) {
      get().reportError(e);
    }
  },
  setArchive: (a) => set({ archive: a, listVersion: get().listVersion + 1 }),
  closeArchive: () => {
    void discardDraft().then((ok) => {
      if (!ok) return;
      const a = get().archive;
      if (a) void api.closeArchive(a.id);
      set({ archive: null, view: 'home', selection: new Set(), query: '' });
    });
  },
  savingDraft: (jobId) => set({ draftSaves: [...get().draftSaves, jobId] }),

  navigate: (node, push = true) => {
    const { history, histIdx } = get();
    const h = push ? [...history.slice(0, histIdx + 1), node] : history;
    set({
      cwd: node,
      history: h,
      histIdx: push ? h.length - 1 : histIdx,
      selection: new Set(),
      focus: null,
      anchor: null,
      query: '',
      view: 'browser',
      sidebarOpen: false,
    });
  },
  back: () => {
    const { history, histIdx } = get();
    if (histIdx > 0) set({ histIdx: histIdx - 1, cwd: history[histIdx - 1], selection: new Set(), focus: null, query: '' });
  },
  forward: () => {
    const { history, histIdx } = get();
    if (histIdx < history.length - 1)
      set({ histIdx: histIdx + 1, cwd: history[histIdx + 1], selection: new Set(), focus: null, query: '' });
  },
  up: (parent) => {
    if (parent != null) get().navigate(parent);
  },
  setSort: (key) => {
    const s = get().sort;
    set({ sort: s.key === key ? { key, desc: !s.desc } : { key, desc: false } });
  },
  setQuery: (q) => set({ query: q, selection: new Set(), focus: null }),
  setSelection: (sel, focus, anchor, bytes) =>
    set((s) => ({
      selection: sel,
      selectionBytes: bytes ?? 0,
      focus: focus === undefined ? s.focus : focus,
      anchor: anchor === undefined ? s.anchor : anchor,
    })),
  clearSelection: () => set({ selection: new Set(), selectionBytes: 0, focus: null, anchor: null }),
  refreshList: () => set({ listVersion: get().listVersion + 1 }),
  toggleExpanded: (node, open) => {
    const e = new Set(get().expanded);
    const want = open ?? !e.has(node);
    if (want) e.add(node);
    else e.delete(node);
    set({ expanded: e });
  },

  upsertJob: (j) => set((s) => ({ jobs: { ...s.jobs, [j.id]: j } })),
  setJobs: (list) => set({ jobs: Object.fromEntries(list.map((j) => [j.id, j])) }),
  setDrawer: (open) => set({ drawer: open }),
  setSidebarOpen: (open) => set({ sidebarOpen: open }),

  openDialog: (d) => {
    const id = nextId();
    set((s) => ({ dialogs: [...s.dialogs, { ...d, id } as Dialog] }));
    return id;
  },
  closeDialog: (id) => set((s) => ({ dialogs: s.dialogs.filter((d) => d.id !== id) })),

  toast: (t) => {
    const id = nextId();
    set((s) => ({ toasts: [...s.toasts.slice(-3), { ...t, id }] }));
    if (!t.sticky) setTimeout(() => get().dismissToast(id), t.action ? 6500 : 3500);
    return id;
  },
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((x) => x.id !== id) })),
  reportError: (e, title) => {
    const err = asError(e);
    const msg = errorHandler.render(err);
    get().toast({ tone: err.key === 'cancelled' ? 'info' : 'error', text: title ? `${title}: ${msg}` : msg, sticky: err.key !== 'cancelled' });
  },
}));
