// Mount (docs/12-mount.md §9).

import { open as pick } from '@tauri-apps/plugin-dialog';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Icon } from '../../components/Icon';
import { Modal, PasswordInput, Segmented, Select } from '../../components/ui';
import { errorText } from '../../i18n';
import { api, asError } from '../../lib/api';
import { archiveStem, basename, dirname, shortLabel } from '../../lib/format';
import type { MountInfo, MountProbe, MountTarget } from '../../lib/types';
import { type Dialog, mountRoot, useApp } from '../../store/app';
import { samePath, startedInShell, useClose } from './shared';

/**
 * Mount options (docs/04-ui-design.md §4.10) for the archive shown, or for an archive on disk
 * (the dialog window of the file manager's "Mount"), whose format is looked up first.
 */
export function MountDialog({ d }: { d: Extract<Dialog, { kind: 'mount' }> }) {
  const { t } = useTranslation();
  const close = useClose(d);
  const [target, setTarget] = useState<MountTarget | null>(() => {
    const a = useApp.getState().archive;
    return d.path || !a
      ? null
      : { path: a.path, name: a.name, format: a.info.format, mountClass: a.mountClass, encrypted: a.info.encrypted, archiveId: a.id };
  });
  const [error, setError] = useState('');
  useEffect(() => {
    if (!d.path) return;
    // The mounts are fetched too, so an archive mounted already shows as such from the start.
    void Promise.all([api.mountTarget(d.path), useApp.getState().refreshMounts()]).then(
      ([x]) => setTarget(x),
      (e) => setError(errorText(t, asError(e))),
    );
  }, [d.path, t]);
  if (target) return <MountForm d={d} target={target} />;
  // Nothing until the archive is known: a dialog window shows itself once it fits its dialog.
  if (!error) return null;
  return (
    <Modal
      title={t('mount.title', { name: basename(d.path ?? '') })}
      onClose={close}
      width={540}
      footer={<button type="button" className="btn primary" onClick={close}>{t('common.close')}</button>}
    >
      <div className="err"><Icon name="alert" /><span>{error}</span></div>
    </Modal>
  );
}

