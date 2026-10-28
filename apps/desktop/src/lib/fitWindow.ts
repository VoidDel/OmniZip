// Sizes the small windows opened from the file manager (docs/07 §6): the standalone dialog
// windows and the progress windows of their jobs.

import { LogicalSize, PhysicalPosition, currentMonitor, getCurrentWindow, type Window } from '@tauri-apps/api/window';
import { useEffect, useRef } from 'react';

import { nextFrame } from './frame';

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

/** Moves the window up when it has grown past the bottom of the monitor's work area. */
async function keepOnScreen(win: Window) {
  const [monitor, pos, size] = await Promise.all([currentMonitor(), win.outerPosition(), win.outerSize()]);
  if (!monitor) return;
  const area = monitor.workArea;
  const bottom = area.position.y + area.size.height;
  if (pos.y + size.height <= bottom) return;
  await win.setPosition(new PhysicalPosition(pos.x, Math.max(area.position.y, bottom - size.height)));
}

/**
 * While `width` is set, keeps the window `width` logical pixels wide and as tall as the open
 * dialog (capped at 88% of the screen), refitting as sections open and close. Resizing keeps
 * the window's top-left corner where it is, unless that would push its bottom off the screen.
 */
export function useFitWindowToDialog(width: number | null, options: FitOptions = {}) {
  const opts = useRef(options);
  opts.current = options;
  useEffect(() => {
    if (width == null) return;
    const win = getCurrentWindow();
    let fitted = false;
    let last: number | undefined;
    let cancel = () => {};
    const fit = () => {
      cancel();
      cancel = nextFrame(() => {
        const modal = document.querySelector<HTMLElement>('.modal');
        if (!modal) return;
        // The content's own height, from its parts: the modal box itself follows the window
        // (it stretches in a job window and is capped at the window height), and measuring it
        // would keep a window from shrinking back once a section is closed again.
        const part = (sel: string) => modal.querySelector<HTMLElement>(`:scope > ${sel}`);
        const natural = (part('header')?.offsetHeight ?? 0) + (part('.mb')?.scrollHeight ?? 0) + (part('footer')?.offsetHeight ?? 0);
        const cap = Math.round(window.screen.availHeight * 0.88);
        const min = opts.current.minHeight ?? 0;
        const want = Math.ceil(natural);
        // Sub-pixel rounding must not make a question 1–2 px taller than the progress view.
        const height = Math.min(min > 0 && want <= min + 2 ? min : Math.max(want, min), cap);
        if (last !== undefined && Math.abs(height - last) <= 1) return;
        last = height;
        void win.setSize(new LogicalSize(width, height)).then(async () => {
          const first = !fitted;
          fitted = true;
          // The first fit of a window created hidden centres it (onFirstFit) before showing it.
          if (first && opts.current.onFirstFit) await opts.current.onFirstFit();
          else await keepOnScreen(win).catch(() => undefined);
        });
      });
    };
    const observer = new MutationObserver(fit);
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, characterData: true });
    fit();
    return () => {
      observer.disconnect();
      cancel();
    };
  }, [width]);
}
