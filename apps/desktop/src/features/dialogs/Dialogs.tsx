// Dialogs (docs/04-ui-design.md §4.2–§4.10). The host renders the top-most open dialog.

import { useApp } from '../../store/app';

import { CompressDialog } from './CompressDialog';
import { ConvertDialog } from './ConvertDialog';
import { PreviewDialog, PropsDialog, RenameDialog } from './EntryDialogs';
import { ExtractDialog } from './ExtractDialog';
import { MountDialog } from './MountDialog';
import { AskDialog, ConfirmDialog, ConflictDialog, JobPasswordDialog, OpenPasswordDialog } from './prompts';

export function DialogHost() {
  const dialogs = useApp((s) => s.dialogs);
  const d = dialogs[dialogs.length - 1];
  if (!d) return null;
  switch (d.kind) {
    case 'extract':
      return <ExtractDialog key={d.id} d={d} />;
    case 'compress':
      return <CompressDialog key={d.id} d={d} />;
    case 'convert':
      return <ConvertDialog key={d.id} d={d} />;
    case 'openPassword':
      return <OpenPasswordDialog key={d.id} d={d} />;
    case 'jobPassword':
      return <JobPasswordDialog key={d.id} d={d} />;
    case 'conflict':
      return <ConflictDialog key={d.id} d={d} />;
    case 'confirm':
      return <ConfirmDialog key={d.id} d={d} />;
    case 'ask':
      return <AskDialog key={d.id} d={d} />;
    case 'props':
      return <PropsDialog key={d.id} d={d} />;
    case 'preview':
      return <PreviewDialog key={d.id} d={d} />;
    case 'rename':
      return <RenameDialog key={d.id} d={d} />;
    case 'mount':
      return <MountDialog key={d.id} d={d} />;
    default:
      return null;
  }
}
