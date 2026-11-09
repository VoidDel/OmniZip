// Small building blocks shared by every view. The components live in omni-ui (shared with the
// other Omni apps); this module wires them to OmniZip: translations, the toast store, and the
// drag region of standalone dialog windows.

import { useTranslation } from 'react-i18next';
import {
  Modal as UiModal,
  PasswordInput as UiPasswordInput,
  Toasts as UiToasts,
  type ModalProps,
  type PasswordInputProps,
} from 'omni-ui';

import { isShellWindow } from '../lib/fitWindow';
import { useApp } from '../store/app';

export { Switch, Segmented, Select, Tabs, ProgressBar, ContextMenu, type MenuItem } from 'omni-ui';

/** Modal dialog with focus trapping and Escape to close. */
export function Modal(props: Omit<ModalProps, 'dragHeader' | 'closeLabel'>) {
  const { t } = useTranslation();
  // In a standalone dialog window the header is the title bar.
  const drag = useApp((s) => !!s.shellLabel) || isShellWindow();
  return <UiModal {...props} dragHeader={drag} closeLabel={t('common.close')} />;
}

/** Toast stack (docs/04-ui-design.md §5.5). */
export function Toasts() {
  const { t } = useTranslation();
  const toasts = useApp((s) => s.toasts);
  const dismiss = useApp((s) => s.dismissToast);
  return <UiToasts toasts={toasts} onDismiss={dismiss} closeLabel={t('common.close')} />;
}

/** Password input with show / hide toggle. */
export function PasswordInput(props: Omit<PasswordInputProps, 'toggleLabel'>) {
  const { t } = useTranslation();
  return <UiPasswordInput {...props} toggleLabel={t('compress.showPassword')} />;
}
