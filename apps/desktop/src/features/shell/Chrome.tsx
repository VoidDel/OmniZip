// Title bar, sidebar and status bar (docs/04-ui-design.md §3).

import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { FileIcon, Icon, Logo, formatBadgeStyle, type IconName } from '../../components/Icon';
import { ContextMenu, ProgressBar, type MenuItem } from '../../components/ui';
import { normalizeLang } from '../../i18n';
import { api } from '../../lib/api';
import { useFmt } from '../../lib/hooks';
import { effectiveDark } from '../../lib/theme';
import type { ArchiveSummary, TreeItem } from '../../lib/types';
import { useApp } from '../../store/app';
import { SETTINGS_TABS } from '../settings/Settings';

export function WindowControls() {
  const { t } = useTranslation();
  const [max, setMax] = useState(false);
  useEffect(() => {
    const w = getCurrentWindow();
    void w.isMaximized().then(setMax);
    const un = w.onResized(() => void w.isMaximized().then(setMax));
    return () => {
      void un.then((f) => f());
    };
  }, []);
  const w = getCurrentWindow();
  return (
    <div className="winctl">
      <button type="button" aria-label={t('title.minimize')} onClick={() => void w.minimize()}>
        <Icon name="minus" />
      </button>
      <button type="button" aria-label={t('title.maximize')} onClick={() => void w.toggleMaximize()}>
        <Icon name={max ? 'restore' : 'square'} />
      </button>
      <button type="button" className="close" aria-label={t('title.close')} onClick={() => void w.close()}>
        <Icon name="x" />
      </button>
    </div>
  );
}

/** Title-bar button that opens a menu below itself; pressing it again closes the menu. */
function MenuButton({ icon, label, items }: { icon: IconName; label: string; items: () => MenuItem[] }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  return (
    <>
      <button
        ref={ref}
        type="button"
        className="iconbtn"
        title={label}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={at !== null}
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          setAt(at ? null : { x: r.left, y: r.bottom + 4 });
        }}
      >
        <Icon name={icon} />
      </button>
      {at && <ContextMenu x={at.x} y={at.y} items={items()} anchor={ref.current} onClose={() => setAt(null)} />}
    </>
  );
}

export function TitleBar() {
  const { t, i18n } = useTranslation();
  const view = useApp((s) => s.view);
  const archive = useApp((s) => s.archive);
  const query = useApp((s) => s.query);
  const setQuery = useApp((s) => s.setQuery);
  const settings = useApp((s) => s.settings);
  const boot = useApp((s) => s.boot);
  const themes = useApp((s) => s.themes);
  const updateSettings = useApp((s) => s.updateSettings);
  const setSidebarOpen = useApp((s) => s.setSidebarOpen);
  const sidebarOpen = useApp((s) => s.sidebarOpen);
  const dark = settings ? effectiveDark(settings) : false;
  // Light / dark / system, then the themes of the appearance in effect.
  const themeItems = (): MenuItem[] => {
    if (!settings) return [];
    const a = settings.appearance;
    const mode = (m: typeof a.mode) => () => void updateSettings((s) => (s.appearance.mode = m));
    const key = dark ? 'theme_dark' : 'theme_light';
    return [
      { label: t('settings.appearance.light'), icon: 'sun', checked: a.mode === 'light', run: mode('light') },
      { label: t('settings.appearance.dark'), icon: 'moon', checked: a.mode === 'dark', run: mode('dark') },
      { label: t('settings.appearance.system'), icon: 'monitor', checked: a.mode === 'system', run: mode('system') },
      { separator: true },
      ...themes
        .filter((th) => th.appearance === (dark ? 'dark' : 'light'))
        .map((th): MenuItem => ({
          label: th.name[i18n.language] ?? th.name['en-US'],
          checked: a[key] === th.id,
          run: () => void updateSettings((s) => (s.appearance[key] = th.id)),
        })),
    ];
  };
  const langItems = (): MenuItem[] => {
    if (!settings || !boot) return [];
    const ui = settings.language.ui;
    const pick = (lang: string) => () => void updateSettings((s) => (s.language.ui = lang));
    const sysLang = boot.languages.find(([l]) => l === normalizeLang(boot.system.locale))?.[1] ?? boot.system.locale;
    return [
      { label: t('settings.language.auto', { lang: sysLang }), checked: ui === 'auto', run: pick('auto') },
      { separator: true },
      ...boot.languages.map(([l, name]): MenuItem => ({ label: name, checked: ui === l, run: pick(l) })),
    ];
  };
  const title =
    view === 'settings' ? (
      <b>{t('settings.title')}</b>
    ) : view === 'browser' && archive ? (
      <>
        <FileIcon name={archive.name} isDir={false} />
        <b>{archive.name}</b>
      </>
    ) : (
      <b>{t('nav.home')}</b>
    );
  return (
    <header className="titlebar" data-tauri-drag-region>
      <div className="tb-side" data-tauri-drag-region>
        <button type="button" className="iconbtn menu-toggle" aria-label={t('title.menu')} onClick={() => setSidebarOpen(!sidebarOpen)}>
          <Icon name="menu" />
        </button>
        <div className="ttl" data-tauri-drag-region>
          {title}
        </div>
      </div>
      {view === 'browser' && (
        <label className="searchbox">
          <Icon name="search" />
          <input
            id="search"
            type="search"
            value={query}
            placeholder={t('title.search')}
            aria-label={t('title.search')}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') setQuery('');
              if (e.key === 'Enter' || e.key === 'ArrowDown') document.getElementById('filelist')?.focus();
            }}
          />
        </label>
      )}
      <div className="tb-side end" data-tauri-drag-region>
        <MenuButton icon={dark ? 'moon' : 'sun'} label={t('title.theme')} items={themeItems} />
        <MenuButton icon="globe" label={t('title.language')} items={langItems} />
        <WindowControls />
      </div>
    </header>
  );
}

