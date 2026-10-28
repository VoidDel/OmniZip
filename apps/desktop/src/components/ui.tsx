// Small building blocks shared by every view.

import { type ReactNode, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

import { isShellWindow } from '../lib/fitWindow';
import { useApp } from '../store/app';
import { Icon, type IconName } from './Icon';

export function Switch({ checked, onChange, label, disabled }: { checked: boolean; onChange: (v: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <button
      type="button"
      className="switch"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
    />
  );
}

export function Segmented<T extends string>({ value, options, onChange, large, label }: {
  value: T;
  options: [T, ReactNode, boolean?][];
  onChange: (v: T) => void;
  large?: boolean;
  label?: string;
}) {
  return (
    <div className={`seg${large ? ' lg' : ''}`} role="group" aria-label={label}>
      {options.map(([v, text, disabled]) => (
        <button type="button" key={v} aria-pressed={v === value} disabled={disabled} onClick={() => onChange(v)}>
          {text}
        </button>
      ))}
    </div>
  );
}

export function ProgressBar({ value, indeterminate }: { value: number; indeterminate?: boolean }) {
  return (
    <div className={`progress${indeterminate ? ' indeterminate' : ''}`} role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(value * 100)}>
      <i style={{ width: `${Math.max(0, Math.min(1, value)) * 100}%` }} />
    </div>
  );
}

/** Modal dialog or side sheet with focus trapping and Escape to close. */
export function Modal({ title, onClose, children, footer, width, sheet, sticky }: {
  title: ReactNode;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  width?: number;
  sheet?: boolean;
  /** Clicking the backdrop does not close (answers that block a job). */
  sticky?: boolean;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  // In a standalone dialog window the header is the title bar.
  const drag = useApp((s) => !!s.shellLabel) || isShellWindow() || undefined;
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    const el = ref.current;
    const first = el?.querySelector<HTMLElement>('[data-autofocus], input:not([type=checkbox]):not([type=radio]):not([type=range]):not([readonly]), select, button.primary');
    first?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onClose();
      }
      if (e.key === 'Tab' && el) {
        const items = Array.from(el.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea, [tabindex]:not([tabindex="-1"]), summary'));
        if (!items.length) return;
        const [a, z] = [items[0], items[items.length - 1]];
        if (e.shiftKey && document.activeElement === a) {
          e.preventDefault();
          z.focus();
        } else if (!e.shiftKey && document.activeElement === z) {
          e.preventDefault();
          a.focus();
        }
      }
    };
    el?.addEventListener('keydown', onKey);
    return () => {
      el?.removeEventListener('keydown', onKey);
      prev?.focus?.();
    };
  }, [onClose]);
  return createPortal(
    <div className={`scrim${sheet ? ' sheet' : ''}`} onMouseDown={(e) => !sticky && e.target === e.currentTarget && onClose()}>
      <div ref={ref} className="modal" role="dialog" aria-modal="true" style={{ ['--mw' as string]: `${width ?? 520}px` }}>
        <header data-tauri-drag-region={drag}>
          <h2 data-tauri-drag-region={drag}>{title}</h2>
          <button type="button" className="iconbtn" onClick={onClose} aria-label={t('common.close')}>
            <Icon name="x" />
          </button>
        </header>
        <div className="mb">{children}</div>
        {footer && <footer>{footer}</footer>}
      </div>
    </div>,
    document.body,
  );
}

export interface MenuItem {
  label?: string;
  icon?: IconName;
  kbd?: string;
  disabled?: boolean;
  danger?: boolean;
  checked?: boolean;
  separator?: boolean;
  run?: () => void;
}

/** Context menu positioned at a point and kept inside the viewport. A press on `anchor` (the
 *  button that toggles the menu) is left to that button instead of closing the menu. */
