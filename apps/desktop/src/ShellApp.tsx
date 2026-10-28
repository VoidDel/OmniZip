// Standalone dialog window for "Extract to…", "Add to archive…" and "Convert to…" started from
// the file manager (docs/07-shell-integration.md §6): only the dialog, no main window. Once the
// job starts, the same window shows its progress (MiniApp) and answers its prompts.

import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useState } from 'react';

import { MiniApp } from './MiniApp';
import { Toasts } from './components/ui';
import { DialogHost } from './features/dialogs/Dialogs';
import { api } from './lib/api';
import { useFitWindowToDialog } from './lib/fitWindow';
import { useApp } from './store/app';

type Kind = 'extract' | 'compress' | 'convert';

/** Window width per dialog (logical pixels); the height follows the content. */
const WIDTH: Record<Kind, number> = { extract: 560, compress: 600, convert: 560 };

/** The request is fetched and its dialog opened once per window, even if effects run twice. */
let requested = false;

export function ShellApp() {
  const [win] = useState(getCurrentWindow);
  const [kind, setKind] = useState<Kind | null>(null);
  const dialogs = useApp((s) => s.dialogs.length);
  const job = useApp((s) => s.shellJob);

  useEffect(() => {
    if (requested) return;
    requested = true;
    useApp.setState({ shellLabel: win.label });
    void api.shellRequest().then((r) => {
      const s = useApp.getState();
      if (r?.kind === 'extract') s.openDialog({ kind: 'extract', paths: r.paths });
      else if (r?.kind === 'compress') s.openDialog({ kind: 'compress', paths: r.paths });
      else if (r?.kind === 'convert') s.openDialog({ kind: 'convert', paths: r.paths });
      else {
        void win.close();
        return;
      }
      setKind(r.kind);
    });
  }, [win]);

  // Cancelled before a job started: nothing is left to show.
  useEffect(() => {
    if (kind && job == null && dialogs === 0) void win.close();
  }, [kind, job, dialogs, win]);

  // The window is shown only after it fits its dialog, so it never appears at the wrong size.
  useFitWindowToDialog(kind && job == null ? WIDTH[kind] : null, {
    onFirstFit: async () => {
      await win.center().catch(() => undefined);
      await api.windowReady();
    },
  });

  if (job != null) return <MiniApp jobId={job} label={win.label} />;
  return (
    <>
      <DialogHost />
      <Toasts />
    </>
  );
}
