// What the dialogs share: compression levels, closing, the level slider.

import { useTranslation } from 'react-i18next';

import type { Level } from '../../lib/types';
import { type Dialog, useApp } from '../../store/app';

export const LEVELS: Level[] = ['store', 'fastest', 'fast', 'normal', 'better', 'best', 'ultra'];
/** Rough output size ratio and throughput (MB/s) per level, for the estimate line. */
export const LEVEL_RATIO = [1, 0.72, 0.6, 0.5, 0.44, 0.4, 0.37];
export const LEVEL_SPEED = [900, 320, 180, 110, 70, 40, 22];

export function useClose(d: Dialog) {
  const close = useApp((s) => s.closeDialog);
  return () => close(d.id);
}

/**
 * A standalone dialog window (file-manager "Extract to…" / "Add to archive…") turns into the
 * job's progress window. Closing the dialog and recording the job is one store update, so the
 * window never sees "no dialog and no job" (which closes it) or the dialog over the progress.
 */
export function startedInShell(dialogId: number, jobId: number) {
  useApp.setState((s) => ({ shellJob: jobId, dialogs: s.dialogs.filter((x) => x.id !== dialogId) }));
}

/** The compression level slider with its labels; `children` go below it. */
export function LevelField({ level, onChange }: { level: number; onChange: (level: number) => void }) {
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
    </div>
  );
}


export const samePath = (a: string, b: string) =>
  document.documentElement.dataset.platform === 'win' ? a.toLowerCase() === b.toLowerCase() : a === b;
