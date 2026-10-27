// i18next with ICU MessageFormat (docs/06-i18n.md §4). All four languages are bundled: together
// they are smaller than one font file, and switching must not wait on the network or disk.

import i18next, { type TFunction } from 'i18next';
import ICU from 'i18next-icu';
import { initReactI18next } from 'react-i18next';

import enUS from '@locales/en-US/app.json';
import jaJP from '@locales/ja-JP/app.json';
import zhCN from '@locales/zh-CN/app.json';
import zhTW from '@locales/zh-TW/app.json';

import type { ErrorReport } from '../lib/types';

export const SUPPORTED = ['zh-CN', 'zh-TW', 'en-US', 'ja-JP'] as const;
export type Lang = (typeof SUPPORTED)[number];

const resources = {
  'en-US': { app: enUS },
  'zh-CN': { app: zhCN },
  'zh-TW': { app: zhTW },
  'ja-JP': { app: jaJP },
};

/** Maps any locale tag to a supported language. */
export function normalizeLang(tag: string | null | undefined): Lang {
  const l = (tag ?? '').replace('_', '-').toLowerCase();
  if (l.startsWith('zh')) return /hant|tw|hk|mo/.test(l) ? 'zh-TW' : 'zh-CN';
  if (l.startsWith('ja')) return 'ja-JP';
  return 'en-US';
}

/** The UI language for a setting (`auto` follows the system). */
export function resolveLang(setting: string, systemLocale: string): Lang {
  return setting === 'auto' ? normalizeLang(systemLocale) : normalizeLang(setting);
}

export async function initI18n(lang: Lang): Promise<void> {
  await i18next
    .use(ICU)
    .use(initReactI18next)
    .init({
      resources,
      lng: lang,
      fallbackLng: 'en-US',
      ns: ['app'],
      defaultNS: 'app',
      interpolation: { escapeValue: false },
      returnNull: false,
    });
  document.documentElement.lang = lang;
}

export async function setLanguage(lang: Lang): Promise<void> {
  await i18next.changeLanguage(lang);
  document.documentElement.lang = lang;
}

const ERROR_PARAMS = ['path', 'detail', 'format', 'method', 'operation', 'ratio', 'size', 'count', 'issue'];

/** Localized text for an error from the backend. */
export function errorText(t: TFunction, e: ErrorReport): string {
  const params: Record<string, string> = Object.fromEntries(ERROR_PARAMS.map((k) => [k, '']));
  Object.assign(params, e.params);
  const text = t(`errors.${e.key}`, { ...params, defaultValue: e.message }) as string;
  return text.replace(/[\s:：]+$/u, '');
}

export default i18next;
