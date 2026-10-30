// Standalone dialog window for "Extract to…", "Add to archive…", "Convert to…", "Mount" and
// "Eject" started from the file manager (docs/07-shell-integration.md §6): only the dialog, no
// main window. Once a job starts (or ejecting a mount saves its changes), the same window shows
// its progress (MiniApp) and answers its prompts.

import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useState } from 'react';

import { MiniApp } from './MiniApp';
import { Toasts } from './components/ui';
import { DialogHost } from './features/dialogs/Dialogs';
import { api } from './lib/api';
import { useFitWindowToDialog } from './lib/fitWindow';
import { mountRoot, useApp } from './store/app';

type Kind = 'extract' | 'compress' | 'convert' | 'mount' | 'unmount';

/** Window width per dialog (logical pixels); the height follows the content. */
const WIDTH: Record<Kind, number> = { extract: 560, compress: 600, convert: 560, mount: 540, unmount: 440 };

/**
 * "Eject" on a drive (or mount folder) OmniZip mounted: unmounts it. A mount with unsaved
 * changes first asks whether to save them, in this window; nothing else is shown. Resolves like
 * `unmount`: the job saving the changes, 0 when there was none, null when cancelled or failed.
 */
async function eject(path: string): Promise<number | null> {
  const s = useApp.getState();
  await s.refreshMounts();
  const key = (p: string) => p.replace(/[\\/]+$/, '').toLowerCase();
  const m = useApp.getState().mounts.find((x) => key(mountRoot(x)) === key(path));
  return m ? s.unmount(m.id) : null;
}

/** The request is fetched and its dialog opened once per window, even if effects run twice. */
let requested = false;

export function ShellApp() {
  const [win] = useState(getCurrentWindow);
  const [kind, setKind] = useState<Kind | null>(null);
  const dialogs = useApp((s) => s.dialogs.length);
  const job = useApp((s) => s.shellJob);
  // Ejecting: the window stays (hidden unless it asks something) until that is done.
  const [ejecting, setEjecting] = useState(false);

  useEffect(() => {
    if (requested) return;
    requested = true;
    useApp.setState({ shellLabel: win.label });
    void api.shellRequest().then((r) => {
      const s = useApp.getState();
      if (r?.kind === 'extract') s.openDialog({ kind: 'extract', paths: r.paths });
      else if (r?.kind === 'compress') s.openDialog({ kind: 'compress', paths: r.paths });
      else if (r?.kind === 'convert') s.openDialog({ kind: 'convert', paths: r.paths });
      else if (r?.kind === 'mount') s.openDialog({ kind: 'mount', path: r.path });
      else if (r?.kind === 'unmount') {
        setEjecting(true);
        void eject(r.path).then((saving) => {
          if (saving) useApp.setState({ shellJob: saving });
          setEjecting(false);
        });
      } else {
        void win.close();
        return;
      }
      setKind(r.kind);
    });
  }, [win]);

  // Cancelled before a job started (or ejected without saving): nothing is left to show.
  useEffect(() => {
    if (kind && job == null && dialogs === 0 && !ejecting) void win.close();
  }, [kind, job, dialogs, ejecting, win]);

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