export function ContextMenu({ x, y, items, onClose, anchor }: { x: number; y: number; items: MenuItem[]; onClose: () => void; anchor?: HTMLElement | null }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  const [active, setActive] = useState(-1);
  useLayoutEffect(() => {
    const r = ref.current?.getBoundingClientRect();
    if (!r) return;
    setPos({
      left: Math.max(8, Math.min(x, window.innerWidth - r.width - 8)),
      top: y + r.height > window.innerHeight - 8 ? Math.max(8, y - r.height) : y,
    });
    ref.current?.focus();
  }, [x, y]);
  useEffect(() => {
    const close = (e: MouseEvent) => {
      const target = e.target as Node;
      if (!ref.current?.contains(target) && !anchor?.contains(target)) onClose();
    };
    const blur = () => onClose();
    window.addEventListener('mousedown', close, true);
    window.addEventListener('blur', blur);
    window.addEventListener('resize', blur);
    return () => {
      window.removeEventListener('mousedown', close, true);
      window.removeEventListener('blur', blur);
      window.removeEventListener('resize', blur);
    };
  }, [onClose, anchor]);
  const enabled = items.map((it, i) => (!it.separator && !it.disabled ? i : -1)).filter((i) => i >= 0);
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onClose();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const idx = enabled.indexOf(active);
      const next = e.key === 'ArrowDown' ? enabled[(idx + 1) % enabled.length] : enabled[(idx - 1 + enabled.length) % enabled.length];
      setActive(next ?? -1);
    } else if (e.key === 'Enter' && active >= 0) {
      e.preventDefault();
      onClose();
      items[active].run?.();
    }
  };
  return createPortal(
    <div ref={ref} className="ctx" role="menu" tabIndex={-1} style={pos} onKeyDown={onKey} onContextMenu={(e) => e.preventDefault()}>
      {items.map((it, i) =>
        it.separator ? (
          <hr key={i} />
        ) : (
          <button
            key={i}
            type="button"
            role="menuitem"
            className={`${it.danger ? 'danger' : ''}${i === active ? ' active' : ''}`}
            disabled={it.disabled}
            onMouseEnter={() => setActive(i)}
            onClick={() => {
              onClose();
              it.run?.();
            }}
          >
            {it.icon ? <Icon name={it.icon} /> : <i className="ic" />}
            <span>{it.label}</span>
            {it.checked && <Icon name="check" className="check" />}
            {it.kbd && <kbd>{it.kbd}</kbd>}
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}

/** Toast stack (docs/04-ui-design.md §5.5). */
export function Toasts() {
  const { t } = useTranslation();
  const toasts = useApp((s) => s.toasts);
  const dismiss = useApp((s) => s.dismissToast);
  const icon: Record<string, IconName> = { success: 'checkCircle', info: 'info', warn: 'alert', error: 'alert' };
  return (
    <div className="toasts" aria-live="polite">
      {toasts.map((x) => (
        <div key={x.id} className={`toast ${x.tone}`} role="status">
          <Icon name={icon[x.tone]} />
          <span>{x.text}</span>
          {x.action && (
            <button
              type="button"
              className="lnk"
              onClick={() => {
                dismiss(x.id);
                x.action?.run();
              }}
            >
              {x.action.label}
            </button>
          )}
          <button type="button" className="iconbtn" aria-label={t('common.close')} onClick={() => dismiss(x.id)}>
            <Icon name="x" />
          </button>
        </div>
      ))}
    </div>
  );
}

/** Password input with show / hide toggle. */
export function PasswordInput({ value, onChange, id, autoFocus, onEnter, label }: {
  value: string;
  onChange: (v: string) => void;
  id?: string;
  autoFocus?: boolean;
  onEnter?: () => void;
  label: string;
}) {
  const { t } = useTranslation();
  const [show, setShow] = useState(false);
  return (
    <div className="pw">
      <input
        id={id}
        type={show ? 'text' : 'password'}
        value={value}
        autoComplete="off"
        spellCheck={false}
        aria-label={label}
        data-autofocus={autoFocus ? '' : undefined}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => e.key === 'Enter' && onEnter?.()}
      />
      <button type="button" className="iconbtn sm" onClick={() => setShow(!show)} aria-label={t('compress.showPassword')} aria-pressed={show}>
        <Icon name={show ? 'eyeOff' : 'eye'} />
      </button>
    </div>
  );
}
