// Convert to… (docs/04-ui-design.md §4.11).

import { open as pick } from '@tauri-apps/plugin-dialog';
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Icon } from '../../components/Icon';
import { Modal, Select } from '../../components/ui';
import { api } from '../../lib/api';
import { archiveStem, basename, dirname, joinPath } from '../../lib/format';
import type { FormatId } from '../../lib/types';
import { type Dialog, useApp } from '../../store/app';
import { LevelField, LEVELS, samePath, startedInShell, useClose } from './shared';

/** Formats an archive can be converted to: those that hold several entries. */
const CONVERT_TO: FormatId[] = ['zip', '7z', 'tar.zst', 'tar.gz', 'tar.xz', 'tar.bz2', 'tar'];

/** Format of an archive file by its name, among the conversion targets. */
const formatOf = (path: string) => CONVERT_TO.find((f) => basename(path).toLowerCase().endsWith(`.${f}`));


/**
 * "Convert to…" (docs/04-ui-design.md §4.11): archives on disk written again in another format,
 * each as `<name>.<format>` in the chosen folder; a name that would replace one of the archives
 * being converted, or another new one, gets a number. A password a source needs is asked for by
 * the job.
 */
export function ConvertDialog({ d }: { d: Extract<Dialog, { kind: 'convert' }> }) {
  const { t } = useTranslation();
  const close = useClose(d);
  const boot = useApp((s) => s.boot)!;
  const settings = useApp((s) => s.settings)!;
  const shell = useApp((s) => s.shellLabel);
  const sep = boot.system.pathSeparator;
  const one = d.paths.length === 1;
  const name = one ? basename(d.paths[0]) : t('extract.batch', { count: d.paths.length });
  const [format, setFormat] = useState<FormatId>(() => {
    const preferred = CONVERT_TO.find((f) => f === settings.formats.default_format) ?? 'zip';
    return preferred !== formatOf(d.paths[0]) ? preferred : preferred === '7z' ? 'zip' : '7z';
  });
  const [level, setLevel] = useState(Math.max(0, LEVELS.indexOf(settings.formats.default_level)));
  const [dir, setDir] = useState(() => dirname(d.paths[0]));
  const [verify, setVerify] = useState(true);
  const [openAfter, setOpenAfter] = useState(false);
  const [deleteSource, setDeleteSource] = useState(false);
  const items = useMemo(() => {
    const taken: string[] = [];
    const free = (p: string) => !d.paths.some((s) => samePath(s, p)) && !taken.some((x) => samePath(x, p));
    return d.paths.map((source) => {
      const stem = archiveStem(basename(source));
      let destination = joinPath(dir, `${stem}.${format}`, sep);
      for (let n = 2; !free(destination); n++) destination = joinPath(dir, `${stem} (${n}).${format}`, sep);
      taken.push(destination);
      return { source, destination };
    });
  }, [d.paths, dir, format, sep]);
  const [existing, setExisting] = useState<string[]>([]);
  useEffect(() => {
    if (!dir) {
      setExisting([]);
      return;
    }
    void Promise.all(items.map((i) => api.exists(i.destination))).then((r) => setExisting(items.filter((_, k) => r[k]).map((i) => i.destination)));
  }, [items, dir]);

  const start = async () => {
    try {
      const id = await api.convert({ items, format, level: LEVELS[level], verify, deleteSource, openAfter }, shell ?? undefined);
      if (shell) startedInShell(d.id, id);
      else close();
    } catch (e) {
      useApp.getState().reportError(e);
    }
  };

  return (
    <Modal
      title={t('convert.title', { name })}
      onClose={close}
      width={560}
      footer={
        <>
          <button type="button" className="btn" onClick={close}>{t('common.cancel')}</button>
          <button type="button" className="btn primary" disabled={!dir} onClick={() => void start()}>
            <Icon name="refresh" />
            <span>{t('convert.start')}</span>
          </button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="cv-dest">{t('convert.to')}</label>
        <div className="inline">
          <input id="cv-dest" className="inp mono grow" value={dir} onChange={(e) => setDir(e.target.value)} />
          <button
            type="button"
            className="btn outline"
            onClick={async () => {
              const p = await pick({ directory: true, multiple: false, defaultPath: dir || undefined });
              if (typeof p === 'string') setDir(p);
            }}
          >
            {t('common.browse')}
          </button>
        </div>
        {one ? (
          <div className="hint ellipsis" title={items[0].destination}>{t('convert.output', { name: basename(items[0].destination) })}</div>
        ) : (
          <div className="hint">{t('convert.outputs', { count: items.length, format: format.toUpperCase() })}</div>
        )}
        {existing.length > 0 && (
          <div className="note warn">
            <Icon name="alert" />
            <span>{one ? t('compress.exists', { name: basename(existing[0]) }) : t('convert.existsMany', { count: existing.length })}</span>
          </div>
        )}
      </div>
      <div className="fmt-level">
        <div className="field">
          <label htmlFor="cv-format">{t('convert.format')}</label>
          <Select id="cv-format" value={format} onChange={setFormat} options={CONVERT_TO.map((f) => [f, f.toUpperCase()])} />
        </div>
        <LevelField level={level} onChange={setLevel} />
      </div>
      <div className="field" style={{ gap: 8 }}>
        <label className="chk"><input type="checkbox" checked={verify} onChange={(e) => setVerify(e.target.checked)} /> {t('compress.verify')}</label>
        <label className="chk"><input type="checkbox" checked={openAfter} onChange={(e) => setOpenAfter(e.target.checked)} /> {t('convert.openAfter')}</label>
        <label className="chk"><input type="checkbox" checked={deleteSource} onChange={(e) => setDeleteSource(e.target.checked)} /> {t('convert.deleteSource')}</label>
      </div>
    </Modal>
  );
}
