// Settings (docs/04-ui-design.md §4.9, docs/05-theming.md, docs/06-i18n.md).

import { open } from '@tauri-apps/plugin-dialog';
import { type ReactNode, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Icon, Logo, type IconName } from '../../components/Icon';
import { Segmented, Switch } from '../../components/ui';
import { normalizeLang } from '../../i18n';
import { api } from '../../lib/api';
import { useFmt } from '../../lib/hooks';
import type { MountProbe, Settings as S, ShellStatus, ThemeSummary } from '../../lib/types';
import { type SettingsTab, useApp } from '../../store/app';

/** Sections of the settings page: listed in the sidebar, and above the page in narrow windows. */
export const SETTINGS_TABS: [SettingsTab, IconName][] = [
  ['general', 'layout'],
  ['appearance', 'palette'],
  ['language', 'globe'],
  ['formats', 'archive'],
  ['shell', 'terminal'],
  ['security', 'shield'],
  ['mount', 'drive'],
  ['advanced', 'cpu'],
  ['about', 'info'],
];

const ACCENTS = ['#2F6FED', '#5B8CFF', '#7A5AF8', '#C2410C', '#B8641B', '#D4587A', '#DB2777', '#0E9F8E', '#1E9E63', '#4D7C0F', '#0891B2', '#64748B'];

function Row({ label, hint, children, col }: { label: ReactNode; hint?: ReactNode; children?: ReactNode; col?: boolean }) {
  return (
    <div className={`set-row${col ? ' col' : ''}`}>
      <div className="lab">
        <span>{label}</span>
        {hint && <small>{hint}</small>}
      </div>
      <div className="ctl">{children}</div>
    </div>
  );
}

function Select<T extends string | number>({ value, options, onChange, label }: { value: T; options: [T, string][]; onChange: (v: T) => void; label: string }) {
  return (
    <select
      className="sel"
      value={String(value)}
      aria-label={label}
      onChange={(e) => {
        const raw = e.target.value;
        const found = options.find(([v]) => String(v) === raw);
        if (found) onChange(found[0]);
      }}
    >
      {options.map(([v, l]) => (
        <option key={String(v)} value={String(v)}>{l}</option>
      ))}
    </select>
  );
}

function ThemeCard({ th, selected, onPick, lang }: { th: ThemeSummary; selected: boolean; onPick: () => void; lang: string }) {
  const c = th.colors;
  const name = th.name[lang] ?? th.name['en-US'];
  return (
    <button type="button" className="tcard" aria-pressed={selected} title={name} onClick={onPick}>
      <div className="prev" style={{ background: c['bg.canvas'] }}>
        <div className="ps" style={{ background: c['bg.sidebar'] }}>
          <i style={{ background: c.accent, width: '70%' }} />
          <i style={{ background: c['fg.secondary'], width: '55%' }} />
          <i style={{ background: c['fg.secondary'], width: '62%' }} />
        </div>
        <div className="pm">
          <i style={{ background: c['bg.surface'], border: `1px solid ${c['border.default']}` }} />
          <i style={{ background: c['fg.primary'], width: '46%', opacity: 0.85 }} />
          <i style={{ background: c['fg.secondary'], width: '70%', opacity: 0.6 }} />
          <i style={{ background: c.accent, width: '28%' }} />
        </div>
      </div>
      <div className="tn">
        <span>{name}</span>
        <small>{th.contrast.toFixed(1)}:1</small>
      </div>
    </button>
  );
}

/** Registration state of the Windows context menu, with register / remove actions. */
function ShellStatusRow() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<ShellStatus | null>(null);
  useEffect(() => {
    void api.shellStatus().then(setStatus);
  }, []);
  const run = (f: () => Promise<ShellStatus>) => void f().then(setStatus).catch((e) => useApp.getState().reportError(e));
  if (!status) return null;
  if (!status.supported) {
    return (
      <Row label={t('settings.shell.status')} hint={t('settings.shell.pending')}>
        <span className="status-warn"><Icon name="alert" />{t('settings.shell.notRegistered')}</span>
      </Row>
    );
  }
  const hint = !status.dll ? t('settings.shell.missingDll') : status.stale ? t('settings.shell.stale') : t('settings.shell.win11');
  return (
    <Row label={t('settings.shell.status')} hint={hint}>
      {status.registered && !status.stale ? (
        <>
          <span className="status-ok"><Icon name="checkCircle" />{t('settings.shell.registered')}</span>
          <button type="button" className="btn" onClick={() => run(api.shellUnregister)}>{t('settings.shell.unregister')}</button>
        </>
      ) : (
        <>
          <span className="status-warn"><Icon name="alert" />{t('settings.shell.notRegistered')}</span>
          <button type="button" className="btn outline" disabled={!status.dll} onClick={() => run(api.shellRegister)}>
            {t('settings.shell.register')}
          </button>
        </>
      )}
    </Row>
  );
}

