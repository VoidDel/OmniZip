// Title bar, sidebar and status bar (docs/04-ui-design.md §3).

import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { FileIcon, Icon, Logo, formatBadgeStyle } from '../../components/Icon';
import { ContextMenu, ProgressBar, type MenuItem } from '../../components/ui';
import { normalizeLang } from '../../i18n';
import { api } from '../../lib/api';
import { useFmt } from '../../lib/hooks';
import { effectiveDark } from '../../lib/theme';
import type { TreeItem } from '../../lib/types';
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

export function TitleBar({ onToggleTheme }: { onToggleTheme: () => void }) {
  const { t } = useTranslation();
  const view = useApp((s) => s.view);
  const archive = useApp((s) => s.archive);
  const query = useApp((s) => s.query);
  const setQuery = useApp((s) => s.setQuery);
  const settings = useApp((s) => s.settings);
  const boot = useApp((s) => s.boot);
  const updateSettings = useApp((s) => s.updateSettings);
  const setSidebarOpen = useApp((s) => s.setSidebarOpen);
  const sidebarOpen = useApp((s) => s.sidebarOpen);
  const dark = settings ? effectiveDark(settings) : false;
  const langButton = useRef<HTMLButtonElement>(null);
  const [langMenu, setLangMenu] = useState<{ x: number; y: number } | null>(null);
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
      <button type="button" className="iconbtn menu-toggle" aria-label={t('title.menu')} onClick={() => setSidebarOpen(!sidebarOpen)}>
        <Icon name="menu" />
      </button>
      <div className="ttl" data-tauri-drag-region>
        {title}
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
      <button type="button" className="iconbtn" title={t('title.toggleTheme')} aria-label={t('title.toggleTheme')} onClick={onToggleTheme}>
        <Icon name={dark ? 'sun' : 'moon'} />
      </button>
      <button
        ref={langButton}
        type="button"
        className="iconbtn"
        title={t('title.language')}
        aria-label={t('title.language')}
        aria-haspopup="menu"
        aria-expanded={langMenu !== null}
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          setLangMenu(langMenu ? null : { x: r.left, y: r.bottom + 4 });
        }}
      >
        <Icon name="globe" />
      </button>
      {langMenu && <ContextMenu x={langMenu.x} y={langMenu.y} items={langItems()} anchor={langButton.current} onClose={() => setLangMenu(null)} />}
      <WindowControls />
    </header>
  );
}

function TreeNode({ id, item, depth }: { id: number; item: TreeItem; depth: number }) {
  const cwd = useApp((s) => s.cwd);
  const view = useApp((s) => s.view);
  const query = useApp((s) => s.query);
  const expanded = useApp((s) => s.expanded.has(item.node));
  const toggle = useApp((s) => s.toggleExpanded);
  const navigate = useApp((s) => s.navigate);
  const version = useApp((s) => s.listVersion);
  const [children, setChildren] = useState<TreeItem[] | null>(null);
  useEffect(() => {
    if (expanded && item.hasChildren) void api.dirs(id, item.node).then(setChildren);
  }, [expanded, id, item.node, item.hasChildren, version]);
  const current = cwd === item.node && view === 'browser' && !query;
  return (
    <>
      <button
        type="button"
        className="tree-item"
        role="treeitem"
        aria-expanded={item.hasChildren ? expanded : undefined}
        aria-current={current}
        style={{ paddingLeft: 6 + depth * 14 }}
        title={item.name}
        onClick={() => navigate(item.node)}
      >
        <span
          className="tw"
          onClick={(e) => {
            e.stopPropagation();
            toggle(item.node);
          }}
        >
          {item.hasChildren && <Icon name={expanded ? 'chevronDown' : 'chevronRight'} />}
        </span>
        <Icon name={item.nestedArchive ? 'archive' : expanded ? 'folderOpen' : 'folder'} className={`fi ${item.nestedArchive ? 'archive' : 'folder'}`} />
        <span className="lbl">{item.name}</span>
      </button>
      {expanded && children?.map((c) => <TreeNode key={c.node} id={id} item={c} depth={depth + 1} />)}
    </>
  );
}

export function Sidebar() {
  const { t } = useTranslation();
  const fmt = useFmt();
  const view = useApp((s) => s.view);
  const archive = useApp((s) => s.archive);
  const setView = useApp((s) => s.setView);
  const settingsTab = useApp((s) => s.settingsTab);
  const setSettingsTab = useApp((s) => s.setSettingsTab);
  const settings = useApp((s) => s.settings);
  const updateSettings = useApp((s) => s.updateSettings);
  const jobs = useApp((s) => s.jobs);
  const setDrawer = useApp((s) => s.setDrawer);
  const navigate = useApp((s) => s.navigate);
  const cwd = useApp((s) => s.cwd);
  const version = useApp((s) => s.listVersion);
  const collapsed = settings?.appearance.sidebar_collapsed ?? false;
  const [roots, setRoots] = useState<TreeItem[]>([]);
  useEffect(() => {
    if (archive) void api.dirs(archive.id, 0).then(setRoots);
    else setRoots([]);
  }, [archive, version]);

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
      {archive ? (
        <>
          <div className="sb-label">{t('nav.tree')}</div>
          <div className="tree" role="tree">
            <button
              type="button"
              className="tree-item"
              role="treeitem"
              aria-current={cwd === 0 && view === 'browser'}
              onClick={() => navigate(0)}
              title={archive.name}
            >
              <span className="tw" />
              <Icon name="archive" className="fi archive" />
              <span className="lbl">{archive.name}</span>
              <span className="badge" style={{ marginLeft: 'auto', ...formatBadgeStyle(archive.info.format, false) }}>
                {archive.info.format.toUpperCase()}
              </span>
            </button>
            {roots.map((r) => (
              <TreeNode key={r.node} id={archive.id} item={r} depth={1} />
            ))}
          </div>
        </>
      ) : (
        <div className="grow" />
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
          <Icon name="sliders" />
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
  const version = useApp((s) => s.boot?.system.appVersion);
  if (view !== 'browser' || !archive) {
    return (
      <footer className="statusbar">
        <span className="lft">OmniZip {version}</span>
        <span className="mid" />
      </footer>
    );
  }
  const i = archive.info;
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
