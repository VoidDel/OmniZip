// Extract (docs/04-ui-design.md §4.3).

import { open as pick } from '@tauri-apps/plugin-dialog';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Icon } from '../../components/Icon';
import { Modal, PasswordInput, Select } from '../../components/ui';
import { api } from '../../lib/api';
import { basename, dirname } from '../../lib/format';
import { useFmt } from '../../lib/hooks';
import type { OverwriteMode } from '../../lib/types';
import { type Dialog, useApp } from '../../store/app';
import { startedInShell, useClose } from './shared';

function defaultDestination(paths: string[] | undefined, archivePath: string | undefined): string {
  const s = useApp.getState();
  const mode = s.settings?.general.extract_to ?? 'ask';
  const sys = s.boot?.system;
  const base = paths?.[0] ?? archivePath ?? '';
  if (mode === 'downloads' && sys?.downloadsDir) return sys.downloadsDir;
  if (base) return dirname(base);
  return sys?.downloadsDir ?? sys?.homeDir ?? '';
}

export function ExtractDialog({ d }: { d: Extract<Dialog, { kind: 'extract' }> }) {
  const { t } = useTranslation();
  const fmt = useFmt();
  const close = useClose(d);
  const archive = useApp((s) => s.archive);
  const settings = useApp((s) => s.settings)!;
  const sys = useApp((s) => s.boot!.system);
  const encodings = useApp((s) => s.boot!.encodings);
  const fromDisk = !!d.paths?.length;
  const name = fromDisk ? (d.paths!.length === 1 ? basename(d.paths![0]) : t('extract.batch', { count: d.paths!.length })) : archive?.name ?? '';
  const [dest, setDest] = useState(() => defaultDestination(d.paths, archive?.path));
  const [pathMode, setPathMode] = useState<'smart' | 'full' | 'flat'>(d.nodes?.length ? 'full' : 'smart');
  const [overwrite, setOverwrite] = useState<OverwriteMode>('ask');
  const [password, setPassword] = useState('');
  const [encoding, setEncoding] = useState('auto');
  const [keepBroken, setKeepBroken] = useState(false);
  const [openAfter, setOpenAfter] = useState(settings.general.open_after_extract);
  const [deleteAfter, setDeleteAfter] = useState(false);
  const [each, setEach] = useState((d.paths?.length ?? 0) > 1);
  const [scope, setScope] = useState<[number, number] | null>(null);
  useEffect(() => {
    if (!fromDisk && archive) void api.selectionSize(archive.id, d.nodes ?? []).then(setScope);
  }, [fromDisk, archive, d.nodes]);
  const needsPassword = fromDisk || archive?.info.encrypted;
  const destinations = [
    [defaultDestination(d.paths, archive?.path), t('extract.destSame')],
    [sys.downloadsDir, t('extract.destDownloads')],
    [sys.desktopDir, t('extract.destDesktop')],
  ].filter((x): x is [string, string] => !!x[0]);

  const start = async () => {
    try {
      const shell = useApp.getState().shellLabel;
      const id = await api.extract({
        archiveId: fromDisk ? undefined : archive?.id,
        paths: fromDisk ? d.paths : undefined,
        nodes: d.nodes ?? [],
        relativeTo: d.relativeTo,
        destination: dest,
        pathMode,
        overwrite,
        password: password || null,
        encoding: encoding === 'auto' ? null : encoding,
        openAfter,
        deleteAfter: fromDisk && deleteAfter,
        keepBroken,
        eachToFolder: fromDisk && each,
      }, shell ?? undefined);
      if (!fromDisk && password && archive) void api.setArchivePassword(archive.id, password);
      if (shell) startedInShell(d.id, id);
      else close();
    } catch (e) {
      useApp.getState().reportError(e);
    }
  };

  return (
    <Modal
      title={t('extract.title', { name })}
      onClose={close}
      width={560}
      footer={
        <>
          <button type="button" className="btn" onClick={close}>{t('common.cancel')}</button>
          <button type="button" className="btn primary" disabled={!dest} onClick={() => void start()}>
            <Icon name="extract" />
            <span>{t('extract.start')}</span>
          </button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="ex-dest">{t('extract.to')}</label>
        <div className="inline">
          <input id="ex-dest" className="inp mono grow" value={dest} list="ex-dests" onChange={(e) => setDest(e.target.value)} />
          <datalist id="ex-dests">
            {destinations.map(([p, l]) => (
              <option key={p} value={p}>{l}</option>
            ))}
          </datalist>
          <button
            type="button"
            className="btn outline"
            onClick={async () => {
              const p = await pick({ directory: true, multiple: false, defaultPath: dest || undefined });
              if (typeof p === 'string') setDest(p);
            }}
          >
            {t('common.browse')}
          </button>
        </div>
      </div>
      <div className="fpair">
        <div className="field">
          <label htmlFor="ex-paths">{t('extract.paths')}</label>
          <Select id="ex-paths" value={pathMode} onChange={setPathMode} options={(['smart', 'full', 'flat'] as const).map((m) => [m, t(`extract.${m}`)])} />
        </div>
        <div className="field">
          <label htmlFor="ex-ow">{t('extract.overwrite')}</label>
          <Select id="ex-ow" value={overwrite} onChange={setOverwrite} options={(['ask', 'skip', 'overwrite', 'overwrite-older', 'rename'] as OverwriteMode[]).map((m) => [m, t(`extract.overwriteModes.${m}`)])} />
        </div>
      </div>
      {/* The password stays out of the way: a job that needs one asks for it (docs/04-ui-design.md §4.7). */}
      <details className="adv">
        <summary>
          <Icon name="chevronRight" />
          {t('extract.more')}
          <span className="hint">{t('extract.moreHint')}</span>
        </summary>
        <div className="advbody">
          <div className="fpair">
            {needsPassword && (
              <div className="field">
                <label>{t('extract.password')}</label>
                <PasswordInput value={password} onChange={setPassword} label={t('extract.password')} />
              </div>
            )}
            <div className="field">
              <label htmlFor="ex-enc">{t('extract.encoding')}</label>
              <Select id="ex-enc" value={encoding} onChange={setEncoding} options={[['auto', t('settings.language.encodingAuto')], ...encodings.map((x): [string, string] => [x, x])]} />
            </div>
          </div>
          <label className="chk">
            <input type="checkbox" checked={keepBroken} onChange={(e) => setKeepBroken(e.target.checked)} /> {t('extract.keepBroken')}
          </label>
        </div>
      </details>
      <div className="inline" style={{ gap: '8px 20px' }}>
        <label className="chk">
          <input type="checkbox" checked={openAfter} onChange={(e) => setOpenAfter(e.target.checked)} /> {t('extract.openAfter')}
        </label>
        {fromDisk && (
          <label className="chk">
            <input type="checkbox" checked={deleteAfter} onChange={(e) => setDeleteAfter(e.target.checked)} /> {t('extract.deleteAfter')}
          </label>
        )}
        {fromDisk && (d.paths?.length ?? 0) > 1 && (
          <label className="chk">
            <input type="checkbox" checked={each} onChange={(e) => setEach(e.target.checked)} /> {t('extract.each')}
          </label>
        )}
      </div>
      {scope && (
        <div className="hint">
          {d.nodes?.length ? t('extract.scope', { count: d.nodes.length, size: fmt.size(scope[0]) }) : t('extract.scopeAll', { size: fmt.size(scope[0]) })}
        </div>
      )}
    </Modal>
  );
}
