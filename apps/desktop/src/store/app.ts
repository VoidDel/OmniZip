// Application state (zustand). Data that can be large (directory listings) stays in Rust and is
// fetched page by page; the store only keeps what the UI needs to render the current view.

import { create } from 'zustand';

import i18n from '../i18n';
import { api, asError } from '../lib/api';
import type {
  ArchiveSummary,
  Bootstrap,
  Clip,
  ErrorReport,
  JobInfo,
  MountInfo,
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
  | { kind: 'convert'; paths: string[] }
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

/** Where the browser is in an archive: kept for each open archive while another is shown. */
interface ArchiveView {
  cwd: number;
  cwdPath: string;
  history: number[];
  histIdx: number;
  query: string;
  selection: Set<number>;
  selectionBytes: number;
  focus: number | null;
  anchor: number | null;
  expanded: Set<string>;
}

interface State {
  boot: Bootstrap | null;
  settings: Settings | null;
  themes: ThemeSummary[];
  theme: ResolvedTheme | null;
  view: View;
  settingsTab: SettingsTab;
  /** The archive shown in the browser; the fields below up to `expanded` are its view. */
  archive: ArchiveSummary | null;
  /** Every open archive, in the order opened (the sidebar lists them). */
  opened: ArchiveSummary[];
  /** Saved views of the open archives that are not shown, by archive id. */
  views: Record<number, ArchiveView>;
  cwd: number;
  /** Path of `cwd`: node ids change when an archive is rewritten, the path finds it again. */
  cwdPath: string;
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
  /** Folders unfolded in the sidebar by path ('' is the archive itself): unlike node ids, paths survive a rewrite. */
  expanded: Set<string>;
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
  draftSaves: { job: number; draft: number }[];
  /** Items copied or cut from an open archive (cut ones are dimmed until pasted). */
  clip: Clip | null;
  /** Mounted archives (sidebar, settings). */
  mounts: MountInfo[];
}

interface Actions {
  setBoot(b: Bootstrap): void;
  setTheme(t: ResolvedTheme): void;
  setThemes(t: ThemeSummary[]): void;
  saveSettings(next: Settings): Promise<void>;
  updateSettings(f: (s: Settings) => void): Promise<void>;
  setView(v: View): void;
  setSettingsTab(t: SettingsTab): void;
  /** Opens an archive next to the open ones and shows it (or shows it if it is open). */
  openArchive(path: string, password?: string | null): Promise<boolean>;
  /** Starts a new archive (optionally with files and folders) and shows it. */
  newArchive(name: string, paths?: string[]): Promise<void>;
  /** Shows another open archive where it was left. */
  switchArchive(id: number): void;
  /** New summary of an open archive (same node ids). */
  setArchive(a: ArchiveSummary): void;
  /** An open archive was rewritten (new node ids): its view goes to the folder `node`, nothing selected. */
  relocate(a: ArchiveSummary, node: number): void;
  /** Closes an open archive (the shown one by default); a new archive with content asks first unless `force`. */
  closeArchive(id?: number, force?: boolean): void;
  savingDraft(jobId: number, draftId: number): void;
  navigate(node: number, push?: boolean): void;
  back(): void;
  forward(): void;
  up(parent: number | null): void;
  setSort(key: SortKey): void;
  setQuery(q: string): void;
  setSelection(sel: Set<number>, focus?: number | null, anchor?: number | null, bytes?: number): void;
  clearSelection(): void;
  refreshList(): void;
  /** Unfolds or folds a folder (by path) of an open archive in the sidebar. */
  toggleExpanded(archive: number, path: string, open?: boolean): void;
  upsertJob(j: JobInfo): void;
  setJobs(j: JobInfo[]): void;
  setDrawer(open: boolean): void;
  setSidebarOpen(open: boolean): void;
  openDialog(d: DialogSpec): number;
  closeDialog(id: number): void;
  toast(t: Omit<Toast, 'id'>): number;
  dismissToast(id: number): void;
  reportError(e: unknown, title?: string): void;
  refreshMounts(): Promise<void>;
  unmount(id: number): Promise<void>;
}

let seq = 1;
const nextId = () => seq++;

/** Error reports are rendered by the toast host with `errorText`. */
export const errorHandler: { render: (e: ErrorReport) => string } = { render: (e) => e.message };

/** The view of an archive just opened: its root. */
const freshView = (node = 0): ArchiveView => ({
  cwd: node,
  cwdPath: '',
  history: [node],
  histIdx: 0,
  query: '',
  selection: new Set(),
  selectionBytes: 0,
  focus: null,
  anchor: null,
  expanded: new Set(['']),
});

/** The view of the archive being shown, to keep while another one is. */
const viewOf = (s: State): ArchiveView => ({
  cwd: s.cwd,
  cwdPath: s.cwdPath,
  history: s.history,
  histIdx: s.histIdx,
  query: s.query,
  selection: s.selection,
  selectionBytes: s.selectionBytes,
  focus: s.focus,
  anchor: s.anchor,
  expanded: s.expanded,
});

/** State for showing `a` with `view`, the view of the archive shown so far being kept. */
function show(s: State, a: ArchiveSummary, view: ArchiveView): Partial<State> {
  const views = { ...s.views };
  if (s.archive && s.archive.id !== a.id) views[s.archive.id] = viewOf(s);
  delete views[a.id];
  const opened = s.opened.some((x) => x.id === a.id) ? s.opened.map((x) => (x.id === a.id ? a : x)) : [...s.opened, a];
  return { ...view, archive: a, opened, views, view: 'browser', listVersion: s.listVersion + 1 };
}

const samePath = (a: string, b: string) => a.replace(/\\/g, '/').toLowerCase() === b.replace(/\\/g, '/').toLowerCase();

/** Asks before a new archive with something in it is thrown away; resolves to whether to go on. */
function discardDraft(a: ArchiveSummary | undefined): Promise<boolean> {
  const s = useApp.getState();
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
  opened: [],
  views: {},
  cwd: 0,
  cwdPath: '',
  history: [],
  histIdx: -1,
  sort: { key: 'name', desc: false },
  query: '',
  selection: new Set(),
  selectionBytes: 0,
  focus: null,
  anchor: null,
  listVersion: 0,
  expanded: new Set(['']),
  jobs: {},
  drawer: false,
  sidebarOpen: false,
  dialogs: [],
  shellLabel: null,
  shellJob: null,
  toasts: [],
  draftSaves: [],
  clip: null,
  mounts: [],

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

  openArchive: async (path, password) => {
    const open = get().opened.find((x) => !x.draft && samePath(x.path, path));
    if (open) {
      get().switchArchive(open.id);
      return true;
    }
    try {
      const a = await api.openArchive(path, password ?? null, null);
      set(show(get(), a, freshView()));
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
    try {
      const a = await api.draftNew(name, paths);
      set(show(get(), a, freshView()));
    } catch (e) {
      get().reportError(e);
    }
  },
  switchArchive: (id) => {
    const s = get();
    const a = s.opened.find((x) => x.id === id);
    if (!a) return;
    if (s.archive?.id === id) return set({ view: 'browser', sidebarOpen: false });
    // The archive shown unfolds in the sidebar.
    const view = s.views[id] ?? freshView();
    set({ ...show(s, a, { ...view, expanded: new Set([...view.expanded, '']) }), sidebarOpen: false });
  },
  setArchive: (a) =>
    set((s) => ({
      archive: s.archive?.id === a.id ? a : s.archive,
      opened: s.opened.map((x) => (x.id === a.id ? a : x)),
      listVersion: s.listVersion + 1,
    })),
  relocate: (a, node) =>
    set((s) => {
      const opened = s.opened.map((x) => (x.id === a.id ? a : x));
      // Older history entries would point at other folders now.
      if (s.archive?.id !== a.id) {
        const keep = s.views[a.id];
        return { opened, views: { ...s.views, [a.id]: { ...freshView(node), cwdPath: keep?.cwdPath ?? '', expanded: keep?.expanded ?? new Set(['']) } } };
      }
      return { ...freshView(node), cwdPath: s.cwdPath, expanded: s.expanded, archive: a, opened, listVersion: s.listVersion + 1 };
    }),
  closeArchive: (id, force) => {
    const target = get().opened.find((x) => x.id === (id ?? get().archive?.id));
    if (!target) return;
    void (force ? Promise.resolve(true) : discardDraft(target)).then((ok) => {
      if (!ok) return;
      void api.closeArchive(target.id);
      const s = get();
      const at = s.opened.findIndex((x) => x.id === target.id);
      const opened = s.opened.filter((x) => x.id !== target.id);
      const views = { ...s.views };
      delete views[target.id];
      if (s.archive?.id !== target.id) return set({ opened, views });
      // The one shown: show its neighbour where it was left, or the start page.
      const next = opened[Math.max(0, at - 1)];
      if (!next) return set({ archive: null, opened, views, view: 'home', ...freshView() });
      const view = views[next.id] ?? freshView();
      delete views[next.id];
      set({ ...view, archive: next, opened, views, listVersion: s.listVersion + 1 });
    });
  },
  savingDraft: (job, draft) => set({ draftSaves: [...get().draftSaves, { job, draft }] }),

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
  toggleExpanded: (archive, path, open) =>
    set((s) => {
      const toggled = (before: Set<string>) => {
        const e = new Set(before);
        if (open ?? !e.has(path)) e.add(path);
        else e.delete(path);
        return e;
      };
      if (s.archive?.id === archive) return { expanded: toggled(s.expanded) };
      const v = s.views[archive];
      return v ? { views: { ...s.views, [archive]: { ...v, expanded: toggled(v.expanded) } } } : {};
    }),

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
  refreshMounts: async () => {
    try {
      set({ mounts: await api.mounts() });
    } catch {
      // Not available in this window.
    }
  },
  unmount: async (id) => {
    const m = get().mounts.find((x) => x.id === id);
    try {
      await api.unmount(id);
      if (m) get().toast({ tone: 'success', text: i18n.t('mount.ejected', { name: m.label }) });
    } catch (e) {
      get().reportError(e);
    }
    await get().refreshMounts();
  },
}));

/** Where a mount is opened: the root of a drive letter, or the folder. */
export const mountRoot = (m: MountInfo) => (/^[A-Za-z]:$/.test(m.mountPoint) ? `${m.mountPoint}\\` : m.mountPoint);