/** Whether the folder at `path` of archive `id` is unfolded in the sidebar. */
const unfolded = (s: ReturnType<typeof useApp.getState>, id: number, path: string) =>
  (s.archive?.id === id ? s.expanded : s.views[id]?.expanded)?.has(path) ?? false;

/**
 * A folder of an open archive, `path` locating it ('/' between names). Clicking it shows it (and
 * its archive); items dragged onto it go there when the archive can change.
 */
function TreeNode({ a, item, path, depth }: { a: ArchiveSummary; item: TreeItem; path: string; depth: number }) {
  const expanded = useApp((s) => unfolded(s, a.id, path));
  const current = useApp((s) => s.archive?.id === a.id && s.cwd === item.node && s.view === 'browser' && !s.query);
  const toggle = useApp((s) => s.toggleExpanded);
  const version = useApp((s) => s.listVersion);
  const [children, setChildren] = useState<TreeItem[] | null>(null);
  useEffect(() => {
    if (expanded && item.hasChildren) void api.dirs(a.id, item.node).then(setChildren);
  }, [expanded, a, item.node, item.hasChildren, version]);
  const open = () => {
    const s = useApp.getState();
    if (s.archive?.id !== a.id) s.switchArchive(a.id);
    s.navigate(item.node);
  };
  return (
    <>
      <button
        type="button"
        className="tree-item"
        data-drop-archive={a.id}
        data-drop-dir={a.writable ? item.node : undefined}
        role="treeitem"
        aria-expanded={item.hasChildren ? expanded : undefined}
        aria-current={current}
        style={{ paddingLeft: 12 + depth * 14 }}
        title={item.name}
        onClick={open}
      >
        <span
          className="tw"
          onClick={(e) => {
            e.stopPropagation();
            toggle(a.id, path);
          }}
        >
          {item.hasChildren && <Icon name={expanded ? 'chevronDown' : 'chevronRight'} />}
        </span>
        <Icon name={item.nestedArchive ? 'archive' : expanded ? 'folderOpen' : 'folder'} className={`fi ${item.nestedArchive ? 'archive' : 'folder'}`} />
        <span className="lbl">{item.name}</span>
      </button>
      {expanded && children?.map((c) => <TreeNode key={c.node} a={a} item={c} path={`${path}/${c.name}`} depth={depth + 1} />)}
    </>
  );
}

/**
 * An open archive in the sidebar and, while it is unfolded, its folders. Every archive folds on
 * its own; the one shown is unfolded when it is opened or switched to.
 */