/** Mounting opens the volume in the file manager. */
function MountForm({ d, target }: { d: Extract<Dialog, { kind: 'mount' }>; target: MountTarget }) {
  const { t } = useTranslation();
  const close = useClose(d);
  const settings = useApp((s) => s.settings)!;
  const shell = useApp((s) => s.shellLabel);
  const live = useApp((s) => s.mounts.find((m) => samePath(m.archive, target.path)));
  // While it is being ejected from a dialog window, the mount stays on screen until that is done.
  const [ejecting, setEjecting] = useState<MountInfo | null>(null);
  const mounted = ejecting ?? live;
  const [probe, setProbe] = useState<MountProbe | null>(null);
  const [letters, setLetters] = useState<string[]>([]);
  const [as, setAs] = useState<'letter' | 'folder'>('letter');
  // '' picks the next free letter when mounting.
  const [letter, setLetter] = useState('');
  const [folder, setFolder] = useState('');
  const [label, setLabel] = useState('');
  const [warm, setWarm] = useState(settings.mount.warm_solid);
  const [writable, setWritable] = useState(settings.mount.writable);
  // Asked up front for an encrypted archive that is not open (an open one may know its password
  // already), otherwise once mounting reports that a password is needed.
  const [password, setPassword] = useState<string | null>(target.encrypted && target.archiveId == null ? '' : null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    void api.mountProbe().then(setProbe);
    void api.freeLetters().then(setLetters);
  }, []);
  const ready = !!probe && probe.built && probe.health === 'ready';
  const solid = target.mountClass === 'C';
  const windows = useApp((s) => s.boot?.system.platform === 'win');
  /** What is missing to mount: WinFsp, fuse3, or (on macOS) the system's NFS client. */
  const missing = () =>
    probe?.kind === 'win-fsp' || (!probe && windows)
      ? t('mount.noBackend')
      : probe?.kind === 'nfs-loopback'
        ? t('settings.mount.missingDriver', { name: 'mount_nfs' })
        : t('mount.noBackendFuse');

  /** Opens the volume; a dialog window closes once the file manager has it. */
  const reveal = async (m: MountInfo) => {
    if (!shell) {
      close();
      return void api.reveal(mountRoot(m));
    }
    setBusy(true);
    await api.reveal(mountRoot(m)).catch(() => undefined);
    close();
  };

  const eject = async (m: MountInfo) => {
    if (!shell) {
      close();
      return void useApp.getState().unmount(m.id);
    }
    setEjecting(m);
    const job = await useApp.getState().unmount(m.id);
    if (job) startedInShell(d.id, job);
    else if (job === 0) close();
    else setEjecting(null);
  };

  const start = async (elevated = false) => {
    if (password !== null && !password) return setError(t('pwd.empty'));
    setBusy(true);
    setError('');
    try {
      const m = await api.mount({
        path: target.path,
        archiveId: target.archiveId,
        mountPoint: as === 'folder' ? folder : letter || null,
        label: label.trim() || null,
        warm: solid && warm,
        password,
        elevated,
        writable,
      });
      if (password && target.archiveId != null) void api.setArchivePassword(target.archiveId, password);
      if (shell) return await reveal(m);
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
        setError(t(windows ? 'mount.errInUse' : 'mount.errInUseFolder', { point: err.params.path ?? '' }));
        void api.freeLetters().then(setLetters);
      } else if (err.key === 'missing-dependency') {
        setError(missing());
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
        title={t('mount.title', { name: target.name })}
        onClose={close}
        width={520}
        footer={
          <>
            <button type="button" className="btn" disabled={busy || !!ejecting} onClick={() => void eject(mounted)}>
              <Icon name="eject" />
              <span>{t('mount.eject')}</span>
            </button>
            <button type="button" className="btn primary" disabled={busy || !!ejecting} onClick={() => void reveal(mounted)}>
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
      title={t('mount.title', { name: target.name })}
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
              // Elsewhere than on Windows, a new folder named after the label.
              ['letter', windows ? t('mount.letter') : t('mount.autoNone')],
              ['folder', t('mount.folder')],
            ]}
            onChange={setAs}
          />
          {as === 'letter' && windows && (
            <Select
              style={{ flex: 'none', minWidth: 0, width: 150 }}
              value={letter}
              label={t('mount.letter')}
              onChange={setLetter}
              options={[['', letters[0] ? t('mount.auto', { letter: letters[0] }) : t('mount.autoNone')], ...letters.map((l): [string, string] => [l, l])]}
            />
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
                  const p = await pick({ directory: true, multiple: false, defaultPath: folder || dirname(target.path) });
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
        <input id="mt-label" className="inp" value={label} placeholder={shortLabel(archiveStem(target.name))} maxLength={32} onChange={(e) => setLabel(e.target.value)} />
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
        <span className="hint">{target.format === 'rar' ? t('mount.writableHintNoSave') : t('mount.writableHint')}</span>
      </div>
      {solid && (
        <label className="chk">
          <input type="checkbox" checked={warm} onChange={(e) => setWarm(e.target.checked)} /> {t('mount.warm')}
        </label>
      )}
      {probe && !ready ? (
        <div className="note warn">
          <Icon name="alert" />
          <span>{probe.built ? missing() : t('mount.unsupported')}</span>
        </div>
      ) : (
        <div className={`note${solid ? ' warn' : ''}`}>
          <Icon name={solid ? 'alert' : 'info'} />
          <span>{password !== null ? t('mount.passwordHint') : t(`mount.class.${target.mountClass}`, { format: target.format.toUpperCase() })}</span>
        </div>
      )}
      {error && <div className="err"><Icon name="alert" /><span>{error}</span></div>}
    </Modal>
  );
}