const BACKEND_NAMES: Record<MountProbe['kind'], string> = {
  'win-fsp': 'WinFsp',
  fuse: 'FUSE',
  'fs-kit': 'FSKit',
  'nfs-loopback': 'NFS',
};

/** Live state of the mount driver (docs/12-mount.md §9). */
function MountBackendRow() {
  const { t } = useTranslation();
  const [probe, setProbe] = useState<MountProbe | null>(null);
  useEffect(() => {
    void api.mountProbe().then(setProbe);
  }, []);
  if (!probe) return null;
  const name = BACKEND_NAMES[probe.kind];
  const ready = probe.health === 'ready';
  const label = ready
    ? t('settings.mount.installed', { name })
    : probe.health === 'unsupported'
      ? t('settings.mount.unsupported')
      : probe.kind === 'win-fsp'
        ? t('settings.mount.missing')
        : t('settings.mount.missingDriver', { name });
  return (
    <Row label={t('settings.mount.backend')} hint={probe.built ? (probe.detail ?? undefined) : t('settings.mount.pending')}>
      <span className={ready ? 'status-ok' : 'status-warn'} title={probe.detail ?? undefined}>
        <Icon name={ready ? 'checkCircle' : 'alert'} />
        {label}
      </span>
    </Row>
  );
}