function ArchiveTree({ a }: { a: ArchiveSummary }) {
  const { t } = useTranslation();
  const shown = useApp((s) => s.archive?.id === a.id);
  const expanded = useApp((s) => unfolded(s, a.id, ''));
  const current = useApp((s) => s.archive?.id === a.id && s.cwd === 0 && s.view === 'browser');
  const toggle = useApp((s) => s.toggleExpanded);
  const version = useApp((s) => s.listVersion);
  const [roots, setRoots] = useState<TreeItem[]>([]);
  useEffect(() => {
    void api.dirs(a.id, 0).then(setRoots);
  }, [a, version]);
  const open = () => {
    const s = useApp.getState();
    if (shown) s.navigate(0);
    else s.switchArchive(a.id);
  };
  return (
    <>
      {/* A row rather than a button, so that it can hold the close button. Items dragged onto
          an archive that can change go into its root folder. */}
      <div
        className={`tree-item tree-root${shown ? ' shown' : ''}`}
        data-drop-archive={a.id}
        data-drop-dir={a.writable ? 0 : undefined}
        role="treeitem"
        tabIndex={0}
        aria-expanded={roots.length ? expanded : undefined}
        aria-current={current}
        onClick={open}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            open();
          } else if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
            e.preventDefault();
            toggle(a.id, '', e.key === 'ArrowRight');
          }
        }}
        title={a.draft ? a.name : a.path}
      >
        <span
          className="tw"
          onClick={(e) => {
            e.stopPropagation();
            toggle(a.id, '');
          }}
        >
          {roots.length > 0 && <Icon name={expanded ? 'chevronDown' : 'chevronRight'} />}
        </span>
        <Icon name="archive" className="fi archive" />
        <span className="lbl">{a.name}</span>
        {/* A new archive has no format until it is saved. */}
        {!a.draft && (
          <span className="badge" style={formatBadgeStyle(a.info.format, false)}>
            {a.info.format.toUpperCase()}
          </span>
        )}
        <button
          type="button"
          className="tree-close"
          title={t('cmd.closeArchive')}
          aria-label={t('cmd.closeArchive')}
          onClick={(e) => {
            e.stopPropagation();
            useApp.getState().closeArchive(a.id);
          }}
        >
          <Icon name="x" />
        </button>
      </div>
      {expanded && roots.map((r) => <TreeNode key={r.node} a={a} item={r} path={r.name} depth={1} />)}
    </>
  );
}

