import { describe, expect, it } from 'vitest';

import { archiveStem, basename, dirname, extOf, formatSize, unitSystem } from './format';

describe('sizes', () => {
  it('follows the platform convention', () => {
    expect(unitSystem('auto', 'mac').base).toBe(1000);
    expect(unitSystem('auto', 'linux').labels[1]).toBe('KiB');
    expect(unitSystem('auto', 'win')).toEqual({ base: 1024, labels: ['B', 'KB', 'MB', 'GB', 'TB', 'PB'] });
  });

  it('formats with sensible precision', () => {
    expect(formatSize(0, 'kib', 'win', 'en-US')).toBe('0 B');
    expect(formatSize(1536, 'kib', 'win', 'en-US')).toBe('1.50 KiB');
    expect(formatSize(42 * 1024 * 1024, 'auto', 'win', 'en-US')).toBe('42.0 MB');
    expect(formatSize(1_500_000, 'kb', 'win', 'en-US')).toBe('1.50 MB');
    expect(formatSize(null, 'kb', 'win', 'en-US')).toBe('—');
  });
});

describe('names', () => {
  it('strips archive suffixes', () => {
    expect(archiveStem('backup-2026.tar.zst')).toBe('backup-2026');
    expect(archiveStem('movie.part1.rar')).toBe('movie');
    expect(archiveStem('photos.7z.001')).toBe('photos');
    expect(archiveStem('Report.ZIP')).toBe('Report');
    expect(archiveStem('noext')).toBe('noext');
  });

  it('splits paths on both separators', () => {
    expect(basename('C:\\a\\b.zip')).toBe('b.zip');
    expect(basename('/home/x/c.7z')).toBe('c.7z');
    expect(dirname('C:\\a\\b.zip')).toBe('C:\\a');
    expect(dirname('D:\\驱动.zip')).toBe('D:\\');
    expect(dirname('/home/u/a.zip')).toBe('/home/u');
    expect(extOf('a.TAR.GZ')).toBe('gz');
    expect(extOf('.env')).toBe('');
  });
});
