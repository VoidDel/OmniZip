// Questions a job or the browser asks: passwords, conflicts, confirmations.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { FileIcon, Icon } from '../../components/Icon';
import { Modal, PasswordInput } from '../../components/ui';
import { api } from '../../lib/api';
import { basename } from '../../lib/format';
import { useFmt } from '../../lib/hooks';
import { type Dialog, useApp } from '../../store/app';
import { useClose } from './shared';

export function OpenPasswordDialog({ d }: { d: Extract<Dialog, { kind: 'openPassword' }> }) {
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

export function JobPasswordDialog({ d }: { d: Extract<Dialog, { kind: 'jobPassword' }> }) {
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

export function ConflictDialog({ d }: { d: Extract<Dialog, { kind: 'conflict' }> }) {
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

export function ConfirmDialog({ d }: { d: Extract<Dialog, { kind: 'confirm' }> }) {
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

export function AskDialog({ d }: { d: Extract<Dialog, { kind: 'ask' }> }) {
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