export function Sidebar() {
  const { t } = useTranslation();
  const fmt = useFmt();
  const view = useApp((s) => s.view);
  const archive = useApp((s) => s.archive);
  const opened = useApp((s) => s.opened);
  const setView = useApp((s) => s.setView);
  const settingsTab = useApp((s) => s.settingsTab);
  const setSettingsTab = useApp((s) => s.setSettingsTab);
  const settings = useApp((s) => s.settings);
  const updateSettings = useApp((s) => s.updateSettings);
  const jobs = useApp((s) => s.jobs);
  const setDrawer = useApp((s) => s.setDrawer);
  const appVersion = useApp((s) => s.boot?.system.appVersion);
  const collapsed = settings?.appearance.sidebar_collapsed ?? false;

  const list = Object.values(jobs);
  const running = list.filter((j) => j.state === 'running' || j.state === 'paused');
  const total = running.reduce((s, j) => s + j.progress.bytesTotal, 0);
  const done = running.reduce((s, j) => s + j.progress.bytesDone, 0);
  const label = running.length ? t('task.running', { count: running.length }) : t('task.summary', { count: list.length });
  const inSettings = view === 'settings';
  const toggleLabel = t(collapsed ? 'nav.expand' : 'nav.collapse');
  return (
    <aside className="sidebar">
      <div className="sb-top" data-tauri-drag-region>
        <Logo />
        <span className="brandname grow" data-tauri-drag-region>
          OmniZip
          {appVersion && (
            <span className="ver" data-tauri-drag-region>
              {appVersion}
            </span>
          )}
        </span>
        <button
          type="button"
          className="iconbtn sm"
          title={toggleLabel}
          aria-label={toggleLabel}
          onClick={() => void updateSettings((s) => (s.appearance.sidebar_collapsed = !collapsed))}
        >
          <Icon name="sidebar" />
        </button>
      </div>
      <nav className="sb-nav">
        <button type="button" className="nav-item" aria-current={view === 'home'} title={t('nav.home')} onClick={() => setView('home')}>
          <Icon name="home" />
          <span className="lbl">{t('nav.home')}</span>
        </button>
      </nav>
      {opened.length > 0 && (
        <>
          <hr className="sb-sep" />
          <div className="tree" role="tree" aria-label={t('nav.tree')}>
            {opened.map((a) => (
              <ArchiveTree key={a.id} a={a} />
            ))}
          </div>
        </>
      )}
      <div className="sb-bottom">
        {list.length > 0 && (
          <button type="button" className="task-sum" title={label} onClick={() => setDrawer(true)}>
            <div className="row">
              <Icon name="history" />
              <span className="lbl grow">{label}</span>
              {running.length > 0 && <span className="lbl mono" style={{ fontSize: '.78rem', color: 'var(--fg-secondary)' }}>{fmt.percent(done, total)}</span>}
            </div>
            {running.length > 0 && <ProgressBar value={total ? done / total : 0} indeterminate={!total} />}
          </button>
        )}
        {/* The settings sections unfold upwards from the Settings item while settings are shown. */}
        <div id="sb-settings" className="sb-sub" data-open={inSettings} role="group" aria-label={t('nav.settings')} inert={!inSettings}>
          <div>
            {SETTINGS_TABS.map(([id, icon]) => {
              const text = t(`settings.tabs.${id}`);
              return (
                <button key={id} type="button" className="nav-item" aria-current={inSettings && settingsTab === id} title={text} onClick={() => setSettingsTab(id)}>
                  <Icon name={icon} />
                  <span className="lbl">{text}</span>
                </button>
              );
            })}
          </div>
        </div>
        <button
          type="button"
          className="nav-item"
          aria-expanded={inSettings}
          aria-controls="sb-settings"
          title={t('nav.settings')}
          onClick={() => setView(inSettings ? (archive ? 'browser' : 'home') : 'settings')}
        >
          <Icon name="gear" />
          <span className="lbl grow">{t('nav.settings')}</span>
          <Icon name="chevronUp" className="chev" />
        </button>
      </div>
    </aside>
  );
}

export function StatusBar() {
  const { t } = useTranslation();
  const fmt = useFmt();
  const view = useApp((s) => s.view);
  const archive = useApp((s) => s.archive);
  const selection = useApp((s) => s.selection);
  const selSize = useApp((s) => s.selectionBytes);
  const settings = useApp((s) => s.settings);
  const dark = settings ? effectiveDark(settings) : false;
  // Only an open archive has something to report; the app version is shown next to the logo.
  if (view !== 'browser' || !archive) return null;
  const i = archive.info;
  if (archive.draft) {
    return (
      <footer className="statusbar">
        <span className="lft">{t('draft.unsaved')}</span>
        <span className="mid">
          {t('status.items', { count: archive.entries })} · {fmt.size(archive.totalSize)}
          {selection.size > 0 && ` · ${t('status.selected', { count: selection.size, size: fmt.size(selSize) })}`}
        </span>
      </footer>
    );
  }
  return (
    <footer className="statusbar">
      <span className="lft">
        <span className="badge" style={formatBadgeStyle(i.format, dark)}>{i.format.toUpperCase()}</span>
        <span>{i.methods.join(' · ')}</span>
        {i.solid && (
          <>
            <span className="dot" />
            <span>{t('status.solid')}</span>
          </>
        )}
        {i.encrypted && (
          <>
            <span className="dot" />
            <span>{i.encryption ?? 'AES'}</span>
          </>
        )}
        {!archive.writable && (
          <>
            <span className="dot" />
            <span>{t('status.readonly')}</span>
          </>
        )}
        {i.encoding && i.encoding !== 'utf-8' && (
          <>
            <span className="dot" />
            <span>{t('status.encoding', { encoding: i.encoding })}</span>
          </>
        )}
      </span>
      <span className="mid">
        {t('status.items', { count: archive.files })}
        {selection.size > 0 && ` · ${t('status.selected', { count: selection.size, size: fmt.size(selSize) })}`}
      </span>
      <span className="rgt">{t('status.ratio', { value: fmt.percent(archive.packedSize, archive.totalSize) })}</span>
    </footer>
  );
}
