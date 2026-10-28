// Archive browser (docs/04-ui-design.md §3, §5): command bar, breadcrumbs and a virtualized,
// paged file list. Rows are fetched from Rust 200 at a time, so a million entries cost the UI
// no more than a hundred.

import { useVirtualizer } from '@tanstack/react-virtual';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { FileIcon, Icon, formatBadgeStyle } from '../../components/Icon';
import { ContextMenu, type MenuItem } from '../../components/ui';
import { api } from '../../lib/api';
import { useFmt, useSize } from '../../lib/hooks';
import { extOf } from '../../lib/format';
import type { Crumb, Row } from '../../lib/types';
import { useApp, type SortKey } from '../../store/app';
import { dragImage, useDragItems } from './drag';

const PAGE = 200;

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const h = setTimeout(() => setV(value), ms);
    return () => clearTimeout(h);
  }, [value, ms]);
  return v;
}

/** Paged listing of one directory (or search results below it). */
function useListing(id: number, dir: number, sort: SortKey, desc: boolean, query: string, version: number) {
  const [total, setTotal] = useState(0);
  const [rows, setRows] = useState<Map<number, Row>>(new Map());
  const [ready, setReady] = useState(false);
  const pending = useRef(new Set<number>());
  const gen = useRef(0);

  const fetchPage = useCallback(
    (page: number, g: number) => {
      if (pending.current.has(page)) return;
      pending.current.add(page);
      void api
        .list(id, dir, sort, desc, page * PAGE, PAGE, query || null)
        .then((p) => {
          if (g !== gen.current) return;
          setTotal(p.total);
          setRows((prev) => {
            const next = new Map(prev);
            p.rows.forEach((r, i) => next.set(p.offset + i, r));
            return next;
          });
          setReady(true);
        })
        .catch(() => {
          pending.current.delete(page);
        });
    },
    [id, dir, sort, desc, query],
  );

  useEffect(() => {
    gen.current += 1;
    pending.current = new Set();
    setRows(new Map());
    setReady(false);
    fetchPage(0, gen.current);
  }, [fetchPage, version]);

  const ensure = useCallback(
    (start: number, end: number) => {
      for (let p = Math.floor(start / PAGE); p <= Math.floor(end / PAGE); p += 1) fetchPage(p, gen.current);
    },
    [fetchPage],
  );
  return { total, rows, ensure, ready };
}

