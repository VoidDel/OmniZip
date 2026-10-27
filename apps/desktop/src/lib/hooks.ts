import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { errorText } from '../i18n';
import { useApp } from '../store/app';
import { formatDate, formatDuration, formatPercent, formatRelative, formatSize } from './format';
import type { ErrorReport, Platform } from './types';

/** Formatting helpers bound to the current language and size-unit setting. */
export function useFmt() {
  const { i18n, t } = useTranslation();
  const unit = useApp((s) => s.settings?.language.size_unit ?? 'auto');
  const platform = (useApp((s) => s.boot?.system.platform) ?? 'win') as Platform;
  const lang = i18n.language;
  return useMemo(
    () => ({
      lang,
      platform,
      size: (b: number | null | undefined) => formatSize(b, unit, platform, lang),
      date: (ms: number | null | undefined) => formatDate(ms, lang),
      relative: (ms: number) => formatRelative(ms, lang),
      duration: (s: number) => formatDuration(s, lang),
      percent: (a: number, b: number) => formatPercent(a, b, lang),
      error: (e: ErrorReport) => errorText(t, e),
    }),
    [lang, unit, platform, t],
  );
}

/** Element size, updated on resize. */
export function useSize<T extends HTMLElement>(): [React.RefObject<T | null>, { width: number; height: number }] {
  const ref = useRef<T>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setSize({ width: e.contentRect.width, height: e.contentRect.height }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, size];
}

/** A stable callback that always sees the latest props. */
export function useEvent<A extends unknown[], R>(f: (...a: A) => R): (...a: A) => R {
  const ref = useRef(f);
  ref.current = f;
  return useCallback((...a: A) => ref.current(...a), []);
}
