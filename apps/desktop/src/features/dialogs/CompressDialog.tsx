// New archive / Add to archive (docs/04-ui-design.md §4.2).

import { save } from '@tauri-apps/plugin-dialog';
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Icon } from '../../components/Icon';
import { Modal, PasswordInput, Select, Tabs } from '../../components/ui';
import { api } from '../../lib/api';
import { archiveStem, basename, dirname, joinPath } from '../../lib/format';
import { useFmt } from '../../lib/hooks';
import type { FormatId, PathStats } from '../../lib/types';
import { type Dialog, useApp } from '../../store/app';
import { LEVEL_RATIO, LEVEL_SPEED, LevelField, LEVELS, startedInShell, useClose } from './shared';

/** Tabs of the Advanced section of the compress dialog. */
type AdvTab = 'encrypt' | 'volume' | 'method' | 'other';

const WRITABLE: FormatId[] = ['zip', '7z', 'tar.zst', 'tar.gz', 'tar.xz', 'tar.bz2', 'tar', 'gz', 'xz', 'zst', 'bz2'];

export function CompressDialog({ d }: { d: Extract<Dialog, { kind: 'compress' }> }) {
  const { t } = useTranslation();
  const fmt = useFmt();
  const close = useClose(d);
  const boot = useApp((s) => s.boot)!;
  const settings = useApp((s) => s.settings)!;
  const sep = boot.system.pathSeparator;
  const shell = useApp((s) => s.shellLabel);
  const [stats, setStats] = useState<PathStats | null>(null);
  const [format, setFormat] = useState<FormatId>((settings.formats.default_format as FormatId) || 'zip');
  const [level, setLevel] = useState(Math.max(0, LEVELS.indexOf(settings.formats.default_level)));
  const [dir, setDir] = useState('');
  const [stem, setStem] = useState('');
  const [password, setPassword] = useState('');
  const [encryptNames, setEncryptNames] = useState(true);
  const [volume, setVolume] = useState<'none' | 'fat32' | 'dvd' | 'email' | 'custom'>('none');
  const [customMb, setCustomMb] = useState(100);
  const [method, setMethod] = useState('');
  const [solid, setSolid] = useState(true);
  const [skipCompressed, setSkipCompressed] = useState(settings.formats.skip_compressed);
  const [comment, setComment] = useState('');
  const [exclude, setExclude] = useState('');
  const [deleteSource, setDeleteSource] = useState(false);
  const [verify, setVerify] = useState(true);
  const [openAfter, setOpenAfter] = useState(false);
  const [exists, setExists] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);
  const [advTab, setAdvTab] = useState<AdvTab>('encrypt');

  // Saving a new archive: its items are already chosen (no exclusions, nothing to delete
  // afterwards), and it is saved to Downloads under its name unless the user picks a place.
  const draft = d.draft !== undefined;
  useEffect(() => {
    const a = useApp.getState().archive;
    const stats: Promise<PathStats> =
      draft && a ? Promise.resolve({ bytes: a.totalSize, files: a.files, commonParent: null, suggestedName: a.name }) : api.pathStats(d.paths);
    void stats.then((s) => {
      setStats(s);
      setDir(s.commonParent ?? boot.system.downloadsDir ?? '');
      setStem(s.suggestedName);
    });
  }, [d.paths, draft, boot.system.downloadsDir]);
  const ext = '.' + format;
  const dest = dir ? joinPath(dir, stem + ext, sep) : '';
  useEffect(() => {
    if (dest) void api.exists(dest).then(setExists);
  }, [dest]);

  const support = boot.formats.find((f) => f.format === format);
  const canEncrypt = support?.capabilities.includes('ENCRYPT') ?? false;
  const singleFile = support?.capabilities.includes('SINGLE_FILE') ?? false;
  const methods = boot.writeMethods[format] ?? [];
  const tooMany = singleFile && (stats?.files ?? 0) !== 1;
  const bytes = stats?.bytes ?? 0;
  const factor = format === 'zip' ? 1.1 : format.startsWith('tar') ? 1.05 : 1;
  const out = Math.round(bytes * LEVEL_RATIO[level] * factor);
  const secs = bytes / (LEVEL_SPEED[level] * 1024 * 1024);
  const volumeSize =
    volume === 'fat32' ? 4 * 1024 ** 3 - 1 : volume === 'dvd' ? 4_700_000_000 : volume === 'email' ? 25 * 1024 * 1024 : volume === 'custom' ? customMb * 1024 * 1024 : null;
  // Formats without a choice of method (and not 7z, which has solid mode) have no Method tab;
  // a new archive has nothing under Other but a ZIP comment.
  const canSkip = (format === 'zip' || format === '7z') && LEVELS[level] !== 'store' && !/^(store|copy)$/i.test(method);
  const tabs: AdvTab[] = [
    'encrypt',
    'volume',
    ...(methods.length > 0 || format === '7z' ? (['method'] as const) : []),
    ...(!draft || format === 'zip' ? (['other'] as const) : []),
  ];
  const tab = tabs.includes(advTab) ? advTab : 'encrypt';
  const panel = (id: AdvTab) => ({ className: `tabpanel${tab === id ? '' : ' off'}`, inert: tab !== id, role: 'tabpanel', 'aria-labelledby': `cf-tab-${id}` });
  // Tabs holding a setting that is not the default show a dot.
  const changed: Record<AdvTab, boolean> = {
    encrypt: canEncrypt && !!password,
    volume: volume !== 'none',
    method: !!method || (format === '7z' && !solid) || (canSkip && skipCompressed !== settings.formats.skip_compressed),
    other: (format === 'zip' && !!comment) || (!draft && !!exclude.trim()),
  };
  const activeAdvanced = [
    canEncrypt && password ? t('props.encrypted') : null,
    volume !== 'none' ? `${t('compress.volume')} ${volume === 'custom' ? `${customMb} MB` : t(`compress.volumes.${volume}`)}` : null,
  ].filter((x): x is string => x !== null);
  const strength = useMemo(() => {
    let s = 0;
    if (password.length >= 8) s += 1;
    if (/[A-Z]/.test(password) && /[a-z]/.test(password)) s += 1;
    if (/\d/.test(password)) s += 1;
    if (/[^\w]/.test(password) || password.length >= 14) s += 1;
    return s;
  }, [password]);

  const start = async () => {
    try {
      const id = await api.compress({
        paths: d.paths,
        destination: dest,
        format,
        level: LEVELS[level],
        method: method || null,
        password: canEncrypt && password ? password : null,
        encryptNames: format === '7z' && encryptNames,
        solid,
        volumeSize,
        comment: comment || null,
        exclude: draft ? [] : exclude.split(/\r?\n/).map((x) => x.trim()).filter(Boolean),
        deleteSource: !draft && deleteSource,
        verify,
        openAfter,
        draft: d.draft ?? null,
        skipCompressed: canSkip && skipCompressed,
      }, shell ?? undefined);
      if (d.draft !== undefined) useApp.getState().savingDraft(id, d.draft);
      if (shell) startedInShell(d.id, id);
      else close();
    } catch (e) {
      useApp.getState().reportError(e);
    }
  };

  return (
    <Modal
      title={t('compress.title')}
      onClose={close}
      width={600}
      footer={
        <>
          {/* What is compressed and what it should come to, next to the button that starts it
              (the estimate follows the format as much as the level). */}
          <span className="est grow">
            {stats &&
              (bytes > 0
                ? `${t('compress.estimate', { count: stats.files, from: fmt.size(bytes), to: fmt.size(out) })} · ${fmt.duration(secs)}`
                : t('compress.sources', { count: stats.files, size: fmt.size(bytes) }))}
          </span>
          <button type="button" className="btn" onClick={close}>{t('common.cancel')}</button>
          <button type="button" className="btn primary" disabled={!dest || tooMany || !stats} onClick={() => void start()}>
            <Icon name="package" />
            <span>{t('compress.start')}</span>
          </button>
        </>
      }
    >
      <div className="field">
        {/* The full path follows the label; a long one loses its start, not the file name. */}
        <div className="lrow">
          <label htmlFor="cf-name">{t('compress.saveTo')}</label>
          <span className="hint mono path-tail" title={dest}><bdi>{dest}</bdi></span>
        </div>
        <div className="inline">
          <input id="cf-name" className="inp grow" value={stem} onChange={(e) => setStem(e.target.value)} />
          <span className="mono hint">{ext}</span>
          <button
            type="button"
            className="btn outline"
            onClick={async () => {
              const p = await save({ defaultPath: dest || undefined, filters: [{ name: format.toUpperCase(), extensions: [format.split('.').pop()!] }] });
              if (p) {
                setDir(dirname(p));
                setStem(archiveStem(basename(p)));
              }
            }}
          >
            {t('common.browse')}
          </button>
        </div>
        {exists && <div className="note warn"><Icon name="alert" /><span>{t('compress.exists', { name: stem + ext })}</span></div>}
        {tooMany && <div className="note warn"><Icon name="alert" /><span>{t('compress.singleFile', { format: format.toUpperCase() })}</span></div>}
      </div>
      <div className="fmt-level">
        <div className="field">
          <label htmlFor="cf-format">{t('compress.format')}</label>
          <Select
            id="cf-format"
            value={format}
            onChange={(f) => {
              setFormat(f);
              setMethod('');
            }}
            options={WRITABLE.map((f) => [f, f.toUpperCase()])}
          />
        </div>
        <LevelField level={level} onChange={setLevel} />
      </div>
      <details className="adv" open={moreOpen} onToggle={(e) => setMoreOpen((e.target as HTMLDetailsElement).open)}>
        <summary>
          <Icon name="chevronRight" />
          {t('compress.advanced')}
          {/* Once set, encryption and volumes stay visible while the section is collapsed. */}
          <span className="hint">{activeAdvanced.length > 0 ? activeAdvanced.join(' · ') : t('compress.advancedHint')}</span>
        </summary>
        <div className="advbody">
          <Tabs id="cf-tab" value={tab} onChange={setAdvTab} label={t('compress.advanced')} options={tabs.map((id) => [id, t(`compress.tabs.${id}`), changed[id]])} />
          {/* The panels share one cell: the section is as tall as the tallest, so switching tabs
              does not resize the window. */}
          <div className="tabpanels">
            <div {...panel('encrypt')}>
              <div className="field">
                <span className="flabel">{t('compress.password')}</span>
                {canEncrypt ? (
                  <>
                    <div className="inline">
                      <PasswordInput value={password} onChange={setPassword} label={t('compress.password')} />
                      <span className="hint">{t('compress.strength')}</span>
                      <span className="strength" aria-hidden="true">
                        {[0, 1, 2, 3].map((i) => (
                          <i key={i} className={i < strength ? `on${strength <= 2 ? ' w' : ''}` : ''} />
                        ))}
                      </span>
                    </div>
                    {format === '7z' && (
                      <label className="chk">
                        <input type="checkbox" checked={encryptNames} onChange={(e) => setEncryptNames(e.target.checked)} /> {t('compress.encryptNames')}
                      </label>
                    )}
                  </>
                ) : (
                  <div className="hint">{t('compress.noEncryption', { format: format.toUpperCase() })}</div>
                )}
              </div>
            </div>
            <div {...panel('volume')}>
              <div className="frow">
                <label htmlFor="cf-vol">{t('compress.volume')}</label>
                <div className="inline">
                  <Select id="cf-vol" value={volume} onChange={setVolume} options={(['none', 'fat32', 'dvd', 'email', 'custom'] as const).map((v) => [v, t(`compress.volumes.${v}`)])} />
                  {volume === 'custom' && (
                    <input className="inp" type="number" min={1} style={{ width: 100, flex: 'none' }} value={customMb} aria-label={t('compress.customSize')} onChange={(e) => setCustomMb(Math.max(1, Number(e.target.value)))} />
                  )}
                </div>
              </div>
            </div>
            <div {...panel('method')}>
              {methods.length > 0 && (
                <div className="frow">
                  <label htmlFor="cf-method">{t('compress.method')}</label>
                  <Select id="cf-method" value={method} onChange={setMethod} options={[['', t('compress.auto')], ...methods.map((m): [string, string] => [m, m])]} />
                </div>
              )}
              {format === '7z' && (
                <label className="chk">
                  <input type="checkbox" checked={solid} onChange={(e) => setSolid(e.target.checked)} /> {t('compress.solid')}
                </label>
              )}
              {canSkip && (
                <div className="field">
                  <label className="chk">
                    <input type="checkbox" checked={skipCompressed} onChange={(e) => setSkipCompressed(e.target.checked)} /> {t('compress.skipCompressed')}
                  </label>
                  <span className="hint">{t('compress.skipCompressedHint')}</span>
                </div>
              )}
            </div>
            <div {...panel('other')}>
              {format === 'zip' && (
                <div className="field">
                  <label htmlFor="cf-comment">{t('compress.comment')}</label>
                  <input id="cf-comment" className="inp" value={comment} onChange={(e) => setComment(e.target.value)} />
                </div>
              )}
              {!draft && (
                <div className="field">
                  <label htmlFor="cf-exclude">{t('compress.exclude')}</label>
                  <textarea id="cf-exclude" className="inp mono" rows={3} value={exclude} placeholder={'node_modules\n*.log'} onChange={(e) => setExclude(e.target.value)} />
                </div>
              )}
            </div>
          </div>
        </div>
      </details>
      <div className="field" style={{ gap: 8 }}>
        <label className="chk"><input type="checkbox" checked={verify} onChange={(e) => setVerify(e.target.checked)} /> {t('compress.verify')}</label>
        <label className="chk"><input type="checkbox" checked={openAfter} onChange={(e) => setOpenAfter(e.target.checked)} /> {t('compress.openAfter')}</label>
        {!draft && <label className="chk"><input type="checkbox" checked={deleteSource} onChange={(e) => setDeleteSource(e.target.checked)} /> {t('compress.deleteSource')}</label>}
      </div>
    </Modal>
  );
}
