// Resolves the theme through the Rust theme service and applies it (docs/05-theming.md §8). The
// token writing itself is omni-ui's, shared with the other Omni apps.

import { getCurrentWindow } from '@tauri-apps/api/window';
import { onSystemThemeChange, systemPrefersDark, writeTheme, writeThemeAnimated } from 'omni-ui';

import { api } from './api';
import type { ResolvedTheme, Settings, SystemInfo } from './types';

export { onSystemThemeChange, systemPrefersDark };

export function effectiveDark(settings: Settings): boolean {
  const m = settings.appearance.mode;
  return m === 'dark' || (m === 'system' && systemPrefersDark());
}

export function materialActive(settings: Settings, system: SystemInfo, highContrast = false): boolean {
  return settings.appearance.material && system.materialSupported && !highContrast;
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
  const opts = {
    platform: system.platform,
    density: settings.appearance.density,
    iconStyle: settings.appearance.icon_style,
    material: materialActive(settings, system, resolved.highContrast),
    fontSize: settings.appearance.font_size,
  };
  if (animate) writeThemeAnimated(resolved, opts);
  else writeTheme(resolved, opts);
  // The native window theme drives Mica / vibrancy and the WebView color scheme. Following the
  // system must hand control back to the OS, otherwise prefers-color-scheme would stop tracking it.
  const mode = settings.appearance.mode;
  void getCurrentWindow()
    .setTheme(mode === 'system' ? null : mode)
    .catch(() => undefined);
  return resolved;
}
