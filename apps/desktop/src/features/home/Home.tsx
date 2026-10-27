// Start page (docs/04-ui-design.md §4.1).

import { open } from '@tauri-apps/plugin-dialog';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Icon, formatBadgeStyle } from '../../components/Icon';
import { ContextMenu, type MenuItem } from '../../components/ui';
import { api } from '../../lib/api';
import { useFmt } from '../../lib/hooks';
import type { RecentItem } from '../../lib/types';
import { useApp } from '../../store/app';

/** Extensions offered in the "open archive" dialog. */
export function archiveFilter(t: (k: string) => string) {
  const boot = useApp.getState().boot;
  const exts = new Set<string>();
  boot?.formats.filter((f) => f.engine).forEach((f) => f.extensions.forEach((e) => exts.add(e.split('.').pop()!)));
  return [
    { name: t('home.filterArchives'), extensions: Array.from(exts) },
    { name: t('home.filterAll'), extensions: ['*'] },
  ];
}

export async function pickArchive(t: (k: string) => string): Promise<string | null> {
  const p = await open({ multiple: false, directory: false, filters: archiveFilter(t), title: t('home.open') });
  return typeof p === 'string' ? p : null;
}

export function Home({ dragOver }: { dragOver: boolean }) {
  const { t } = useTranslation();
  const fmt = useFmt();
  const openArchive = useApp((s) => s.openArchive);
  const openDialogSpec = useApp((s) => s.openDialog);
  const dark = useApp((s) => s.theme?.appearance === 'dark');
  const version = useApp((s) => s.listVersion);
  const [recent, setRecent] = useState<RecentItem[]>([]);
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  useEffect(() => {
    void api.recent().then(setRecent);
  }, [version]);

  const onOpen = async () => {
    const p = await pickArchive(t);
    if (p) await openArchive(p);
  };
  const onNew = async () => {
    const picked = await open({ multiple: true, directory: false, title: t('home.new') });
    const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
    if (paths.length) openDialogSpec({ kind: 'compress', paths });
  };
  const onNewFolder = async () => {
    const picked = await open({ multiple: false, directory: true, title: t('home.new') });
    if (typeof picked === 'string') openDialogSpec({ kind: 'compress', paths: [picked] });
  };
  const onExtract = async () => {
    const picked = await open({ multiple: true, directory: false, filters: archiveFilter(t), title: t('home.extract') });
    const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
    if (paths.length) openDialogSpec({ kind: 'extract', paths });
  };

  return (
    <div className="content">
      <div className="home">
        <div className={`drop${dragOver ? ' over' : ''}`}>
          <Icon name="upload" />
          <h2>{t('home.dropTitle')}</h2>
          <p>{t('home.dropHint')}</p>
        </div>
        <div className="actions">
          <button type="button" className="btn primary lg" onClick={() => void onOpen()}>
            <Icon name="archive" />
            <span>{t('home.open')}</span>
          </button>
          <button
            type="button"
            className="btn outline lg"
            onClick={() => void onNew()}
          >
            <Icon name="plus" />
            <span>{t('home.new')}</span>
          </button>
          <button type="button" className="btn outline lg" onClick={() => void onNewFolder()}>
            <Icon name="folderPlus" />
            <span>{t('home.newFromFolder')}</span>
          </button>
          <button type="button" className="btn outline lg" onClick={() => void onExtract()}>
            <Icon name="extract" />
            <span>{t('home.extract')}</span>
          </button>
        </div>
        <div>
          <div className="sect">
            <h3>{t('home.recent')}</h3>
            {recent.length > 0 && (
              <button type="button" onClick={() => void api.clearRecent().then(() => setRecent([]))}>
                {t('home.clear')}
              </button>
            )}
          </div>
          {recent.length === 0 ? (
            <p className="hint" style={{ marginTop: 12 }}>{t('home.emptyRecent')}</p>
          ) : (
            <div className="cards">
              {recent.map((r) => (
                <button
                  key={r.path}
                  type="button"
                  className="card"
                  title={r.path}
                  style={formatBadgeStyle(r.format, dark)}
                  onClick={() => void openArchive(r.path)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setMenu({
                      x: e.clientX,
                      y: e.clientY,
                      items: [
                        { label: t('cmd.open'), icon: 'folderOpen', run: () => void openArchive(r.path) },
                        { label: t('home.reveal'), icon: 'external', run: () => void api.reveal(r.path) },
                        { separator: true },
                        { label: t('home.remove'), icon: 'x', run: () => void api.removeRecent(r.path).then(() => setRecent((x) => x.filter((i) => i.path !== r.path))) },
                      ],
                    });
                  }}
                >
                  <span className="ico">{r.format.toUpperCase().replace('TAR.', 'T.')}</span>
                  <span className="meta">
                    <b>{r.name}</b>
                    <span>
                      {fmt.size(r.size)} · {fmt.relative(r.opened)}
                    </span>
                  </span>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={menu.items} onClose={() => setMenu(null)} />}
    </div>
  );
}
