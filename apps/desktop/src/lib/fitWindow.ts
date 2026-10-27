// Sizes the small windows opened from the file manager (docs/07 §6): the standalone dialog
// windows and the progress windows of their jobs.

import { LogicalSize, getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useRef } from 'react';

/**
 * Size of a job window: the progress view and the questions it asks (password, confirmations)
 * share it, so the window neither moves nor changes size between them. Questions that need
 * more room (conflicts) only grow downwards.
 */
export const JOB_WINDOW = { width: 440, height: 200 } as const;

/** Whether this is a small window opened from the file manager (dialog or progress). */
export function isShellWindow(): boolean {
  return document.documentElement.dataset.shell === 'true';
}

export interface FitOptions {
  /** Never smaller than this (logical pixels). */
  minHeight?: number;
  /** Runs once after the first resize (to center and show a window created hidden). */
  onFirstFit?: () => void | Promise<void>;
}

/**
 * While `width` is set, keeps the window `width` logical pixels wide and as tall as the open
 * dialog (capped at 88% of the screen), refitting as sections open and close. Resizing keeps
 * the window's top-left corner where it is.
 */
export function useFitWindowToDialog(width: number | null, options: FitOptions = {}) {
  const opts = useRef(options);
  opts.current = options;
  useEffect(() => {
    if (width == null) return;
    const win = getCurrentWindow();
    let fitted = false;
    const heights: number[] = [];
    let frame = 0;
    const fit = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const modal = document.querySelector<HTMLElement>('.modal');
        if (!modal) return;
        const body = modal.querySelector<HTMLElement>('.mb');
        const natural = modal.offsetHeight - (body?.clientHeight ?? 0) + (body?.scrollHeight ?? 0);
        const cap = Math.round(window.screen.availHeight * 0.88);
        const min = opts.current.minHeight ?? 0;
        const want = Math.ceil(natural);
        // Sub-pixel rounding must not make a question 1–2 px taller than the progress view.
        const height = Math.min(min > 0 && want <= min + 2 ? min : Math.max(want, min), cap);
        const last = heights[heights.length - 1];
        if (last !== undefined && Math.abs(height - last) <= 1) return;
        // Never flip-flop between two sizes (A → B → A).
        if (heights.length >= 2 && Math.abs(height - heights[heights.length - 2]) <= 1) return;
        heights.push(height);
        void win.setSize(new LogicalSize(width, height)).then(async () => {
          if (fitted) return;
          fitted = true;
          await opts.current.onFirstFit?.();
        });
      });
    };
    const observer = new MutationObserver(fit);
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, characterData: true });
    fit();
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [width]);
}
