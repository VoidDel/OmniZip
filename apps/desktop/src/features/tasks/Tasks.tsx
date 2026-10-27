// Task center (docs/04-ui-design.md §4.5) and the bridge from job events to the UI.

import { useEffect } from 'react';
import { useTranslation } from 'react-i18next';

import { Icon } from '../../components/Icon';
import { ProgressBar } from '../../components/ui';
import { api, events } from '../../lib/api';
import { basename } from '../../lib/format';
import { useFmt } from '../../lib/hooks';
import type { JobInfo, PromptEvent } from '../../lib/types';
import { useApp } from '../../store/app';

const shown = new Set<string>();

/** Turns a prompt event into a dialog in the window that owns the job (once per prompt). */
export function routePrompt(p: PromptEvent) {
  const key = `${p.jobId}:${p.promptId}`;
  if (shown.has(key)) return;
  shown.add(key);
  const s = useApp.getState();
  const d = p.data as Record<string, unknown>;
  if (p.kind === 'password') {
    s.openDialog({ kind: 'jobPassword', jobId: p.jobId, promptId: p.promptId, archive: String(d.archive ?? ''), attempt: Number(d.attempt ?? 0) });
  } else if (p.kind === 'conflict') {
    s.openDialog({ kind: 'conflict', jobId: p.jobId, promptId: p.promptId, data: d });
  } else {
    s.openDialog({ kind: 'confirm', jobId: p.jobId, promptId: p.promptId, data: d });
  }
}

/** Subscribes to job events for the main window. */
export function useJobEvents() {
  const { t } = useTranslation();
  const fmt = useFmt();
  useEffect(() => {
    void api.jobs().then((list) => useApp.getState().setJobs(list));
    const seen = new Map<number, string>();
    const onJob = (j: JobInfo) => {
      const s = useApp.getState();
      const before = seen.get(j.id) ?? s.jobs[j.id]?.state;
      seen.set(j.id, j.state);
      s.upsertJob(j);
      const finished = (j.state === 'done' || j.state === 'failed' || j.state === 'cancelled') && before !== j.state;
      // Mini windows report their own jobs.
      if (!finished || (j.window && j.window !== 'main')) return;
      if (j.state === 'failed' && j.error) {
        s.toast({ tone: 'error', text: t('toast.failed', { title: `${t(`task.kinds.${j.kind}`)} ${j.title}`, message: fmt.error(j.error) }), sticky: true });
      } else if (j.state === 'cancelled') {
        s.toast({ tone: 'info', text: t('toast.cancelled') });
      } else if (j.state === 'done' && j.report) {
        const r = j.report;
        const damaged = r.warnings.filter((w) => ['E1201', 'E1202', 'E1205'].includes(w.code)).length;
        if (j.kind === 'extract') {
          s.toast({
            tone: r.warnings.length ? 'warn' : 'success',
            text: t('toast.extracted', { count: r.files, dest: j.output ? basename(j.output) : '' }),
            action: j.output ? { label: t('task.openFolder'), run: () => void api.reveal(j.output!) } : undefined,
          });
        } else if (j.kind === 'compress') {
          s.toast({
            tone: 'success',
            text: t('toast.compressed', { name: j.output ? basename(j.output) : j.title }),
            action: j.output ? { label: t('toast.show'), run: () => void api.reveal(j.output!) } : undefined,
          });
        } else if (j.kind === 'test') {
          s.toast(damaged ? { tone: 'error', text: t('toast.testedErrors', { name: j.title, count: damaged }), sticky: true } : { tone: 'success', text: t('toast.tested', { name: j.title }) });
        } else {
          s.toast({ tone: 'success', text: t('toast.updated', { name: j.title }) });
        }
        if (r.skipped > 0 && j.kind === 'extract') s.toast({ tone: 'info', text: t('toast.skipped', { count: r.skipped }) });
      }
    };
    const onPrompt = (p: PromptEvent) => {
      if (!p.window || p.window === 'main') routePrompt(p);
    };
    void api.prompts().then((list) => list.forEach(onPrompt));
    const onChanged = (id: number) => {
      const s = useApp.getState();
      if (s.archive?.id === id) void api.summary(id).then((a) => useApp.getState().setArchive(a));
    };
    const subs = [events.onJob(onJob), events.onPrompt(onPrompt), events.onArchiveChanged(onChanged)];
    return () => {
      subs.forEach((p) => void p.then((f) => f()));
    };
  }, [t, fmt]);
}

