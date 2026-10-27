import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import './styles/tokens.css';
import './styles/app.css';

import { App } from './App';
import { MiniApp } from './MiniApp';
import { ShellApp } from './ShellApp';
import i18next, { errorText, initI18n, resolveLang } from './i18n';
import { api } from './lib/api';
import { applyTheme } from './lib/theme';
import { errorHandler, useApp } from './store/app';

async function main() {
  const params = new URLSearchParams(location.search);
  const mini = params.get('mini');
  // Standalone dialog window opened from the file manager (no main window).
  const shell = params.has('shell');
  // Small windows opened from the file manager (dialog or progress): the dialog is the window.
  if (shell || mini) document.documentElement.dataset.shell = 'true';
  const boot = await api.bootstrap();
  useApp.getState().setBoot(boot);
  await initI18n(resolveLang(boot.settings.language.ui, boot.system.locale));
  errorHandler.render = (e) => errorText(i18next.t, e);
  const theme = await applyTheme(boot.settings, boot.system);
  useApp.getState().setTheme(theme);

  // The context menu of the WebView is replaced by our own menus.
  document.addEventListener('contextmenu', (e) => {
    const el = e.target as HTMLElement;
    if (!el.closest('input, textarea, .code, .toast')) e.preventDefault();
  });

  createRoot(document.getElementById('root')!).render(
    <StrictMode>{mini ? <MiniApp jobId={Number(mini)} /> : shell ? <ShellApp /> : <App />}</StrictMode>,
  );
  // Show the window only once the first frame is painted (no white flash, docs/05-theming.md §8).
  // A dialog window shows itself once it has been sized to its dialog.
  if (!shell) requestAnimationFrame(() => requestAnimationFrame(() => void api.windowReady()));
}

void main().catch((e) => {
  document.body.textContent = `OmniZip failed to start: ${JSON.stringify(e)}`;
  void api.windowReady();
});