export function SettingsView() {
  const { t, i18n } = useTranslation();
  const fmt = useFmt();
  const tab = useApp((s) => s.settingsTab);
  const setTab = useApp((s) => s.setSettingsTab);
  const settings = useApp((s) => s.settings)!;
  const boot = useApp((s) => s.boot)!;
  const themes = useApp((s) => s.themes);
  const theme = useApp((s) => s.theme);
  const update = useApp((s) => s.updateSettings);
  const set = (f: (s: S) => void) => void update(f);
  const sys = boot.system;
  const lang = i18n.language;

  const body = (() => {
    switch (tab) {
      case 'general':
        return (
          <div className="set-group">
            <Row label={t('settings.general.startup')}>
              <Select label={t('settings.general.startup')} value={settings.general.startup} onChange={(v) => set((s) => (s.general.startup = v))} options={[['home', t('settings.general.startupHome')], ['last', t('settings.general.startupLast')]]} />
            </Row>
            <Row label={t('settings.general.extractTo')}>
              <Select
                label={t('settings.general.extractTo')}
                value={settings.general.extract_to}
                onChange={(v) => set((s) => (s.general.extract_to = v))}
                options={[['ask', t('settings.general.extractAsk')], ['same', t('settings.general.extractSame')], ['downloads', t('settings.general.extractDownloads')]]}
              />
            </Row>
            <Row label={t('settings.general.openAfter')}>
              <Switch label={t('settings.general.openAfter')} checked={settings.general.open_after_extract} onChange={(v) => set((s) => (s.general.open_after_extract = v))} />
            </Row>
            <Row label={t('settings.general.notify')} hint={t('settings.general.notifyHint')}>
              <Switch label={t('settings.general.notify')} checked={settings.general.notify} onChange={(v) => set((s) => (s.general.notify = v))} />
            </Row>
          </div>
        );
      case 'appearance': {
        const lights = themes.filter((x) => x.appearance === 'light');
        const darks = themes.filter((x) => x.appearance === 'dark');
        const a = settings.appearance;
        const accentIsCustom = a.accent.startsWith('#');
        const accentColor = theme?.tokens.accent ?? '#2F6FED';
        return (
          <>
            <div className="set-group">
              <Row label={t('settings.appearance.mode')}>
                <Segmented value={a.mode} onChange={(v) => set((s) => (s.appearance.mode = v))} options={[['light', t('settings.appearance.light')], ['dark', t('settings.appearance.dark')], ['system', t('settings.appearance.system')]]} />
              </Row>
              <Row col label={t('settings.appearance.themeLight')} hint={t('settings.appearance.themeHint')}>
                <div className="themes">
                  {lights.map((th) => (
                    <ThemeCard key={th.id} th={th} lang={lang} selected={a.theme_light === th.id} onPick={() => set((s) => (s.appearance.theme_light = th.id))} />
                  ))}
                </div>
              </Row>
              <Row col label={t('settings.appearance.themeDark')}>
                <div className="themes">
                  {darks.map((th) => (
                    <ThemeCard key={th.id} th={th} lang={lang} selected={a.theme_dark === th.id} onPick={() => set((s) => (s.appearance.theme_dark = th.id))} />
                  ))}
                </div>
              </Row>
              <Row col label={t('settings.appearance.accent')}>
                <div className="swatches">
                  <Segmented
                    value={accentIsCustom ? ('custom' as string) : a.accent}
                    onChange={(v) => v !== 'custom' && set((s) => (s.appearance.accent = v))}
                    options={[
                      ['theme', t('settings.appearance.accentTheme')],
                      ['system', t('settings.appearance.accentSystem'), !sys.accentColor],
                    ]}
                  />
                  <span style={{ width: 8 }} />
                  {ACCENTS.map((c) => (
                    <button key={c} type="button" className="sw" style={{ background: c }} aria-pressed={a.accent.toUpperCase() === c} aria-label={c} onClick={() => set((s) => (s.appearance.accent = c))} />
                  ))}
                  <input type="color" aria-label={t('settings.appearance.accentCustom')} value={accentIsCustom ? a.accent : accentColor} onChange={(e) => set((s) => (s.appearance.accent = e.target.value.toUpperCase()))} />
                </div>
              </Row>
              <Row label={t('settings.appearance.material')} hint={sys.materialSupported ? t('settings.appearance.materialHint') : t('settings.appearance.materialUnavailable')}>
                {sys.platform === 'win' && sys.materialSupported ? (
                  // One choice for both "whether" and "which": Off, Mica, Mica Alt or Acrylic.
                  <Segmented
                    label={t('settings.appearance.material')}
                    value={a.material ? a.material_kind : 'off'}
                    onChange={(v) =>
                      set((s) => {
                        s.appearance.material = v !== 'off';
                        if (v !== 'off') s.appearance.material_kind = v;
                      })
                    }
                    options={[
                      ['off', t('settings.appearance.materialKinds.off')],
                      ['mica', t('settings.appearance.materialKinds.mica')],
                      ['mica-alt', t('settings.appearance.materialKinds.micaAlt')],
                      ['acrylic', t('settings.appearance.materialKinds.acrylic')],
                    ]}
                  />
                ) : (
                  <Switch label={t('settings.appearance.material')} disabled={!sys.materialSupported} checked={a.material && sys.materialSupported} onChange={(v) => set((s) => (s.appearance.material = v))} />
                )}
              </Row>
              <Row label={t('settings.appearance.density')}>
                <Segmented value={a.density} onChange={(v) => set((s) => (s.appearance.density = v))} options={[['compact', t('settings.appearance.compact')], ['comfortable', t('settings.appearance.comfortable')], ['spacious', t('settings.appearance.spacious')]]} />
              </Row>
              <Row label={t('settings.appearance.icons')}>
                <Segmented value={a.icon_style} onChange={(v) => set((s) => (s.appearance.icon_style = v))} options={[['mono', t('settings.appearance.mono')], ['duotone', t('settings.appearance.duotone')]]} />
              </Row>
              <Row label={t('settings.appearance.fontSize')}>
                <span className="mono hint">{a.font_size}px</span>
                <input type="range" min={12} max={20} step={1} value={a.font_size} aria-label={t('settings.appearance.fontSize')} onChange={(e) => set((s) => (s.appearance.font_size = Number(e.target.value)))} />
              </Row>
            </div>
            <div className="inline">
              <button
                type="button"
                className="btn outline"
                onClick={async () => {
                  const p = await open({ multiple: false, filters: [{ name: 'Theme', extensions: ['json'] }] });
                  if (typeof p !== 'string') return;
                  try {
                    useApp.getState().setThemes(await api.importTheme(p));
                    useApp.getState().toast({ tone: 'success', text: t('toast.themeImported') });
                  } catch (e) {
                    useApp.getState().reportError(e);
                  }
                }}
              >
                <Icon name="upload" />
                <span>{t('settings.appearance.import')}</span>
              </button>
              <button type="button" className="btn" onClick={() => void api.openThemesFolder()}>
                <Icon name="folderOpen" />
                <span>{t('settings.appearance.openFolder')}</span>
              </button>
              <button type="button" className="btn" onClick={() => void api.reloadThemes().then((x) => useApp.getState().setThemes(x))}>
                <Icon name="refresh" />
                <span>{t('settings.appearance.reload')}</span>
              </button>
            </div>
          </>
        );
      }
      case 'language': {
        const sysLang = boot.languages.find(([l]) => l === normalizeLang(sys.locale))?.[1] ?? sys.locale;
        return (
          <div className="set-group">
            <Row label={t('settings.language.ui')}>
              <Select
                label={t('settings.language.ui')}
                value={settings.language.ui}
                onChange={(v) => set((s) => (s.language.ui = v))}
                options={[['auto', t('settings.language.auto', { lang: sysLang })], ...boot.languages.map(([l, n]) => [l, n] as [string, string])]}
              />
            </Row>
            <Row label={t('settings.language.sizeUnit')} hint={`${t('settings.language.sample')}: ${fmt.size(1_536_000)} · ${fmt.size(4.2 * 1024 ** 3)}`}>
              <Select
                label={t('settings.language.sizeUnit')}
                value={settings.language.size_unit}
                onChange={(v) => set((s) => (s.language.size_unit = v))}
                options={[['auto', t('settings.language.unitAuto')], ['kb', t('settings.language.unitKb')], ['kib', t('settings.language.unitKib')]]}
              />
            </Row>
            <Row label={t('settings.language.sample')}>
              <span className="mono hint">{fmt.date(Date.now())}</span>
            </Row>
            <Row label={t('settings.language.encoding')}>
              <Select
                label={t('settings.language.encoding')}
                value={settings.language.default_encoding}
                onChange={(v) => set((s) => (s.language.default_encoding = v))}
                options={[['auto', t('settings.language.encodingAuto')], ...boot.encodings.map((e) => [e, e] as [string, string])]}
              />
            </Row>
          </div>
        );
      }
      case 'formats':
        return (
          <>
            <div className="set-group">
              <table className="fmt-table">
                <thead>
                  <tr>
                    <th>{t('settings.formats.format')}</th>
                    <th>{t('settings.formats.support')}</th>
                    <th>{t('settings.formats.associate')}</th>
                  </tr>
                </thead>
                <tbody>
                  {boot.formats
                    .filter((f) => f.tier <= 2)
                    .map((f) => {
                      const ext = f.extensions[0].replace(/^\./, '').split('.').pop()!;
                      const rw = f.capabilities.includes('WRITE');
                      return (
                        <tr key={f.format}>
                          <td><b>{f.label}</b> <span className="mono hint">{f.extensions.slice(0, 3).join(' ')}</span></td>
                          <td className={f.engine ? '' : 'dimc'}>
                            {f.engine ? (rw ? t('settings.formats.readWrite') : t('settings.formats.readOnly')) : t('settings.formats.notInBuild')}
                          </td>
                          <td>
                            {f.engine && (
                              <Switch label={f.label} checked={settings.formats.associations[ext] ?? false} onChange={(v) => set((s) => (s.formats.associations[ext] = v))} />
                            )}
                          </td>
                        </tr>
                      );
                    })}
                </tbody>
              </table>
            </div>
            <div className="set-group">
              <Row label={t('settings.formats.defaultFormat')}>
                <Select label={t('settings.formats.defaultFormat')} value={settings.formats.default_format} onChange={(v) => set((s) => (s.formats.default_format = v))} options={['zip', '7z', 'tar.zst', 'tar.gz', 'tar.xz'].map((f) => [f, f.toUpperCase()] as [string, string])} />
              </Row>
              <Row label={t('settings.formats.defaultLevel')}>
                <Select
                  label={t('settings.formats.defaultLevel')}
                  value={settings.formats.default_level}
                  onChange={(v) => set((s) => (s.formats.default_level = v))}
                  options={(['store', 'fastest', 'fast', 'normal', 'better', 'best', 'ultra'] as const).map((l) => [l, t(`compress.levels.${l}`)] as [S['formats']['default_level'], string])}
                />
              </Row>
              <Row label={t('settings.formats.defaultApp')} hint={t('settings.formats.defaultAppHint')}>
                <button type="button" className="btn outline" onClick={() => void api.openDefaultApps()}>
                  <Icon name="external" />
                  <span>{t('settings.formats.openDefaultApps')}</span>
                </button>
              </Row>
            </div>
          </>
        );
      case 'shell': {
        const items = Object.keys(settings.shell.items);
        const on = items.filter((k) => settings.shell.items[k]);
        const line = (icon: IconName, label: string, cls = '') => (
          <div className={cls} key={label}>
            {settings.shell.icons ? <Icon name={icon} /> : <i className="ic" />}
            <span>{label}</span>
          </div>
        );
        return (
          <>
            <div className="set-group">
              <ShellStatusRow />
              <Row label={t('settings.shell.style')}>
                <Segmented value={settings.shell.style} onChange={(v) => set((s) => (s.shell.style = v))} options={[['cascade', t('settings.shell.cascade')], ['flat', t('settings.shell.flat')]]} />
              </Row>
              <Row col label={t('settings.shell.items')}>
                <div className="inline" style={{ gap: '8px 18px' }}>
                  {items.map((k) => (
                    <label key={k} className="chk">
                      <input type="checkbox" checked={settings.shell.items[k]} onChange={(e) => set((s) => (s.shell.items[k] = e.target.checked))} /> {t(`settings.shell.items_${k}`)}
                    </label>
                  ))}
                </div>
              </Row>
              <Row label={t('settings.shell.icons')}>
                <Switch label={t('settings.shell.icons')} checked={settings.shell.icons} onChange={(v) => set((s) => (s.shell.icons = v))} />
              </Row>
              <Row col label={t('settings.shell.preview')}>
                <div className="menu-prev">
                  {line('folderOpen', t('cmd.open'))}
                  <hr />
                  {settings.shell.style === 'cascade' ? (
                    <>
                      {line('archive', 'OmniZip', 'hl')}
                      {on.map((k) => line(k.startsWith('add') ? 'package' : k === 'mount' ? 'drive' : k === 'test' ? 'checkCircle' : 'extract', t(`settings.shell.items_${k}`), 'sub'))}
                    </>
                  ) : (
                    <>
                      {on.slice(0, 4).map((k) => line(k.startsWith('add') ? 'package' : 'extract', t(`settings.shell.items_${k}`)))}
                      {on.length > 4 && line('archive', 'OmniZip ›', 'sub')}
                    </>
                  )}
                  <hr />
                  {line('copy', t('cmd.copyPath'))}
                </div>
              </Row>
            </div>
          </>
        );
      }
      case 'security':
        return (
          <div className="set-group">
            <Row label={t('settings.security.ratio')} hint={t('settings.security.ratioHint')}>
              <Select label={t('settings.security.ratio')} value={settings.security.max_ratio} onChange={(v) => set((s) => (s.security.max_ratio = v))} options={[[100, '1 : 100'], [1000, '1 : 1 000'], [10000, '1 : 10 000']]} />
            </Row>
            <Row label={t('settings.security.origin')} hint={t('settings.security.originHint')}>
              <Switch label={t('settings.security.origin')} checked={settings.security.propagate_origin} onChange={(v) => set((s) => (s.security.propagate_origin = v))} />
            </Row>
            <Row label={t('settings.security.symlinks')}>
              <Select
                label={t('settings.security.symlinks')}
                value={settings.security.symlinks}
                onChange={(v) => set((s) => (s.security.symlinks = v))}
                options={[['inside', t('settings.security.symInside')], ['file', t('settings.security.symFile')], ['skip', t('settings.security.symSkip')]]}
              />
            </Row>
          </div>
        );
      case 'mount':
        return (
          <div className="set-group">
            <MountBackendRow />
            <Row label={t('settings.mount.cache')}>
              <span className="mono hint">{settings.mount.cache_gb} GB</span>
              <input type="range" min={1} max={32} value={settings.mount.cache_gb} aria-label={t('settings.mount.cache')} onChange={(e) => set((s) => (s.mount.cache_gb = Number(e.target.value)))} />
            </Row>
            <Row label={t('settings.mount.unmountOnExit')}>
              <Switch label={t('settings.mount.unmountOnExit')} checked={settings.mount.unmount_on_exit} onChange={(v) => set((s) => (s.mount.unmount_on_exit = v))} />
            </Row>
            <Row label={t('settings.mount.restore')}>
              <Switch label={t('settings.mount.restore')} checked={settings.mount.restore_on_start} onChange={(v) => set((s) => (s.mount.restore_on_start = v))} />
            </Row>
            <Row label={t('settings.mount.warm')}>
              <Switch label={t('settings.mount.warm')} checked={settings.mount.warm_solid} onChange={(v) => set((s) => (s.mount.warm_solid = v))} />
            </Row>
            {sys.platform === 'win' && (
              <Row label={t('settings.mount.offline')} hint={t('settings.mount.offlineHint')}>
                <Switch label={t('settings.mount.offline')} checked={settings.mount.offline_attribute} onChange={(v) => set((s) => (s.mount.offline_attribute = v))} />
              </Row>
            )}
          </div>
        );
      case 'advanced':
        return (
          <div className="set-group">
            <Row label={t('settings.advanced.threads')}>
              <Select label={t('settings.advanced.threads')} value={settings.advanced.threads} onChange={(v) => set((s) => (s.advanced.threads = v))} options={[[0, t('settings.advanced.auto')], [1, '1'], [2, '2'], [4, '4'], [8, '8'], [16, '16']]} />
            </Row>
            <Row label={t('settings.advanced.logLevel')}>
              <Select label={t('settings.advanced.logLevel')} value={settings.advanced.log_level} onChange={(v) => set((s) => (s.advanced.log_level = v))} options={['error', 'warn', 'info', 'debug', 'trace'].map((x) => [x, x] as [string, string])} />
            </Row>
            <Row label={t('settings.advanced.configFile')}>
              <input className="inp mono" readOnly value={boot.configPath ?? ''} style={{ width: 'min(360px, 100%)' }} />
            </Row>
          </div>
        );
      case 'about':
        return (
          <>
            <div className="about-logo">
              <Logo size={56} />
              <div>
                <b>OmniZip</b>
                <br />
                <span>{t('app.tagline')}</span>
              </div>
            </div>
            <div className="set-group">
              <Row label={t('settings.about.version')}>
                <span className="mono">{sys.appVersion}</span>
              </Row>
              <Row label={t('settings.about.license')}>
                <span>MIT</span>
              </Row>
              <Row col label={t('settings.about.engines')}>
                <div className="chips">
                  {Array.from(new Set(boot.formats.map((f) => f.engine).filter(Boolean))).map((e) => (
                    <span key={e} className="chip">{e}</span>
                  ))}
                </div>
              </Row>
              <Row col label={t('settings.about.components')}>
                <div className="chips">
                  {['Tauri 2', 'React 19', 'zip', 'sevenz-rust2', 'tar', 'flate2 · zlib-rs', 'liblzma', 'zstd', 'lz4_flex', 'brotli', 'unrar', 'chardetng', 'Fluent', 'i18next'].map((c) => (
                    <span key={c} className="chip">{c}</span>
                  ))}
                </div>
              </Row>
            </div>
          </>
        );
      default:
        return null;
    }
  })();

  return (
    <div className="settings">
      <nav className="set-nav">
        {SETTINGS_TABS.map(([id, icon]) => (
          <button key={id} type="button" className="nav-item" aria-current={tab === id} onClick={() => setTab(id)}>
            <Icon name={icon} />
            <span className="lbl">{t(`settings.tabs.${id}`)}</span>
          </button>
        ))}
      </nav>
      <div className="set-body">
        <h2>{t(`settings.tabs.${tab}`)}</h2>
        {body}
      </div>
    </div>
  );
}