export function TaskCard({ j }: { j: JobInfo }) {
  const { t } = useTranslation();
  const fmt = useFmt();
  const p = j.progress;
  const frac = p.bytesTotal ? p.bytesDone / p.bytesTotal : p.filesTotal ? p.filesDone / p.filesTotal : 0;
  const live = j.state === 'running' || j.state === 'paused';
  const status =
    j.state === 'done' ? t('task.done') : j.state === 'failed' ? t('task.failed') : j.state === 'cancelled' ? t('task.cancelled') : j.state === 'paused' ? t('task.paused') : fmt.size(j.speed) + '/s';
  const icon = j.kind === 'compress' || j.kind === 'add' ? 'package' : j.kind === 'test' ? 'checkCircle' : j.kind === 'extract' ? 'extract' : 'edit';
  return (
    <div className={`task ${j.state}`}>
      <div className="tt">
        <Icon name={icon} />
        <b title={j.title}>
          {t(`task.kinds.${j.kind}`)} · {j.title}
        </b>
        <span className="mono" style={{ fontSize: '.8rem' }}>{fmt.percent(frac, 1)}</span>
      </div>
      <ProgressBar value={j.state === 'done' ? 1 : frac} indeterminate={live && !p.bytesTotal && !p.filesTotal} />
      <div className="meta">
        <span>{status}</span>
        <span>{live && j.eta != null ? t('task.remaining', { time: fmt.duration(j.eta) }) : p.bytesTotal ? fmt.size(p.bytesTotal) : ''}</span>
      </div>
      {live && p.current && (
        <div className="meta">
          <span className="mono" style={{ fontSize: '.76rem' }}>{p.current}</span>
        </div>
      )}
      {j.state === 'failed' && j.error && <div className="errline">{fmt.error(j.error)}</div>}
      {j.report && j.report.warnings.length > 0 && <div className="hint">{t('task.warnings', { count: j.report.warnings.length })}</div>}
      <div className="acts">
        {live && (
          <>
            <button type="button" className="btn outline" onClick={() => void (j.state === 'running' ? api.pauseJob(j.id) : api.resumeJob(j.id))}>
              {t(j.state === 'running' ? 'task.pause' : 'task.resume')}
            </button>
            <button type="button" className="btn" onClick={() => void api.cancelJob(j.id)}>{t('task.cancel')}</button>
          </>
        )}
        {j.state === 'done' && j.output && (
          <button type="button" className="btn outline" onClick={() => void api.reveal(j.output!)}>{t('task.openFolder')}</button>
        )}
      </div>
    </div>
  );
}

export function TaskDrawer() {
  const { t } = useTranslation();
  const open = useApp((s) => s.drawer);
  const setDrawer = useApp((s) => s.setDrawer);
  const jobs = useApp((s) => s.jobs);
  if (!open) return null;
  const list = Object.values(jobs).sort((a, b) => b.id - a.id);
  const finished = list.some((j) => !['running', 'paused'].includes(j.state));
  return (
    <aside className="drawer" aria-label={t('task.title')}>
      <header>
        <h2>{t('task.title')}</h2>
        <button
          type="button"
          className="btn"
          disabled={!finished}
          onClick={() =>
            void api.clearJobs().then(() => api.jobs().then((l) => useApp.getState().setJobs(l)))
          }
        >
          {t('task.clear')}
        </button>
        <button type="button" className="iconbtn" aria-label={t('common.close')} onClick={() => setDrawer(false)}>
          <Icon name="x" />
        </button>
      </header>
      <div className="list">
        {list.length === 0 ? (
          <div className="emptyst">
            <Icon name="history" />
            <div>{t('task.none')}</div>
          </div>
        ) : (
          list.map((j) => <TaskCard key={j.id} j={j} />)
        )}
      </div>
    </aside>
  );
}
