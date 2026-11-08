// Dialogs about entries of an open archive: properties, preview, rename.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { FileIcon, Icon } from '../../components/Icon';
import { Modal } from '../../components/ui';
import { api } from '../../lib/api';
import { useFmt } from '../../lib/hooks';
import type { NodeInfo, Preview } from '../../lib/types';
import { type Dialog, useApp } from '../../store/app';
import { useClose } from './shared';

export function PropsDialog({ d }: { d: Extract<Dialog, { kind: 'props' }> }) {
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

export function PreviewDialog({ d }: { d: Extract<Dialog, { kind: 'preview' }> }) {
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

export function RenameDialog({ d }: { d: Extract<Dialog, { kind: 'rename' }> }) {
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
