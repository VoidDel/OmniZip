// Small progress window for jobs started from the file manager (docs/04-ui-design.md §4.4).

import { LogicalSize, getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Icon } from './components/Icon';
import { ProgressBar } from './components/ui';
import { DialogHost } from './features/dialogs/Dialogs';
import { routePrompt } from './features/tasks/Tasks';
import { api, events } from './lib/api';
import { JOB_WINDOW, useFitWindowToDialog } from './lib/fitWindow';
import { basename } from './lib/format';
import { useFmt } from './lib/hooks';
import { applyTheme, onSystemThemeChange } from './lib/theme';
import type { JobInfo } from './lib/types';
import { useApp } from './store/app';

export function MiniApp({ jobId, label: windowLabel }: { jobId: number; label?: string }) {
  const { t } = useTranslation();
  const fmt = useFmt();
  const [job, setJob] = useState<JobInfo | null>(null);
  const label = windowLabel ?? `mini-${jobId}`;

  useEffect(() => {
    void api.jobs().then((list) => {
      const j = list.find((x) => x.id === jobId);
      if (j) setJob(j);
    });
    const subs = [
      events.onJob((j) => j.id === jobId && setJob(j)),
      events.onPrompt((p) => p.window === label && routePrompt(p)),
    ];
    // Prompts raised before this window finished loading.
    void api.prompts().then((list) => list.filter((p) => p.window === label).forEach(routePrompt));
    return () => subs.forEach((s) => void s.then((f) => f()));
  }, [jobId, label]);

  // Follow the OS switching between light and dark while the window is open.
  useEffect(() => {
    const { settings, boot } = useApp.getState();
    if (!settings || !boot) return;
    return onSystemThemeChange(() => void applyTheme(settings, boot.system, true).then((th) => useApp.getState().setTheme(th)));
  }, []);

  // The progress view and the questions of the job share one window size and position: a
  // question fills the window (and only grows downwards when it needs more room), then the
  // window returns to the progress view at the same place.
  const asking = useApp((s) => s.dialogs.length > 0);
  useEffect(() => {
    const root = document.documentElement;
    root.dataset.phase = 'job';
    return () => void delete root.dataset.phase;
  }, []);
  useEffect(() => {
    const w = getCurrentWindow();
    if (asking) void api.focusWindow().catch(() => undefined);
    else void w.setSize(new LogicalSize(JOB_WINDOW.width, JOB_WINDOW.height)).catch(() => undefined);
  }, [asking]);
  useFitWindowToDialog(asking ? JOB_WINDOW.width : null, { minHeight: JOB_WINDOW.height });

  // Close shortly after a clean finish; keep the window open when there is something to read.
  useEffect(() => {
    if (!job) return;
    const clean = job.state === 'done' && (job.report?.warnings.length ?? 0) === 0;
    if (clean || job.state === 'cancelled') {
      const h = setTimeout(() => void getCurrentWindow().close(), clean ? 1400 : 300);
      return () => clearTimeout(h);
    }
  }, [job]);

  const w = getCurrentWindow();
  const p = job?.progress;
  const frac = p && p.bytesTotal ? p.bytesDone / p.bytesTotal : p && p.filesTotal ? p.filesDone / p.filesTotal : 0;
  const live = job?.state === 'running' || job?.state === 'paused';
  const title = !job
    ? t('task.preparing')
    : job.state === 'done'
      ? `${t('task.done')} · ${job.title}`
      : job.state === 'failed'
        ? `${t('task.failed')} · ${job.title}`
        : t('task.working', { kind: t(`task.kinds.${job.kind}`), name: job.title });

  // While a question is open the window shows only that question (on a translucent material
  // the progress view would otherwise show through it).
  if (asking) return <DialogHost />;

  return (
    <div className="mini">
      <div className="mini-top" data-tauri-drag-region>
        <Icon name={job?.state === 'done' ? 'checkCircle' : job?.state === 'failed' ? 'alert' : job?.kind === 'compress' ? 'package' : 'extract'} />
        <span className="ttl" data-tauri-drag-region title={title}>{title}</span>
        <span className="mono hint" data-tauri-drag-region>{fmt.percent(job?.state === 'done' ? 1 : frac, 1)}</span>
        <div className="winctl">
          <button type="button" aria-label={t('title.minimize')} onClick={() => void w.minimize()}>
            <Icon name="minus" />
          </button>
          <button type="button" className="close" aria-label={t('title.close')} onClick={() => (live && job ? void api.cancelJob(job.id) : void w.close())}>
            <Icon name="x" />
          </button>
        </div>
      </div>
      <div className="mini-body">
        <ProgressBar value={job?.state === 'done' ? 1 : frac} indeterminate={live && !p?.bytesTotal && !p?.filesTotal} />
        <div className="meta">
          {live && job && <span>{fmt.size(job.speed)}/s</span>}
          {live && job?.eta != null && <span>{t('task.remaining', { time: fmt.duration(job.eta) })}</span>}
          {job?.state === 'done' && job.report && (
            <span>
              {job.kind === 'extract' && t('toast.extracted', { count: job.report.files, dest: job.output ? basename(job.output) : '' })}
              {job.kind === 'compress' && t('toast.compressed', { name: job.output ? basename(job.output) : job.title })}
              {job.kind === 'test' && t('toast.tested', { name: job.title })}
            </span>
          )}
          {job?.report && job.report.warnings.length > 0 && <span>{t('task.warnings', { count: job.report.warnings.length })}</span>}
        </div>
        {job?.state === 'failed' && job.error && <div className="err"><Icon name="alert" /><span>{fmt.error(job.error)}</span></div>}
        {live && p?.current && <div className="cur">{p.current}</div>}
        <div className="acts">
          {live && job ? (
            <>
              <button type="button" className="btn" onClick={() => void w.minimize()}>{t('task.background')}</button>
              <button type="button" className="btn outline" onClick={() => void (job.state === 'running' ? api.pauseJob(job.id) : api.resumeJob(job.id))}>
                {t(job.state === 'running' ? 'task.pause' : 'task.resume')}
              </button>
              <button type="button" className="btn" onClick={() => void api.cancelJob(job.id)}>{t('task.cancel')}</button>
            </>
          ) : (
            <>
              {job?.output && (
                <button type="button" className="btn outline" onClick={() => void api.reveal(job.output!)}>{t('task.openFolder')}</button>
              )}
              <button type="button" className="btn primary" onClick={() => void w.close()}>{t('task.close')}</button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
