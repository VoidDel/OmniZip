// Applies resolved theme tokens as CSS custom properties (docs/05-theming.md §8).

import { getCurrentWindow } from '@tauri-apps/api/window';

import { api } from './api';
import type { ResolvedTheme, Settings, SystemInfo } from './types';

export function systemPrefersDark(): boolean {
  return window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false;
}

export function effectiveDark(settings: Settings): boolean {
  const m = settings.appearance.mode;
  return m === 'dark' || (m === 'system' && systemPrefersDark());
}

export function materialActive(settings: Settings, system: SystemInfo, highContrast = false): boolean {
  return settings.appearance.material && system.materialSupported && !highContrast;
}

function write(resolved: ResolvedTheme, settings: Settings, system: SystemInfo): void {
  const root = document.documentElement;
  for (const [k, v] of Object.entries(resolved.tokens)) root.style.setProperty(`--${k}`, v);
  root.dataset.appearance = resolved.appearance;
  root.dataset.hc = String(resolved.highContrast);
  root.dataset.platform = system.platform;
  root.dataset.density = settings.appearance.density;
  root.dataset.icons = settings.appearance.icon_style;
  root.dataset.material = String(materialActive(settings, system, resolved.highContrast));
  root.style.colorScheme = resolved.appearance;
  root.style.setProperty('--size-base', `${settings.appearance.font_size}px`);
}

/** Resolves the theme for the current settings and applies it, cross-fading when possible. */
export async function applyTheme(settings: Settings, system: SystemInfo, animate = false): Promise<ResolvedTheme> {
  const dark = effectiveDark(settings);
  const id = dark ? settings.appearance.theme_dark : settings.appearance.theme_light;
  const resolved = await api.resolveTheme(id, {
    accent: settings.appearance.accent,
    systemAccent: system.accentColor,
    material: settings.appearance.material && system.materialSupported,
  });
  const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
  const doc = document as Document & { startViewTransition?: (cb: () => void) => unknown };
  if (animate && !reduce && doc.startViewTransition) {
    doc.startViewTransition(() => write(resolved, settings, system));
  } else {
    write(resolved, settings, system);
  }
  // The native window theme drives Mica / vibrancy and the WebView color scheme. Following the
  // system must hand control back to the OS, otherwise prefers-color-scheme would stop tracking it.
  const mode = settings.appearance.mode;
  void getCurrentWindow()
    .setTheme(mode === 'system' ? null : mode)
    .catch(() => undefined);
  return resolved;
}

/** Calls `f` when the OS switches between light and dark. */
export function onSystemThemeChange(f: () => void): () => void {
  const mq = window.matchMedia?.('(prefers-color-scheme: dark)');
  if (!mq) return () => {};
  mq.addEventListener('change', f);
  return () => mq.removeEventListener('change', f);
}
