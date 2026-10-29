// Main window (docs/04-ui-design.md §3).

import { getCurrentWebview } from '@tauri-apps/api/webview';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Toasts } from './components/ui';
import { Browser } from './features/browser/Browser';
import { DialogHost } from './features/dialogs/Dialogs';
import { Home, pickArchive } from './features/home/Home';
import { SettingsView } from './features/settings/Settings';
import { ResizeEdges, Sidebar, StatusBar, TitleBar } from './features/shell/Chrome';
import { TaskDrawer, useJobEvents } from './features/tasks/Tasks';
import i18n, { resolveLang, setLanguage } from './i18n';
import { api, events } from './lib/api';
import { applyTheme, effectiveDark, onSystemThemeChange } from './lib/theme';
import type { LaunchRequest } from './lib/types';
import { useApp } from './store/app';

function handleLaunch(r: LaunchRequest) {
  const s = useApp.getState();
  switch (r.kind) {
    case 'open':
      void s.openArchive(r.path);
      break;
    case 'extract':
      s.openDialog({ kind: 'extract', paths: r.paths });
      break;
    case 'compress':
      s.openDialog({ kind: 'compress', paths: r.paths });
      break;
    case 'mount':
      void s.openArchive(r.path).then((ok) => ok && s.openDialog({ kind: 'mount' }));
      break;
    case 'convert':
      s.openDialog({ kind: 'convert', paths: r.paths });
      break;
  }
}

/** Files dropped from the file manager (docs/04-ui-design.md §5.2). */
async function handleDrop(paths: string[]) {
  const s = useApp.getState();
  if (!paths.length) return;
  if (s.view === 'browser' && s.archive) {
    if (s.archive.writable) {
      await api.add(s.archive.id, paths, s.cwd).catch((e) => s.reportError(e));
      return;
    }
  }
  const { archives, others } = await api.classify(paths);
  if (others.length === 0 && archives.length === 1) await s.openArchive(archives[0]);
  else if (others.length === 0) s.openDialog({ kind: 'extract', paths: archives });
  // Other files start a new archive with them in it (docs/04-ui-design.md §1).
  else await s.newArchive(i18n.t('draft.name'), paths);
}

export function App() {
  const { t } = useTranslation();
  const view = useApp((s) => s.view);
  const archive = useApp((s) => s.archive);
  const settings = useApp((s) => s.settings)!;
  const boot = useApp((s) => s.boot)!;
  const sidebarOpen = useApp((s) => s.sidebarOpen);
  const setSidebarOpen = useApp((s) => s.setSidebarOpen);
  const [dragOver, setDragOver] = useState(false);
  useJobEvents();

  // Theme follows settings and the OS.
  const appearanceKey = JSON.stringify(settings.appearance);
  const first = useRef(true);
  useEffect(() => {
    const run = (animate: boolean) => void applyTheme(settings, boot.system, animate).then((th) => useApp.getState().setTheme(th));
    run(!first.current);
    first.current = false;
    return onSystemThemeChange(() => run(true));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [appearanceKey, boot.system]);

  // Language follows settings.
  useEffect(() => {
    void setLanguage(resolveLang(settings.language.ui, boot.system.locale));
  }, [settings.language.ui, boot.system.locale]);

  // Launch requests: at start-up and forwarded from later invocations.
  useEffect(() => {
    if (boot.launch) handleLaunch(boot.launch);
    else if (settings.general.startup === 'last') void api.recent().then((r) => r[0] && useApp.getState().openArchive(r[0].path));
    const un = events.onLaunch(handleLaunch);
    // A request that came after start-up but before the listener (macOS hands files opened from
    // Finder over as an event) waits until asked for.
    void un.then(() => api.takeLaunch()).then((r) => r && handleLaunch(r));
    return () => void un.then((f) => f());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Mounted archives: restored at start-up, changed from other windows, or taken down by the system.
  useEffect(() => {
    const refresh = () => void useApp.getState().refreshMounts();
    refresh();
    const un = events.onMounts(refresh);
    return () => void un.then((f) => f());
  }, []);

  // Drag and drop from the file manager.
  useEffect(() => {
    const un = getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === 'enter' || p.type === 'over') setDragOver(true);
      else if (p.type === 'leave') setDragOver(false);
      else if (p.type === 'drop') {
        setDragOver(false);
        void handleDrop(p.paths);
      }
    });
    return () => void un.then((f) => f());
  }, []);

  // Global shortcuts (docs/04-ui-design.md §5.1).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const s = useApp.getState();
      if (s.dialogs.length) return;
      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;
      const k = e.key.toLowerCase();
      if (k === 'o') {
        e.preventDefault();
        void pickArchive(t).then((p) => {
          if (p) void s.openArchive(p);
        });
      } else if (k === ',') {
        e.preventDefault();
        s.setView('settings');
      } else if (e.shiftKey && k === 'd') {
        e.preventDefault();
        void s.updateSettings((x) => (x.appearance.mode = effectiveDark(x) ? 'light' : 'dark'));
      } else if (k === 'w' && s.archive) {
        e.preventDefault();
        s.closeArchive();
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [t]);

  const collapsed = settings.appearance.sidebar_collapsed;
  return (
    <div className={`window${collapsed ? ' collapsed' : ''}${sidebarOpen ? ' sb-open' : ''}`}>
      <Sidebar />
      <div className="maincol">
        <TitleBar />
        <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0, position: 'relative' }}>
          {view === 'settings' ? <SettingsView /> : view === 'browser' && archive ? <Browser key={archive.id} /> : <Home dragOver={dragOver} />}
          {dragOver && view === 'browser' && archive && (
            <div className="drop-overlay">{archive.writable ? t('cmd.add') : t('toast.readonly', { format: archive.info.format.toUpperCase() })}</div>
          )}
        </div>
        <StatusBar />
        <TaskDrawer />
      </div>
      {sidebarOpen && <div className="sb-scrim" onClick={() => setSidebarOpen(false)} />}
      <DialogHost />
      <Toasts />
      <ResizeEdges />
    </div>
  );
}
