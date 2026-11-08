import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ArchiveSummary } from '../lib/types';

// The store talks to Rust through `api`; here that is a few stubs.
vi.mock('../lib/api', async (importActual) => {
  const actual = await importActual<typeof import('../lib/api')>();
  return {
    ...actual,
    api: {
      openArchive: vi.fn(),
      closeArchive: vi.fn(async () => undefined),
      draftNew: vi.fn(),
    },
  };
});

const { api } = await import('../lib/api');
const { useApp } = await import('./app');

let nextId = 1;
const summary = (name: string, extra: Partial<ArchiveSummary> = {}): ArchiveSummary =>
  ({
    id: nextId++,
    path: `C:\\data\\${name}`,
    name,
    totalSize: 0,
    packedSize: 0,
    files: 0,
    dirs: 0,
    writable: true,
    capabilities: [],
    draft: false,
    entries: 0,
    ...extra,
  }) as ArchiveSummary;

const open = async (a: ArchiveSummary) => {
  vi.mocked(api.openArchive).mockResolvedValueOnce(a);
  expect(await useApp.getState().openArchive(a.path)).toBe(true);
};

const initial = useApp.getState();
beforeEach(() => {
  useApp.setState(initial, true);
  vi.clearAllMocks();
});

describe('open archives', () => {
  it('keep their place while another one is shown', async () => {
    const [a, b] = [summary('a.zip'), summary('b.7z')];
    await open(a);
    useApp.getState().navigate(5);
    useApp.getState().setSelection(new Set([7]), 7, 7, 100);
    await open(b);
    let s = useApp.getState();
    expect(s.archive?.id).toBe(b.id);
    expect(s.opened.map((x) => x.id)).toEqual([a.id, b.id]);
    expect(s.cwd).toBe(0);
    expect(s.views[a.id].cwd).toBe(5);

    s.switchArchive(a.id);
    s = useApp.getState();
    expect(s.cwd).toBe(5);
    expect([...s.selection]).toEqual([7]);
    expect(s.views[a.id]).toBeUndefined();
    expect(s.views[b.id].cwd).toBe(0);
  });

  it('are not opened twice, whatever the path spelling', async () => {
    const a = summary('a.zip');
    await open(a);
    await open(summary('b.zip'));
    expect(await useApp.getState().openArchive('c:/DATA/A.ZIP')).toBe(true);
    expect(api.openArchive).toHaveBeenCalledTimes(2);
    expect(useApp.getState().archive?.id).toBe(a.id);
  });

  it('ask for a password when the archive needs one', async () => {
    vi.mocked(api.openArchive).mockRejectedValueOnce({
      code: 'E1103',
      key: 'password-required',
      params: {},
      message: 'password required',
    });
    expect(await useApp.getState().openArchive('C:\\x.7z')).toBe(false);
    expect(useApp.getState().dialogs).toMatchObject([{ kind: 'openPassword', path: 'C:\\x.7z', wrong: false }]);
  });
});

describe('closing', () => {
  it('shows the neighbour where it was left, then the start page', async () => {
    const [a, b, c] = [summary('a.zip'), summary('b.zip'), summary('c.zip')];
    await open(a);
    useApp.getState().navigate(3);
    await open(b);
    await open(c);
    useApp.getState().switchArchive(b.id);

    useApp.getState().closeArchive(undefined, true);
    await vi.waitFor(() => expect(useApp.getState().archive?.id).toBe(a.id));
    expect(useApp.getState().cwd).toBe(3);
    expect(useApp.getState().opened.map((x) => x.id)).toEqual([a.id, c.id]);
    expect(api.closeArchive).toHaveBeenCalledWith(b.id);

    // One that is not shown just goes.
    useApp.getState().closeArchive(c.id, true);
    await vi.waitFor(() => expect(useApp.getState().opened).toHaveLength(1));
    expect(useApp.getState().views[c.id]).toBeUndefined();

    useApp.getState().closeArchive(a.id, true);
    await vi.waitFor(() => expect(useApp.getState().archive).toBeNull());
    expect(useApp.getState().view).toBe('home');
  });

  it('asks before a new archive with content is thrown away', async () => {
    const d = summary('new.zip', { draft: true, entries: 3 });
    vi.mocked(api.draftNew).mockResolvedValueOnce(d);
    await useApp.getState().newArchive('new.zip');
    useApp.getState().closeArchive();
    const [ask] = useApp.getState().dialogs;
    expect(ask).toMatchObject({ kind: 'ask', danger: true });
    expect(api.closeArchive).not.toHaveBeenCalled();
    if (ask.kind !== 'ask') throw new Error('not a question');
    ask.onConfirm();
    await vi.waitFor(() => expect(api.closeArchive).toHaveBeenCalledWith(d.id));
  });
});

describe('a rewritten archive', () => {
  it('goes to the folder given, keeping the folder path and the sidebar', async () => {
    const [a, b] = [summary('a.zip'), summary('b.zip')];
    await open(a);
    useApp.setState({ cwdPath: 'docs' });
    useApp.getState().toggleExpanded(a.id, 'docs', true);
    useApp.getState().navigate(4);
    useApp.getState().setSelection(new Set([9]));

    const rewritten = { ...a, files: 10 };
    useApp.getState().relocate(rewritten, 12);
    let s = useApp.getState();
    expect(s.cwd).toBe(12);
    expect(s.history).toEqual([12]);
    expect(s.selection.size).toBe(0);
    expect(s.cwdPath).toBe('docs');
    expect(s.expanded.has('docs')).toBe(true);
    expect(s.archive?.files).toBe(10);

    // Not shown: its saved view is replaced.
    await open(b);
    useApp.getState().relocate({ ...rewritten, files: 11 }, 2);
    s = useApp.getState();
    expect(s.archive?.id).toBe(b.id);
    expect(s.views[a.id]).toMatchObject({ cwd: 2, cwdPath: 'docs' });
    expect(s.opened.find((x) => x.id === a.id)?.files).toBe(11);
  });
});

describe('navigation', () => {
  it('keeps a history to go back and forward in', async () => {
    await open(summary('a.zip'));
    const s = useApp.getState();
    s.navigate(1);
    s.navigate(2);
    s.back();
    expect(useApp.getState().cwd).toBe(1);
    useApp.getState().navigate(3);
    // Going somewhere new drops what was ahead.
    expect(useApp.getState().history).toEqual([0, 1, 3]);
    useApp.getState().forward();
    expect(useApp.getState().cwd).toBe(3);
    useApp.getState().back();
    useApp.getState().back();
    useApp.getState().back();
    expect(useApp.getState().cwd).toBe(0);
  });
});
