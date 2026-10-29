// Dialogs (docs/04-ui-design.md §4.2–§4.10). The host renders the top-most open dialog.

import { open as pick, save } from '@tauri-apps/plugin-dialog';
import { type ReactNode, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { FileIcon, Icon } from '../../components/Icon';
import { Modal, PasswordInput, Segmented } from '../../components/ui';
import { errorText } from '../../i18n';
import { api, asError } from '../../lib/api';
import { archiveStem, basename, dirname, joinPath } from '../../lib/format';
import { useFmt } from '../../lib/hooks';
import type { FormatId, Level, MountProbe, NodeInfo, OverwriteMode, PathStats, Preview } from '../../lib/types';
import { type Dialog, mountRoot, useApp } from '../../store/app';

const LEVELS: Level[] = ['store', 'fastest', 'fast', 'normal', 'better', 'best', 'ultra'];
/** Rough output size ratio and throughput (MB/s) per level, for the estimate line. */
const LEVEL_RATIO = [1, 0.72, 0.6, 0.5, 0.44, 0.4, 0.37];
const LEVEL_SPEED = [900, 320, 180, 110, 70, 40, 22];

function useClose(d: Dialog) {
  const close = useApp((s) => s.closeDialog);
  return () => close(d.id);
}

/**
 * A standalone dialog window (file-manager "Extract to…" / "Add to archive…") turns into the
 * job's progress window. Closing the dialog and recording the job is one store update, so the
 * window never sees "no dialog and no job" (which closes it) or the dialog over the progress.
 */
function startedInShell(dialogId: number, jobId: number) {
  useApp.setState((s) => ({ shellJob: jobId, dialogs: s.dialogs.filter((x) => x.id !== dialogId) }));
}

// ----------------------------------------------------------------------------------- extract

function defaultDestination(paths: string[] | undefined, archivePath: string | undefined): string {
  const s = useApp.getState();
  const mode = s.settings?.general.extract_to ?? 'ask';
  const sys = s.boot?.system;
  const base = paths?.[0] ?? archivePath ?? '';
  if (mode === 'downloads' && sys?.downloadsDir) return sys.downloadsDir;
  if (base) return dirname(base);
  return sys?.downloadsDir ?? sys?.homeDir ?? '';
}

function ExtractDialog({ d }: { d: Extract<Dialog, { kind: 'extract' }> }) {
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
          <select id="ex-paths" className="sel" value={pathMode} onChange={(e) => setPathMode(e.target.value as 'smart' | 'full' | 'flat')}>
            {(['smart', 'full', 'flat'] as const).map((m) => (
              <option key={m} value={m}>{t(`extract.${m}`)}</option>
            ))}
          </select>
        </div>
        <div className="field">
          <label htmlFor="ex-ow">{t('extract.overwrite')}</label>
          <select id="ex-ow" className="sel" value={overwrite} onChange={(e) => setOverwrite(e.target.value as OverwriteMode)}>
            {(['ask', 'skip', 'overwrite', 'overwrite-older', 'rename'] as OverwriteMode[]).map((m) => (
              <option key={m} value={m}>{t(`extract.overwriteModes.${m}`)}</option>
            ))}
          </select>
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
              <select id="ex-enc" className="sel" value={encoding} onChange={(e) => setEncoding(e.target.value)}>
                <option value="auto">{t('settings.language.encodingAuto')}</option>
                {encodings.map((x) => (
                  <option key={x} value={x}>{x}</option>
                ))}
              </select>
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

// ---------------------------------------------------------------------------------- compress

/** The compression level slider with its labels; `children` go below it. */
function LevelField({ level, onChange, children }: { level: number; onChange: (level: number) => void; children?: ReactNode }) {
  const { t } = useTranslation();
  return (
    <div className="field">
      <label htmlFor="cf-level">
        {t('compress.level')} · <b style={{ color: 'var(--fg-primary)' }}>{t(`compress.levels.${LEVELS[level]}`)}</b>
      </label>
      <input id="cf-level" type="range" min={0} max={6} value={level} onChange={(e) => onChange(Number(e.target.value))} />
      <div className="levels">
        {LEVELS.map((l) => (
          <span key={l}>{t(`compress.levels.${l}`)}</span>
        ))}
      </div>
      {children}
    </div>
  );
}

/** Tabs of the Advanced section of the compress dialog. */
type AdvTab = 'encrypt' | 'volume' | 'method' | 'other';

const WRITABLE: FormatId[] = ['zip', '7z', 'tar.zst', 'tar.gz', 'tar.xz', 'tar.bz2', 'tar', 'gz', 'xz', 'zst', 'bz2'];

function CompressSheet({ d }: { d: Extract<Dialog, { kind: 'compress' }> }) {
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
  const tabs: AdvTab[] = [
    'encrypt',
    'volume',
    ...(methods.length > 0 || format === '7z' ? (['method'] as const) : []),
    ...(!draft || format === 'zip' ? (['other'] as const) : []),
  ];
  const tab = tabs.includes(advTab) ? advTab : 'encrypt';
  const panel = (id: AdvTab) => ({ className: `tabpanel${tab === id ? '' : ' off'}`, inert: tab !== id });
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
      sheet={!shell}
      width={shell ? 600 : undefined}
      footer={
        <>
          <button type="button" className="btn" onClick={close}>{t('common.cancel')}</button>
          <button type="button" className="btn primary" disabled={!dest || tooMany || !stats} onClick={() => void start()}>
            <Icon name="package" />
            <span>{t('compress.start')}</span>
          </button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="cf-name">{t('compress.saveTo')}</label>
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
        <div className="hint mono ellipsis" title={dest}>{dest}</div>
        {stats && <div className="hint">{t('compress.sources', { count: stats.files, size: fmt.size(stats.bytes) })}</div>}
        {exists && <div className="note warn"><Icon name="alert" /><span>{t('compress.exists', { name: stem + ext })}</span></div>}
        {tooMany && <div className="note warn"><Icon name="alert" /><span>{t('compress.singleFile', { format: format.toUpperCase() })}</span></div>}
      </div>
      <div className="fmt-level">
        <div className="field">
          <label htmlFor="cf-format">{t('compress.format')}</label>
          <select
            id="cf-format"
            className="sel"
            value={format}
            onChange={(e) => {
              setFormat(e.target.value as FormatId);
              setMethod('');
            }}
          >
            {WRITABLE.map((f) => (
              <option key={f} value={f}>{f.toUpperCase()}</option>
            ))}
          </select>
        </div>
        <LevelField level={level} onChange={setLevel}>
          {bytes > 0 && (
            <div className="est">
              {t('compress.estimate', { from: fmt.size(bytes), to: fmt.size(out) })} · {fmt.duration(secs)}
            </div>
          )}
        </LevelField>
      </div>
      <details className="adv" open={moreOpen} onToggle={(e) => setMoreOpen((e.target as HTMLDetailsElement).open)}>
        <summary>
          <Icon name="chevronRight" />
          {t('compress.advanced')}
          {/* Once set, encryption and volumes stay visible while the section is collapsed. */}
          <span className="hint">{activeAdvanced.length > 0 ? activeAdvanced.join(' · ') : t('compress.advancedHint')}</span>
        </summary>
        <div className="advbody">
          <Segmented value={tab} onChange={setAdvTab} label={t('compress.advanced')} options={tabs.map((id) => [id, t(`compress.tabs.${id}`)])} />
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
                  <select id="cf-vol" className="sel" value={volume} onChange={(e) => setVolume(e.target.value as typeof volume)}>
                    {(['none', 'fat32', 'dvd', 'email', 'custom'] as const).map((v) => (
                      <option key={v} value={v}>{t(`compress.volumes.${v}`)}</option>
                    ))}
                  </select>
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
                  <select id="cf-method" className="sel" value={method} onChange={(e) => setMethod(e.target.value)}>
                    <option value="">{t('compress.auto')}</option>
                    {methods.map((m) => (
                      <option key={m} value={m}>{m}</option>
                    ))}
                  </select>
                </div>
              )}
              {format === '7z' && (
                <label className="chk">
                  <input type="checkbox" checked={solid} onChange={(e) => setSolid(e.target.checked)} /> {t('compress.solid')}
                </label>
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

// ----------------------------------------------------------------------------------- convert

/** Formats an archive can be converted to: those that hold several entries. */
const CONVERT_TO: FormatId[] = ['zip', '7z', 'tar.zst', 'tar.gz', 'tar.xz', 'tar.bz2', 'tar'];

/** Format of an archive file by its name, among the conversion targets. */
const formatOf = (path: string) => CONVERT_TO.find((f) => basename(path).toLowerCase().endsWith(`.${f}`));

const samePath = (a: string, b: string) =>
  document.documentElement.dataset.platform === 'win' ? a.toLowerCase() === b.toLowerCase() : a === b;

/**
 * "Convert to…" (docs/04-ui-design.md §4.11): archives on disk written again in another format,
 * each as `<name>.<format>` in the chosen folder; a name that would replace one of the archives
 * being converted, or another new one, gets a number. A password a source needs is asked for by
 * the job.
 */
function ConvertDialog({ d }: { d: Extract<Dialog, { kind: 'convert' }> }) {
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
          <select id="cv-format" className="sel" value={format} onChange={(e) => setFormat(e.target.value as FormatId)}>
            {CONVERT_TO.map((f) => (
              <option key={f} value={f}>{f.toUpperCase()}</option>
            ))}
          </select>
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

// --------------------------------------------------------------------------------- passwords

function OpenPasswordDialog({ d }: { d: Extract<Dialog, { kind: 'openPassword' }> }) {
  const { t } = useTranslation();
  const close = useClose(d);
  const [pw, setPw] = useState('');
  const [error, setError] = useState(d.wrong ? t('pwd.wrong') : '');
  const submit = async () => {
    if (!pw) return setError(t('pwd.empty'));
    close();
    await useApp.getState().openArchive(d.path, pw);
  };
  return (
    <Modal
      title={t('pwd.title')}
      onClose={close}
      width={420}
      footer={
        <>
          <button type="button" className="btn" onClick={close}>{t('common.cancel')}</button>
          <button type="button" className="btn primary" onClick={() => void submit()}>{t('common.ok')}</button>
        </>
      }
    >
      <p className="hint" style={{ margin: 0 }}>{t('pwd.desc', { name: basename(d.path) })}</p>
      <PasswordInput value={pw} onChange={setPw} autoFocus onEnter={() => void submit()} label={t('pwd.label')} />
      {error && <div className="err"><Icon name="alert" /><span>{error}</span></div>}
    </Modal>
  );
}

function JobPasswordDialog({ d }: { d: Extract<Dialog, { kind: 'jobPassword' }> }) {
  const { t } = useTranslation();
  const closeDialog = useApp((s) => s.closeDialog);
  const [pw, setPw] = useState('');
  const answer = (value: string | null) => {
    closeDialog(d.id);
    void api.answer(d.jobId, d.promptId, { kind: 'password', value });
  };
  return (
    <Modal
      title={t('pwd.title')}
      onClose={() => answer(null)}
      width={420}
      sticky
      footer={
        <>
          <button type="button" className="btn" onClick={() => answer(null)}>{t('common.cancel')}</button>
          <button type="button" className="btn primary" disabled={!pw} onClick={() => answer(pw)}>{t('common.ok')}</button>
        </>
      }
    >
      <p className="hint" style={{ margin: 0 }}>{t('pwd.desc', { name: d.archive })}</p>
      <PasswordInput value={pw} onChange={setPw} autoFocus onEnter={() => pw && answer(pw)} label={t('pwd.label')} />
      {d.attempt > 0 && <div className="err"><Icon name="alert" /><span>{t('pwd.wrong')}</span></div>}
    </Modal>
  );
}

// --------------------------------------------------------------------------------- conflicts

function ConflictDialog({ d }: { d: Extract<Dialog, { kind: 'conflict' }> }) {
  const { t } = useTranslation();
  const fmt = useFmt();
  const closeDialog = useApp((s) => s.closeDialog);
  const [all, setAll] = useState(true);
  const c = d.data as { path: string; existingSize: number; existingMtime: number | null; incomingSize: number; incomingMtime: number | null };
  const name = basename(c.path);
  const newer = (c.incomingMtime ?? 0) > (c.existingMtime ?? 0);
  const answer = (choice: 'overwrite' | 'skip' | 'rename' | 'overwrite-older' | 'abort') => {
    closeDialog(d.id);
    void api.answer(d.jobId, d.promptId, { kind: 'conflict', choice, applyToAll: all });
  };
  return (
    <Modal
      title={t('conflict.title', { name })}
      onClose={() => answer('abort')}
      width={580}
      sticky
      footer={
        <>
          <button type="button" className="btn danger" onClick={() => answer('abort')}>{t('conflict.abort')}</button>
          <span className="grow" />
          <button type="button" className="btn" onClick={() => answer('skip')}>{t('conflict.skip')}</button>
          <button type="button" className="btn" onClick={() => answer('rename')}>{t('conflict.keepBoth')}</button>
          <button type="button" className="btn" onClick={() => answer('overwrite-older')}>{t('conflict.older')}</button>
          <button type="button" className="btn primary" onClick={() => answer('overwrite')}>{t('conflict.overwrite')}</button>
        </>
      }
    >
      <div className="conf">
        <div className="side">
          <h4>{t('conflict.existing')}</h4>
          <FileIcon name={name} isDir={false} />
          <b title={c.path}>{name}</b>
          <span>{fmt.size(c.existingSize)}</span>
          <span>{fmt.date(c.existingMtime)}</span>
        </div>
        <div className="side">
          <h4>{t('conflict.incoming')}</h4>
          <FileIcon name={name} isDir={false} />
          <b>{name}</b>
          <span>{fmt.size(c.incomingSize)}</span>
          <span>
            {fmt.date(c.incomingMtime)} {newer && <span className="new">· {t('conflict.newer')}</span>}
          </span>
        </div>
      </div>
      <label className="chk">
        <input type="checkbox" checked={all} onChange={(e) => setAll(e.target.checked)} /> {t('conflict.applyAll')}
      </label>
    </Modal>
  );
}

function ConfirmDialog({ d }: { d: Extract<Dialog, { kind: 'confirm' }> }) {
  const { t } = useTranslation();
  const fmt = useFmt();
  const closeDialog = useApp((s) => s.closeDialog);
  const job = useApp((s) => s.jobs[d.jobId]);
  const data = d.data as { ratio?: number; unpacked?: number };
  const answer = (yes: boolean) => {
    closeDialog(d.id);
    void api.answer(d.jobId, d.promptId, { kind: 'confirm', yes });
  };
  return (
    <Modal
      title={t('confirm.highRatioTitle')}
      onClose={() => answer(false)}
      width={480}
      sticky
      footer={
        <>
          <button type="button" className="btn" onClick={() => answer(true)}>{t('confirm.continue')}</button>
          <button type="button" className="btn primary" onClick={() => answer(false)}>{t('confirm.stop')}</button>
        </>
      }
    >
      <div className="note warn">
        <Icon name="alert" />
        <span>{t('confirm.highRatio', { name: job?.title ?? '', ratio: data.ratio ?? 0, size: fmt.size(data.unpacked ?? 0) })}</span>
      </div>
    </Modal>
  );
}

function AskDialog({ d }: { d: Extract<Dialog, { kind: 'ask' }> }) {
  const { t } = useTranslation();
  const closeDialog = useClose(d);
  const cancel = () => {
    closeDialog();
    d.onCancel?.();
  };
  return (
    <Modal
      title={d.title}
      onClose={cancel}
      width={440}
      footer={
        <>
          <button type="button" className="btn" onClick={cancel}>{t('common.cancel')}</button>
          {d.alternative && (
            <button
              type="button"
              className="btn"
              style={d.alternative.danger ? { color: 'var(--danger)' } : undefined}
              onClick={() => {
                closeDialog();
                d.alternative?.onClick();
              }}
            >
              {d.alternative.label}
            </button>
          )}
          <button
            type="button"
            className={`btn primary`}
            style={d.danger ? { background: 'var(--danger)' } : undefined}
            onClick={() => {
              closeDialog();
              d.onConfirm();
            }}
          >
            {d.confirm}
          </button>
        </>
      }
    >
      <p style={{ margin: 0 }}>{d.message}</p>
    </Modal>
  );
}

// ------------------------------------------------------------------------- props / preview

function PropsDialog({ d }: { d: Extract<Dialog, { kind: 'props' }> }) {
  const { t } = useTranslation();
  const fmt = useFmt();
  const close = useClose(d);
  const archive = useApp((s) => s.archive)!;
  const [info, setInfo] = useState<NodeInfo | null>(null);
  useEffect(() => {
    void api.node(archive.id, d.node).then(setInfo);
  }, [archive.id, d.node]);
  const yes = (b: boolean) => t(b ? 'common.yes' : 'common.no');
  return (
    <Modal title={t('props.title')} onClose={close} width={480} footer={<button type="button" className="btn primary" onClick={close}>{t('common.ok')}</button>}>
      {info && (
        <>
          <div className="inline" style={{ gap: 12, flexWrap: 'nowrap' }}>
            <FileIcon name={info.name} isDir={info.isDir} kind={info.kind} />
            <b className="ellipsis" style={{ fontSize: '1.05rem' }}>{info.name}</b>
          </div>
          <dl className="dl">
            <dt>{t('props.path')}</dt>
            <dd className="mono" title={info.path}>{info.path || '/'}</dd>
            <dt>{t('props.size')}</dt>
            <dd>{fmt.size(info.size)}</dd>
            <dt>{t('props.packed')}</dt>
            <dd>{info.packed ? fmt.size(info.packed) : '—'}</dd>
            <dt>{t('props.ratio')}</dt>
            <dd>{info.packed ? fmt.percent(info.packed, info.size) : '—'}</dd>
            {info.isDir ? (
              <>
                <dt>{t('props.items')}</dt>
                <dd>{info.files}</dd>
              </>
            ) : (
              <>
                <dt>{t('props.crc')}</dt>
                <dd className="mono">{info.crc32 != null ? info.crc32.toString(16).toUpperCase().padStart(8, '0') : '—'}</dd>
              </>
            )}
            <dt>{t('props.method')}</dt>
            <dd>{info.method ?? '—'}</dd>
            <dt>{t('props.mtime')}</dt>
            <dd>{fmt.date(info.mtime) || '—'}</dd>
            <dt>{t('props.encrypted')}</dt>
            <dd>{yes(info.encrypted)}</dd>
            {info.linkTarget && (
              <>
                <dt>{t('props.linkTarget')}</dt>
                <dd className="mono">{info.linkTarget}</dd>
              </>
            )}
            {info.comment && (
              <>
                <dt>{t('props.comment')}</dt>
                <dd>{info.comment}</dd>
              </>
            )}
          </dl>
        </>
      )}
    </Modal>
  );
}

function PreviewDialog({ d }: { d: Extract<Dialog, { kind: 'preview' }> }) {
  const { t } = useTranslation();
  const fmt = useFmt();
  const close = useClose(d);
  const archive = useApp((s) => s.archive)!;
  const [preview, setPreview] = useState<Preview | null>(null);
  const [info, setInfo] = useState<NodeInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    void api.node(archive.id, d.node).then(setInfo);
    api
      .preview(archive.id, d.node)
      .then(setPreview)
      .catch((e) => setError(fmt.error(e)));
  }, [archive.id, d.node, fmt]);
  const openWith = () => void api.openExternal(archive.id, d.node).catch((e) => useApp.getState().reportError(e));
  return (
    <Modal
      title={`${t('preview.title')} · ${info?.name ?? ''}`}
      onClose={close}
      width={760}
      footer={
        <>
          <button type="button" className="btn" onClick={openWith}>
            <Icon name="external" />
            <span>{t('preview.openWith')}</span>
          </button>
          <span className="grow" />
          <button type="button" className="btn primary" onClick={close}>{t('common.close')}</button>
        </>
      }
    >
      {error && <div className="note danger"><Icon name="alert" /><span>{error}</span></div>}
      {!preview && !error && <div className="hint">{t('common.loading')}</div>}
      {preview?.kind === 'text' && (
        <>
          <pre className="code">{preview.text}</pre>
          <div className="prev-meta">
            <span>{t('preview.encoding', { encoding: preview.encoding })}</span>
            {info && <span>{fmt.size(info.size)}</span>}
            {preview.truncated && <span>{t('preview.truncated')}</span>}
          </div>
        </>
      )}
      {preview?.kind === 'image' && (
        <>
          <div className="imgprev">
            <img src={preview.dataUrl} alt={info?.name ?? ''} />
          </div>
          <div className="prev-meta">
            <span>{t('preview.dims', { width: preview.width, height: preview.height })}</span>
            {info && <span>{fmt.size(info.size)}</span>}
            {info && <span>{fmt.date(info.mtime)}</span>}
          </div>
        </>
      )}
      {preview?.kind === 'binary' && (
        <>
          <div className="hint">{t('preview.binary')}</div>
          <pre className="code">{preview.hex}</pre>
        </>
      )}
      {preview?.kind === 'unsupported' && (
        <div className="emptyst" style={{ minHeight: 160 }}>
          <Icon name="file" />
          <div>{t('preview.unsupported')}</div>
        </div>
      )}
    </Modal>
  );
}

function RenameDialog({ d }: { d: Extract<Dialog, { kind: 'rename' }> }) {
  const { t } = useTranslation();
  const close = useClose(d);
  const archive = useApp((s) => s.archive)!;
  const [name, setName] = useState(d.name);
  const invalid = /[\\/]/.test(name);
  const submit = () => {
    if (!name.trim() || invalid || (!d.mkdir && name === d.name)) return close();
    close();
    const done = d.mkdir ? api.mkdir(archive.id, d.node, name.trim()) : api.rename(archive.id, d.node, name.trim());
    void done.catch((e) => useApp.getState().reportError(e));
  };
  return (
    <Modal
      title={d.mkdir ? t('draft.newFolder') : t('rename.title')}
      onClose={close}
      width={420}
      footer={
        <>
          <button type="button" className="btn" onClick={close}>{t('common.cancel')}</button>
          <button type="button" className="btn primary" disabled={invalid} onClick={submit}>{t('common.ok')}</button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="rn">{d.mkdir ? t('draft.folderName') : t('rename.label')}</label>
        <input
          id="rn"
          className="inp"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onFocus={(e) => {
            const dot = d.mkdir ? -1 : d.name.lastIndexOf('.');
            e.target.setSelectionRange(0, dot > 0 ? dot : d.name.length);
          }}
          onKeyDown={(e) => {
            if (e.key !== 'Enter') return;
            // Also swallows the key press, which would otherwise click the button that
            // opened this dialog once focus returns to it (and open the dialog again).
            e.preventDefault();
            submit();
          }}
        />
        {invalid && <div className="err"><Icon name="alert" /><span>{t('rename.invalid')}</span></div>}
      </div>
    </Modal>
  );
}

// ------------------------------------------------------------------------------------- mount

/** Mount options (docs/04-ui-design.md §4.10). Mounting opens the volume in the file manager. */
function MountDialog({ d }: { d: Extract<Dialog, { kind: 'mount' }> }) {
  const { t } = useTranslation();
  const close = useClose(d);
  const archive = useApp((s) => s.archive)!;
  const settings = useApp((s) => s.settings)!;
  const mounted = useApp((s) => s.mounts.find((m) => samePath(m.archive, archive.path)));
  const [probe, setProbe] = useState<MountProbe | null>(null);
  const [letters, setLetters] = useState<string[]>([]);
  const [as, setAs] = useState<'letter' | 'folder'>('letter');
  // '' picks the next free letter when mounting.
  const [letter, setLetter] = useState('');
  const [folder, setFolder] = useState('');
  const [label, setLabel] = useState('');
  const [warm, setWarm] = useState(settings.mount.warm_solid);
  const [writable, setWritable] = useState(settings.mount.writable);
  // Shown once mounting reports that a password is needed.
  const [password, setPassword] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    void api.mountProbe().then(setProbe);
    void api.freeLetters().then(setLetters);
  }, []);
  const ready = !!probe && probe.built && probe.health === 'ready';
  const solid = archive.mountClass === 'C';
  const windows = useApp((s) => s.boot?.system.platform === 'win');

  const start = async (elevated = false) => {
    if (password !== null && !password) return setError(t('pwd.empty'));
    setBusy(true);
    setError('');
    try {
      const m = await api.mount({
        path: archive.path,
        archiveId: archive.id,
        mountPoint: as === 'folder' ? folder : letter || null,
        label: label.trim() || null,
        warm: solid && warm,
        password,
        elevated,
        writable,
      });
      if (password) void api.setArchivePassword(archive.id, password);
      close();
      const s = useApp.getState();
      await s.refreshMounts();
      void api.reveal(mountRoot(m));
      s.toast({ tone: 'success', text: t('mount.mounted', { point: m.mountPoint }), action: { label: t('mount.open'), run: () => void api.reveal(mountRoot(m)) } });
    } catch (e) {
      const err = asError(e);
      if (err.key === 'cancelled') {
        // The administrator prompt was declined.
      } else if (err.key === 'password-required' || err.key === 'wrong-password') {
        setPassword((p) => p ?? '');
        if (err.key === 'wrong-password') setError(t('pwd.wrong'));
      } else if (err.key === 'already-exists') {
        setError(t('mount.errInUse', { point: err.params.path ?? '' }));
        void api.freeLetters().then(setLetters);
      } else if (err.key === 'missing-dependency') {
        setError(t('mount.noBackend'));
      } else {
        const detail = err.params.detail;
        setError(detail ? `${errorText(t, err)} (${detail})` : errorText(t, err));
      }
    } finally {
      setBusy(false);
    }
  };

  if (mounted) {
    return (
      <Modal
        title={t('mount.title', { name: archive.name })}
        onClose={close}
        width={520}
        footer={
          <>
            <button
              type="button"
              className="btn"
              onClick={() => {
                close();
                void useApp.getState().unmount(mounted.id);
              }}
            >
              <Icon name="eject" />
              <span>{t('mount.eject')}</span>
            </button>
            <button
              type="button"
              className="btn primary"
              onClick={() => {
                close();
                void api.reveal(mountRoot(mounted));
              }}
            >
              <Icon name="folderOpen" />
              <span>{t('mount.open')}</span>
            </button>
          </>
        }
      >
        <div className="note">
          <Icon name="drive" />
          <span>{t('mount.already', { point: mounted.mountPoint })}</span>
        </div>
      </Modal>
    );
  }

  return (
    <Modal
      title={t('mount.title', { name: archive.name })}
      onClose={close}
      width={540}
      footer={
        <>
          <button type="button" className="btn" onClick={close}>{t('common.cancel')}</button>
          {windows && (
            <button
              type="button"
              className="btn"
              disabled={!ready || busy || as === 'folder'}
              title={as === 'folder' ? t('mount.elevatedDriveOnly') : t('mount.elevatedHint')}
              onClick={() => void start(true)}
            >
              <Icon name="shield" />
              <span>{t('mount.startElevated')}</span>
            </button>
          )}
          <button type="button" className="btn primary" disabled={!ready || busy || (as === 'folder' && !folder.trim())} onClick={() => void start()}>
            <Icon name="drive" />
            <span>{t('mount.start')}</span>
          </button>
        </>
      }
    >
      <div className="frow">
        <label>{t('mount.as')}</label>
        <div className="inline">
          <Segmented
            value={as}
            label={t('mount.as')}
            options={[
              ['letter', t('mount.letter')],
              ['folder', t('mount.folder')],
            ]}
            onChange={setAs}
          />
          {as === 'letter' && (
            <select className="sel" style={{ flex: 'none', minWidth: 0, width: 150 }} value={letter} aria-label={t('mount.letter')} onChange={(e) => setLetter(e.target.value)}>
              <option value="">{letters[0] ? t('mount.auto', { letter: letters[0] }) : t('mount.autoNone')}</option>
              {letters.map((l) => (
                <option key={l} value={l}>{l}</option>
              ))}
            </select>
          )}
        </div>
      </div>
      {as === 'folder' && (
        <div className="frow">
          <span />
          <div className="field">
            <div className="inline">
              <input className="inp mono grow" value={folder} aria-label={t('mount.folder')} onChange={(e) => setFolder(e.target.value)} />
              <button
                type="button"
                className="btn outline"
                onClick={async () => {
                  const p = await pick({ directory: true, multiple: false, defaultPath: folder || dirname(archive.path) });
                  if (typeof p === 'string') setFolder(p);
                }}
              >
                {t('common.browse')}
              </button>
            </div>
            <span className="hint">{t('mount.folderHint')}</span>
          </div>
        </div>
      )}
      <div className="frow">
        <label htmlFor="mt-label">{t('mount.label')}</label>
        <input id="mt-label" className="inp" value={label} placeholder={archiveStem(archive.name)} maxLength={32} onChange={(e) => setLabel(e.target.value)} />
      </div>
      {password !== null && (
        <div className="frow">
          <label>{t('mount.password')}</label>
          <PasswordInput value={password} onChange={setPassword} autoFocus onEnter={() => void start()} label={t('mount.password')} />
        </div>
      )}
      <div className="field">
        <label className="chk">
          <input type="checkbox" checked={writable} onChange={(e) => setWritable(e.target.checked)} /> {t('mount.writable')}
        </label>
        <span className="hint">{archive.info.format === 'rar' ? t('mount.writableHintNoSave') : t('mount.writableHint')}</span>
      </div>
      {solid && (
        <label className="chk">
          <input type="checkbox" checked={warm} onChange={(e) => setWarm(e.target.checked)} /> {t('mount.warm')}
        </label>
      )}
      {probe && !ready ? (
        <div className="note warn">
          <Icon name="alert" />
          <span>{probe.built ? t('mount.noBackend') : t('mount.unsupported')}</span>
        </div>
      ) : (
        <div className={`note${solid ? ' warn' : ''}`}>
          <Icon name={solid ? 'alert' : 'info'} />
          <span>{password !== null ? t('mount.passwordHint') : t(`mount.class.${archive.mountClass}`, { format: archive.info.format.toUpperCase() })}</span>
        </div>
      )}
      {error && <div className="err"><Icon name="alert" /><span>{error}</span></div>}
    </Modal>
  );
}

export function DialogHost() {
  const dialogs = useApp((s) => s.dialogs);
  const d = dialogs[dialogs.length - 1];
  if (!d) return null;
  switch (d.kind) {
    case 'extract':
      return <ExtractDialog key={d.id} d={d} />;
    case 'compress':
      return <CompressSheet key={d.id} d={d} />;
    case 'convert':
      return <ConvertDialog key={d.id} d={d} />;
    case 'openPassword':
      return <OpenPasswordDialog key={d.id} d={d} />;
    case 'jobPassword':
      return <JobPasswordDialog key={d.id} d={d} />;
    case 'conflict':
      return <ConflictDialog key={d.id} d={d} />;
    case 'confirm':
      return <ConfirmDialog key={d.id} d={d} />;
    case 'ask':
      return <AskDialog key={d.id} d={d} />;
    case 'props':
      return <PropsDialog key={d.id} d={d} />;
    case 'preview':
      return <PreviewDialog key={d.id} d={d} />;
    case 'rename':
      return <RenameDialog key={d.id} d={d} />;
    case 'mount':
      return <MountDialog key={d.id} d={d} />;
    default:
      return null;
  }
}