export function Browser() {
  const { t } = useTranslation();
  const fmt = useFmt();
  const archive = useApp((s) => s.archive)!;
  const cwd = useApp((s) => s.cwd);
  const cwdPath = useApp((s) => s.cwdPath);
  const clip = useApp((s) => s.clip);
  const sort = useApp((s) => s.sort);
  const queryRaw = useApp((s) => s.query);
  const version = useApp((s) => s.listVersion);
  const selection = useApp((s) => s.selection);
  const focus = useApp((s) => s.focus);
  const anchor = useApp((s) => s.anchor);
  const setSelection = useApp((s) => s.setSelection);
  const settings = useApp((s) => s.settings)!;
  const navigate = useApp((s) => s.navigate);
  const openDialogSpec = useApp((s) => s.openDialog);
  const setQuery = useApp((s) => s.setQuery);
  const query = useDebounced(queryRaw.trim(), 150);
  const { total, rows, ensure, ready } = useListing(archive.id, cwd, sort.key, sort.desc, query, version);
  const [crumbs, setCrumbs] = useState<Crumb[]>([]);
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const viewMode = settings.appearance.view_mode;
  const searching = query.length > 0;

  useEffect(() => {
    void api.ancestors(archive.id, cwd).then((c) => {
      setCrumbs(c);
      useApp.setState({ cwdPath: c.map((x) => x.name).join('/') });
    });
  }, [archive.id, cwd, version]);

  const parent = crumbs.length > 1 ? crumbs[crumbs.length - 2].node : crumbs.length === 1 ? 0 : null;

  // ---- selection helpers
  const rowByNode = useCallback(
    (node: number) => {
      for (const r of rows.values()) if (r.node === node) return r;
      return undefined;
    },
    [rows],
  );
  const indexOf = useCallback(
    (node: number | null) => {
      if (node == null) return -1;
      for (const [i, r] of rows) if (r.node === node) return i;
      return -1;
    },
    [rows],
  );
  const select = useCallback(
    (nodes: Set<number>, f: number | null, a?: number | null) => {
      let bytes = 0;
      for (const n of nodes) bytes += rowByNode(n)?.size ?? 0;
      setSelection(nodes, f, a === undefined ? f : a, bytes);
    },
    [rowByNode, setSelection],
  );
  const selectedNodes = useMemo(() => Array.from(selection), [selection]);

  const clickRow = (e: React.MouseEvent, index: number, r: Row) => {
    if (e.shiftKey && anchor != null) {
      const a = indexOf(anchor);
      const next = new Set<number>(e.ctrlKey || e.metaKey ? selection : []);
      for (let i = Math.min(a, index); i <= Math.max(a, index); i += 1) {
        const x = rows.get(i);
        if (x) next.add(x.node);
      }
      select(next, r.node, anchor);
    } else if (e.ctrlKey || e.metaKey) {
      const next = new Set(selection);
      if (next.has(r.node)) next.delete(r.node);
      else next.add(r.node);
      select(next, r.node);
    } else {
      select(new Set([r.node]), r.node);
    }
  };

  const openRow = (r: Row) => {
    if (r.isDir) navigate(r.node);
    else openDialogSpec({ kind: 'preview', node: r.node });
  };

  // ---- commands
  const readonly = !archive.writable;
  // A new archive not saved yet: filled from disk and saved, nothing to extract, test or mount.
  const draft = archive.draft;
  const readonlyToast = () =>
    useApp.getState().toast({ tone: 'warn', text: t('toast.readonly', { format: archive.info.format.toUpperCase() }) });
  const addPicked = async (directory: boolean, title: string) => {
    const picked = await openDialog({ multiple: true, directory, title });
    const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
    if (paths.length) await api.add(archive.id, paths, cwd).catch((e) => useApp.getState().reportError(e));
  };
  const cmd = {
    add: async () => {
      if (readonly) return readonlyToast();
      await addPicked(false, draft ? t('draft.addFiles') : t('cmd.add'));
    },
    addFolder: () => void addPicked(true, t('draft.addFolder')),
    mkdir: () => openDialogSpec({ kind: 'rename', node: cwd, name: t('draft.newFolder'), mkdir: true }),
    save: () => openDialogSpec({ kind: 'compress', paths: [], draft: archive.id }),
    extract: (nodes: number[]) => openDialogSpec({ kind: 'extract', nodes, relativeTo: nodes.length ? cwd : undefined }),
    test: () => void api.test(archive.id).catch((e) => useApp.getState().reportError(e)),
    convert: () => openDialogSpec({ kind: 'convert', paths: [archive.path] }),
    mount: () => openDialogSpec({ kind: 'mount' }),
    remove: (nodes: number[]) => {
      if (readonly) return readonlyToast();
      if (!nodes.length) return;
      openDialogSpec({
        kind: 'ask',
        title: t('browser.deleteTitle'),
        message: t('browser.deleteMessage', { count: nodes.length, name: archive.name }),
        confirm: t('cmd.delete'),
        danger: true,
        onConfirm: () => void api.remove(archive.id, nodes).catch((e) => useApp.getState().reportError(e)),
      });
    },
    rename: (r: Row | undefined) => {
      if (!r) return;
      if (readonly) return readonlyToast();
      openDialogSpec({ kind: 'rename', node: r.node, name: r.name });
    },
    props: (node: number) => openDialogSpec({ kind: 'props', node }),
    openWith: (node: number) => void api.openExternal(archive.id, node).catch((e) => useApp.getState().reportError(e)),
    copy: (nodes: number[], cut: boolean) => {
      if (!nodes.length) return;
      // Items of an archive that cannot change can only be copied.
      if (cut && readonly) useApp.getState().toast({ tone: 'info', text: t('toast.cutReadonly') });
      void api.clipCopy(archive.id, nodes, cut).catch((e) => useApp.getState().reportError(e));
    },
    paste: () => {
      if (readonly) return readonlyToast();
      void api.clipPaste(archive.id, cwd).catch((e) => useApp.getState().reportError(e));
    },
    copyPath: async (node: number) => {
      const info = await api.node(archive.id, node);
      await navigator.clipboard.writeText(info.path).catch(() => undefined);
      useApp.getState().toast({ tone: 'info', text: t('toast.copied') });
    },
    selectAll: () => {
      const all = new Set<number>();
      rows.forEach((r) => all.add(r.node));
      select(all, focus);
    },
    encoding: (x: number, y: number) => {
      const boot = useApp.getState().boot!;
      const current = archive.info.encoding ?? 'auto';
      const items: MenuItem[] = ['auto', ...boot.encodings].map((enc) => ({
        label: enc === 'auto' ? t('settings.language.encodingAuto') : enc,
        checked: enc === current || (enc === 'auto' && !archive.info.encoding),
        run: () =>
          void api
            .reencode(archive.id, enc)
            .then((a) => useApp.getState().setArchive(a))
            .catch((e) => useApp.getState().reportError(e)),
      }));
      setMenu({ x, y, items });
    },
  };

  // Dragging items onto a folder moves them there, or copies them with Ctrl: in this archive
  // (list, breadcrumb, sidebar tree) or into another open one (its tree in the sidebar). Items of
  // an archive that cannot change are copied. Out of the window (on Windows), they are extracted
  // where they are dropped.
  const dragOut =
    document.documentElement.dataset.platform === 'win'
      ? (nodes: number[]) => {
          const text = nodes.length === 1 ? (rowByNode(nodes[0])?.name ?? '') : t('status.items', { count: nodes.length });
          void api.dragOut(archive.id, nodes, dragImage(text)).catch((e) => useApp.getState().reportError(e));
        }
      : undefined;
  const dragItems = useDragItems(
    archive.id,
    !readonly,
    (nodes, target, copy) => {
      const done =
        target.archive === archive.id && !copy
          ? api.move(archive.id, nodes, target.dir)
          : api.transfer(archive.id, nodes, target.archive, target.dir, copy);
      void done.catch((e) => useApp.getState().reportError(e));
    },
    dragOut,
  );
  const pressRow = (e: React.PointerEvent, r: Row) => {
    dragItems.press(
      e,
      () => {
        const sel = useApp.getState().selection;
        if (sel.has(r.node)) return Array.from(sel);
        select(new Set([r.node]), r.node);
        return [r.node];
      },
      (n, copy) =>
        n.length === 1
          ? t(copy ? 'browser.copyOne' : 'browser.moveOne', { name: rowByNode(n[0])?.name ?? r.name })
          : t(copy ? 'browser.copyMany' : 'browser.moveMany', { count: n.length }),
    );
  };

  const rowMenu = (x: number, y: number, r: Row) => {
    const nodes = selection.has(r.node) ? selectedNodes : [r.node];
    if (!selection.has(r.node)) select(new Set([r.node]), r.node);
    setMenu({
      x,
      y,
      items: [
        { label: r.isDir ? t('cmd.open') : t('cmd.preview'), icon: r.isDir ? 'folderOpen' : 'eye', kbd: r.isDir ? 'Enter' : 'Space', run: () => openRow(r) },
        ...(!r.isDir ? [{ label: t('cmd.openWith'), icon: 'external' as const, run: () => cmd.openWith(r.node) }] : []),
        { separator: true },
        ...(draft
          ? []
          : [
              { label: t('cmd.extractSelected'), icon: 'extract' as const, kbd: 'Ctrl+E', run: () => cmd.extract(nodes) },
              { separator: true },
            ]),
        { label: t('cmd.cut'), icon: 'scissors', kbd: 'Ctrl+X', disabled: readonly, run: () => cmd.copy(nodes, true) },
        { label: t('cmd.copy'), icon: 'copy', kbd: 'Ctrl+C', run: () => cmd.copy(nodes, false) },
        { label: t('cmd.copyPath'), icon: 'link', run: () => void cmd.copyPath(r.node) },
        { label: t('cmd.rename'), icon: 'edit', kbd: 'F2', disabled: readonly || nodes.length !== 1, run: () => cmd.rename(r) },
        { label: t('cmd.delete'), icon: 'trash', kbd: 'Del', danger: true, disabled: readonly, run: () => cmd.remove(nodes) },
        { separator: true },
        { label: t('cmd.props'), icon: 'info', kbd: 'Alt+Enter', disabled: nodes.length !== 1, run: () => cmd.props(r.node) },
      ],
    });
  };

  const bgMenu = (x: number, y: number) =>
    setMenu({
      x,
      y,
      items: draft
        ? [
            { label: t('draft.addFiles'), icon: 'plus', run: () => void cmd.add() },
            { label: t('draft.addFolder'), icon: 'folderPlus', run: cmd.addFolder },
            { label: t('draft.newFolder'), icon: 'folder', run: cmd.mkdir },
            { separator: true },
            { label: t('cmd.paste'), icon: 'clipboard', kbd: 'Ctrl+V', run: cmd.paste },
            { label: t('cmd.selectAll'), icon: 'check', kbd: 'Ctrl+A', run: cmd.selectAll },
          ]
        : [
            { label: t('cmd.add'), icon: 'plus', disabled: readonly, run: () => void cmd.add() },
            { label: t('cmd.paste'), icon: 'clipboard', kbd: 'Ctrl+V', disabled: readonly, run: cmd.paste },
            { label: t('cmd.selectAll'), icon: 'check', kbd: 'Ctrl+A', run: cmd.selectAll },
            { separator: true },
            { label: t('cmd.extract'), icon: 'extract', run: () => cmd.extract([]) },
            { label: t('cmd.test'), icon: 'checkCircle', kbd: 'Ctrl+T', run: cmd.test },
            { label: t('cmd.encoding'), icon: 'globe', run: () => cmd.encoding(x, y) },
          ],
    });

  // ---- virtualization
  const scrollRef = useRef<HTMLDivElement>(null);
  const [sizeRef, size] = useSize<HTMLDivElement>();
  const rowH = (settings.appearance.density === 'compact' ? 28 : settings.appearance.density === 'spacious' ? 40 : 32) + (searching ? 14 : 0);
  const cellW = 132;
  const cols = viewMode === 'grid' ? Math.max(1, Math.floor((size.width - 32) / cellW)) : 1;
  const lineCount = viewMode === 'grid' ? Math.ceil(total / cols) : total;
  const lineH = viewMode === 'grid' ? 116 : rowH;
  const virt = useVirtualizer({
    count: lineCount,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => lineH,
    overscan: 8,
    scrollPaddingStart: viewMode === 'grid' ? 0 : 30,
  });
  useEffect(() => virt.measure(), [lineH, cols, virt]);
  const items = virt.getVirtualItems();
  useEffect(() => {
    if (!items.length) return;
    ensure(items[0].index * cols, (items[items.length - 1].index + 1) * cols);
  }, [items, cols, ensure]);

  // Items cut from this archive are dimmed until pasted.
  const cutPaths = clip?.cut && clip.archive === archive.id ? new Set(clip.paths) : null;
  const isCut = (r: Row) => {
    if (!cutPaths) return false;
    const parent = r.parent ?? cwdPath;
    return cutPaths.has(parent ? `${parent}/${r.name}` : r.name);
  };

  const scrollToIndex = (i: number) => virt.scrollToIndex(viewMode === 'grid' ? Math.floor(i / cols) : i, { align: 'auto' });

  // ---- keyboard (docs/04-ui-design.md §5.1)
  const typeahead = useRef({ text: '', at: 0 });
  const onKey = (e: React.KeyboardEvent) => {
    const mod = e.ctrlKey || e.metaKey;
    const cur = Math.max(0, indexOf(focus));
    const move = (to: number) => {
      const i = Math.max(0, Math.min(total - 1, to));
      ensure(i, i);
      const r = rows.get(i);
      if (!r) return;
      if (e.shiftKey && anchor != null) {
        const a = indexOf(anchor);
        const next = new Set<number>();
        for (let k = Math.min(a, i); k <= Math.max(a, i); k += 1) {
          const x = rows.get(k);
          if (x) next.add(x.node);
        }
        select(next, r.node, anchor);
      } else {
        select(new Set([r.node]), r.node);
      }
      scrollToIndex(i);
    };
    const step = viewMode === 'grid' ? cols : 1;
    const page = Math.max(1, Math.floor((scrollRef.current?.clientHeight ?? 400) / lineH)) * step;
    const focused = focus != null ? rowByNode(focus) : undefined;
    switch (true) {
      case e.key === 'ArrowDown':
        e.preventDefault();
        move(focus == null ? 0 : cur + step);
        break;
      case e.key === 'ArrowUp':
        e.preventDefault();
        move(cur - step);
        break;
      case e.key === 'ArrowRight' && viewMode === 'grid':
        e.preventDefault();
        move(cur + 1);
        break;
      case e.key === 'ArrowLeft' && viewMode === 'grid':
        e.preventDefault();
        move(cur - 1);
        break;
      case e.key === 'PageDown':
        e.preventDefault();
        move(cur + page);
        break;
      case e.key === 'PageUp':
        e.preventDefault();
        move(cur - page);
        break;
      case e.key === 'Home':
        e.preventDefault();
        move(0);
        break;
      case e.key === 'End':
        e.preventDefault();
        move(total - 1);
        break;
      case e.key === 'Enter' && e.altKey:
        e.preventDefault();
        if (focused) cmd.props(focused.node);
        break;
      case e.key === 'Enter':
        e.preventDefault();
        if (focused) openRow(focused);
        break;
      case e.key === 'Backspace':
        e.preventDefault();
        if (searching) setQuery('');
        else if (parent != null) navigate(parent);
        break;
      case e.key === 'Delete':
        e.preventDefault();
        cmd.remove(selectedNodes);
        break;
      case e.key === 'F2':
        e.preventDefault();
        if (selectedNodes.length === 1) cmd.rename(rowByNode(selectedNodes[0]));
        break;
      case e.key === ' ':
        e.preventDefault();
        if (focused && !focused.isDir) openDialogSpec({ kind: 'preview', node: focused.node });
        break;
      case mod && e.key.toLowerCase() === 'a':
        e.preventDefault();
        cmd.selectAll();
        break;
      case e.key.length === 1 && !mod && !e.altKey: {
        const now = Date.now();
        const ta = typeahead.current;
        ta.text = now - ta.at < 800 ? ta.text + e.key.toLowerCase() : e.key.toLowerCase();
        ta.at = now;
        for (let k = 1; k <= rows.size; k += 1) {
          const i = (cur + (ta.text.length > 1 ? 0 : k)) % Math.max(1, total);
          const r = rows.get(i);
          if (r?.name.toLowerCase().startsWith(ta.text)) {
            move(i);
            break;
          }
        }
        break;
      }
      default:
    }
  };

  // Global shortcuts that belong to the browser.
  useEffect(() => {
    const onDoc = (e: KeyboardEvent) => {
      if (useApp.getState().dialogs.length) return;
      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;
      const k = e.key.toLowerCase();
      // Copy, cut and paste items, unless text is being edited or selected.
      const editing = e.target instanceof HTMLElement && e.target.closest('input, textarea, [contenteditable="true"]');
      if ((k === 'c' || k === 'x' || k === 'v') && !e.shiftKey && !e.altKey && !editing && !window.getSelection()?.toString()) {
        e.preventDefault();
        if (k === 'v') cmd.paste();
        else cmd.copy(Array.from(useApp.getState().selection), k === 'x');
      } else if (draft && k === 's') {
        e.preventDefault();
        cmd.save();
      } else if (k === 'e' && !draft) {
        e.preventDefault();
        cmd.extract(e.shiftKey ? [] : Array.from(useApp.getState().selection));
      } else if (k === 'f') {
        e.preventDefault();
        document.getElementById('search')?.focus();
      } else if (k === 't' && !draft) {
        e.preventDefault();
        cmd.test();
      } else if (k === 'i') {
        e.preventDefault();
        void cmd.add();
      } else if (k === 'm' && !draft) {
        e.preventDefault();
        cmd.mount();
      } else if (e.key === '1' || e.key === '2') {
        e.preventDefault();
        void useApp.getState().updateSettings((s) => (s.appearance.view_mode = e.key === '1' ? 'table' : 'grid'));
      }
    };
    document.addEventListener('keydown', onDoc);
    return () => document.removeEventListener('keydown', onDoc);
  });

  const sortHeader = (key: SortKey, label: string, cls = '') => (
    <button
      type="button"
      className={cls}
      aria-sort={sort.key === key ? (sort.desc ? 'descending' : 'ascending') : 'none'}
      onClick={() => useApp.getState().setSort(key)}
    >
      {label}
      <Icon name={sort.desc ? 'chevronDown' : 'chevronUp'} />
    </button>
  );

  const typeLabel = (r: Row) => {
    if (r.isDir) return t('type.folder');
    if (r.kind === 'symlink') return t('type.symlink');
    const e = extOf(r.name);
    return e ? e.toUpperCase() : t('type.file');
  };

  const histIdx = useApp((s) => s.histIdx);
  const histLen = useApp((s) => s.history.length);
  const n = selectedNodes.length;
  const btn = (key: string, icon: Parameters<typeof Icon>[0]['name'], label: string, run: () => void, opts: { primary?: boolean; disabled?: boolean } = {}) => (
    <button key={key} type="button" className={`btn${opts.primary ? ' primary' : ''}`} disabled={opts.disabled} title={label} onClick={run}>
      <Icon name={icon} />
      <span className="lbl">{label}</span>
    </button>
  );
  const dark = useApp((s) => s.theme?.appearance === 'dark');

  return (
    <>
      <div className="cmdbar">
        {draft ? (
          <>
            {btn('add', 'plus', t('draft.addFiles'), () => void cmd.add())}
            {btn('addFolder', 'folderPlus', t('draft.addFolder'), cmd.addFolder)}
            {btn('mkdir', 'folder', t('draft.newFolder'), cmd.mkdir)}
            {btn('save', 'package', t('draft.save'), cmd.save, { primary: true, disabled: archive.entries === 0 })}
          </>
        ) : (
          <>
            {btn('add', 'plus', t('cmd.add'), () => void cmd.add(), { disabled: readonly })}
            {btn('extract', 'extract', n ? t('cmd.extractSelected') : t('cmd.extract'), () => cmd.extract(selectedNodes), { primary: true })}
            {btn('mount', 'drive', t('cmd.mount'), cmd.mount)}
            {btn('test', 'checkCircle', t('cmd.test'), cmd.test)}
          </>
        )}
        {n > 0 && btn('delete', 'trash', t('cmd.delete'), () => cmd.remove(selectedNodes), { disabled: readonly })}
        {n === 1 && btn('props', 'info', t('cmd.props'), () => cmd.props(selectedNodes[0]))}
        <button
          type="button"
          className="iconbtn"
          title={t('cmd.more')}
          aria-label={t('cmd.more')}
          onClick={(e) => {
            const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
            setMenu({
              x: r.left,
              y: r.bottom + 4,
              items: [
                ...(draft
                  ? []
                  : [
                      { label: t('cmd.convert'), icon: 'refresh' as const, run: cmd.convert },
                      { label: t('cmd.encoding'), icon: 'globe' as const, run: () => cmd.encoding(r.left, r.bottom + 4) },
                    ]),
                { label: t('cmd.rename'), icon: 'edit', disabled: readonly || n !== 1, run: () => cmd.rename(rowByNode(selectedNodes[0])) },
                { label: t('cmd.copyPath'), icon: 'copy', disabled: n !== 1, run: () => void cmd.copyPath(selectedNodes[0]) },
                { label: t('cmd.selectAll'), icon: 'check', kbd: 'Ctrl+A', run: cmd.selectAll },
                { separator: true },
                ...(draft ? [] : [{ label: t('cmd.reveal'), icon: 'folderOpen' as const, run: () => void api.reveal(archive.path) }]),
                { label: t('cmd.closeArchive'), icon: 'x', run: () => useApp.getState().closeArchive() },
              ],
            });
          }}
        >
          <Icon name="more" />
        </button>
        <span className="grow" />
        <button type="button" className="iconbtn" disabled={histIdx <= 0} title={t('cmd.back')} aria-label={t('cmd.back')} onClick={() => useApp.getState().back()}>
          <Icon name="chevronLeft" />
        </button>
        <button type="button" className="iconbtn" disabled={histIdx >= histLen - 1} title={t('cmd.forward')} aria-label={t('cmd.forward')} onClick={() => useApp.getState().forward()}>
          <Icon name="chevronRight" />
        </button>
        <button type="button" className="iconbtn" disabled={parent == null} title={t('cmd.up')} aria-label={t('cmd.up')} onClick={() => parent != null && navigate(parent)}>
          <Icon name="arrowUp" />
        </button>
        <span style={{ width: 8 }} />
        <button type="button" className="iconbtn" aria-pressed={viewMode === 'table'} title={t('cmd.viewTable')} aria-label={t('cmd.viewTable')} onClick={() => void useApp.getState().updateSettings((s) => (s.appearance.view_mode = 'table'))}>
          <Icon name="list" />
        </button>
        <button type="button" className="iconbtn" aria-pressed={viewMode === 'grid'} title={t('cmd.viewGrid')} aria-label={t('cmd.viewGrid')} onClick={() => void useApp.getState().updateSettings((s) => (s.appearance.view_mode = 'grid'))}>
          <Icon name="grid" />
        </button>
      </div>

      <div className="crumbs">
        {searching ? (
          <>
            <Icon name="search" />
            <span className="cur" style={{ marginLeft: 6 }}>{query}</span>
            <span className="sep">·</span>
            <span>{t('status.results', { count: total })}</span>
            <span className="grow" />
            <button type="button" onClick={() => setQuery('')}>{t('empty.clearSearch')}</button>
          </>
        ) : (
          <>
            <button type="button" className={crumbs.length === 0 ? 'cur' : ''} data-drop-archive={archive.id} data-drop-dir={readonly ? undefined : 0} onClick={() => navigate(0)}>
              <Icon name="archive" className="fi archive" />
              {archive.name}
              {!draft && <span className="badge" style={formatBadgeStyle(archive.info.format, dark)}>{archive.info.format.toUpperCase()}</span>}
            </button>
            {crumbs.map((c, i) => (
              <span key={c.node} style={{ display: 'contents' }}>
                <span className="sep">›</span>
                <button type="button" className={i === crumbs.length - 1 ? 'cur' : ''} data-drop-archive={archive.id} data-drop-dir={readonly ? undefined : c.node} onClick={() => navigate(c.node)}>
                  {c.name}
                </button>
              </span>
            ))}
          </>
        )}
      </div>

      <div
        id="filelist"
        className="content"
        ref={(el) => {
          scrollRef.current = el;
          (sizeRef as React.MutableRefObject<HTMLDivElement | null>).current = el;
        }}
        tabIndex={0}
        role="grid"
        aria-label={archive.name}
        aria-rowcount={total}
        onKeyDown={onKey}
        onMouseDown={(e) => {
          if (e.target === e.currentTarget) useApp.getState().clearSelection();
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          const el = (e.target as HTMLElement).closest('[data-index]');
          const r = el ? rows.get(Number(el.getAttribute('data-index'))) : undefined;
          if (r) rowMenu(e.clientX, e.clientY, r);
          else bgMenu(e.clientX, e.clientY);
        }}
      >
        {ready && total === 0 ? (
          <div className="emptyst">
            <Icon name={searching ? 'search' : draft ? 'upload' : 'folderOpen'} />
            <div>{searching ? t('empty.search', { query }) : draft ? t('draft.empty') : t('empty.dir')}</div>
            {draft && !searching && (
              <div className="inline">
                <button type="button" className="btn outline" onClick={() => void cmd.add()}>
                  <Icon name="plus" />
                  <span>{t('draft.addFiles')}</span>
                </button>
                <button type="button" className="btn outline" onClick={cmd.addFolder}>
                  <Icon name="folderPlus" />
                  <span>{t('draft.addFolder')}</span>
                </button>
              </div>
            )}
            {searching && (
              <button type="button" className="btn outline" onClick={() => setQuery('')}>
                {t('empty.clearSearch')}
              </button>
            )}
          </div>
        ) : viewMode === 'grid' ? (
          <div className="gridv" style={{ height: virt.getTotalSize() }}>
            {items.flatMap((line) =>
              Array.from({ length: cols }, (_, c) => {
                const i = line.index * cols + c;
                if (i >= total) return null;
                const r = rows.get(i);
                const w = (size.width - 32) / cols;
                return (
                  <div
                    key={i}
                    data-index={i}
                    role="gridcell"
                    aria-selected={r ? selection.has(r.node) : false}
                    className={`gcell${r && r.node === focus ? ' focus' : ''}${r && isCut(r) ? ' cut' : ''}`}
                    style={{ transform: `translate(${c * w}px, ${line.start}px)`, width: w - 6, height: lineH - 6 }}
                    title={r?.name}
                    data-drop-archive={archive.id}
                    data-drop-dir={!readonly && r?.isDir ? r.node : undefined}
                    onPointerDown={(e) => r && pressRow(e, r)}
                    onClick={(e) => r && !dragItems.dragged.current && clickRow(e, i, r)}
                    onDoubleClick={() => r && openRow(r)}
                  >
                    {r ? (
                      <>
                        <FileIcon name={r.name} isDir={r.isDir} kind={r.kind} />
                        <span>{r.name}</span>
                        <small>{r.isDir ? t('status.items', { count: r.children }) : fmt.size(r.size)}</small>
                      </>
                    ) : null}
                  </div>
                );
              }),
            )}
          </div>
        ) : (
          <>
            <div className="thead" role="row">
              {sortHeader('name', t('col.name'))}
              {sortHeader('size', t('col.size'), 'num col-size')}
              {sortHeader('packed', t('col.packed'), 'num col-packed')}
              {sortHeader('mtime', t('col.mtime'), 'col-mtime')}
              {sortHeader('type', t('col.type'), 'col-type')}
            </div>
            <div className="rows" style={{ height: virt.getTotalSize() }}>
              {items.map((it) => {
                const r = rows.get(it.index);
                if (!r) return <div key={it.index} className="trow placeholder" style={{ transform: `translateY(${it.start}px)` }} />;
                return (
                  <div
                    key={it.index}
                    data-index={it.index}
                    role="row"
                    aria-selected={selection.has(r.node)}
                    className={`trow${r.node === focus ? ' focus' : ''}${searching ? ' srch' : ''}${isCut(r) ? ' cut' : ''}`}
                    style={{ transform: `translateY(${it.start}px)` }}
                    data-drop-archive={archive.id}
                    data-drop-dir={!readonly && r.isDir ? r.node : undefined}
                    onPointerDown={(e) => pressRow(e, r)}
                    onClick={(e) => !dragItems.dragged.current && clickRow(e, it.index, r)}
                    onDoubleClick={() => openRow(r)}
                  >
                    <div className="nm" role="gridcell">
                      <FileIcon name={r.name} isDir={r.isDir} kind={r.kind} />
                      <span className="nm-txt">
                        <span>{r.name}</span>
                        {searching && <span className="sub">{r.parent || '/'}</span>}
                      </span>
                      {r.encrypted && !r.isDir && (
                        <span className="marks" title={t('props.encrypted')}>
                          <Icon name="lock" />
                        </span>
                      )}
                    </div>
                    <div className="num dim col-size" role="gridcell">{r.isDir ? '—' : fmt.size(r.size)}</div>
                    <div className="num dim col-packed" role="gridcell">{r.isDir || !r.packed ? '—' : fmt.size(r.packed)}</div>
                    <div className="dim col-mtime" role="gridcell">{fmt.date(r.mtime)}</div>
                    <div className="dim col-type" role="gridcell">{typeLabel(r)}</div>
                  </div>
                );
              })}
            </div>
          </>
        )}
      </div>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={menu.items} onClose={() => setMenu(null)} />}
      {dragItems.ghost}
    </>
  );
}
