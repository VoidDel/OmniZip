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

/**
 * A drop-down list. On Linux it is drawn here: WebKitGTK opens a `<select>` as a GTK menu,
 * outside the page, which no style reaches and which looks like nothing else in OmniZip.
 * Elsewhere it is the system's own control. The button is as wide as the longest option, as a
 * `<select>` is, so that it does not change size with the choice.
 */
export function Select<T extends string>({ value, options, onChange, id, label, style }: {
  value: T;
  options: [T, string][];
  onChange: (v: T) => void;
  id?: string;
  label?: string;
  style?: React.CSSProperties;
}) {
  const btn = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  if (document.documentElement.dataset.platform !== 'linux') {
    return (
      <select id={id} className="sel" style={style} value={value} aria-label={label} onChange={(e) => onChange(e.target.value as T)}>
        {options.map(([v, text]) => (
          <option key={v} value={v}>{text}</option>
        ))}
      </select>
    );
  }
  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) btn.current?.focus();
  };
  return (
    <>
      <button
        ref={btn}
        type="button"
        id={id}
        className="sel"
        style={style}
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(e.key)) {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        <span className="sel-val">
          {options.map(([v, text]) => (
            <span key={v} className={v === value ? 'on' : undefined} aria-hidden={v !== value}>{text}</span>
          ))}
        </span>
      </button>
      {open && btn.current && (
        <SelectList
          anchor={btn.current}
          options={options}
          value={value}
          onPick={(v) => {
            close(true);
            if (v !== value) onChange(v);
          }}
          onClose={close}
        />
      )}
    </>
  );
}

/** The open list of a `Select`, below its button (above when there is more room there). */
function SelectList<T extends string>({ anchor, options, value, onPick, onClose }: {
  anchor: HTMLElement;
  options: [T, string][];
  value: T;
  onPick: (v: T) => void;
  onClose: (refocus: boolean) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [active, setActive] = useState(Math.max(0, options.findIndex(([v]) => v === value)));
  const [pos, setPos] = useState<React.CSSProperties>({ visibility: 'hidden' });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const a = anchor.getBoundingClientRect();
    const below = window.innerHeight - a.bottom - 8;
    const above = a.top - 8;
    const up = el.scrollHeight > below && above > below;
    const max = Math.max(80, up ? above : below) - 4;
    const height = Math.min(el.scrollHeight, max);
    setPos({
      left: Math.max(8, Math.min(a.left, window.innerWidth - Math.max(el.offsetWidth, a.width) - 8)),
      top: up ? a.top - 4 - height : a.bottom + 4,
      minWidth: a.width,
      maxHeight: max,
    });
    el.focus();
  }, [anchor]);
  // The active option in view, scrolling the list only (never the page under it).
  useEffect(() => {
    const el = ref.current;
    const item = el?.children[active] as HTMLElement | undefined;
    if (!el || !item) return;
    if (item.offsetTop < el.scrollTop) el.scrollTop = item.offsetTop - 4;
    else if (item.offsetTop + item.offsetHeight > el.scrollTop + el.clientHeight) el.scrollTop = item.offsetTop + item.offsetHeight - el.clientHeight + 4;
  }, [active, pos]);
  useEffect(() => {
    const down = (e: MouseEvent) => {
      const target = e.target as Node;
      // A press on the button is its own: it toggles the list.
      if (!ref.current?.contains(target) && !anchor.contains(target)) onClose(false);
    };
    // The list stays where it opened: anything that moves its button closes it.
    const moved = (e: Event) => {
      if (!ref.current?.contains(e.target as Node)) onClose(false);
    };
    const blur = () => onClose(false);
    window.addEventListener('mousedown', down, true);
    window.addEventListener('scroll', moved, true);
    window.addEventListener('resize', blur);
    window.addEventListener('blur', blur);
    return () => {
      window.removeEventListener('mousedown', down, true);
      window.removeEventListener('scroll', moved, true);
      window.removeEventListener('resize', blur);
      window.removeEventListener('blur', blur);
    };
  }, [anchor, onClose]);
  const onKey = (e: React.KeyboardEvent) => {
    const last = options.length - 1;
    const to = ({ ArrowDown: active + 1, ArrowUp: active - 1, Home: 0, End: last, PageDown: active + 8, PageUp: active - 8 } as Record<string, number>)[e.key];
    if (to !== undefined) {
      e.preventDefault();
      setActive(Math.max(0, Math.min(last, to)));
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      onPick(options[active][0]);
    } else if (e.key === 'Escape' || e.key === 'Tab') {
      e.preventDefault();
      e.stopPropagation();
      onClose(true);
    } else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      // The next option starting with the letter typed.
      const k = e.key.toLowerCase();
      const next = [...options.keys()].map((i) => (active + 1 + i) % options.length).find((i) => options[i][1].toLowerCase().startsWith(k));
      if (next !== undefined) setActive(next);
    }
  };
  return createPortal(
    <div ref={ref} className="ctx sel-pop" role="listbox" tabIndex={-1} style={pos} onKeyDown={onKey}>
      {options.map(([v, text], i) => (
        <button
          key={v}
          type="button"
          role="option"
          aria-selected={v === value}
          className={i === active ? 'active' : undefined}
          onMouseEnter={() => setActive(i)}
          onClick={() => onPick(v)}
        >
          <span>{text}</span>
          {v === value && <Icon name="check" className="check" />}
        </button>
      ))}
    </div>,
    document.body,
  );
}

/**
 * Tabs over panels the caller renders (each with `role="tabpanel"` and `aria-labelledby` set to
 * `${id}-${value}`). Unlike `Segmented`, which picks a value, they switch what is shown: text
 * with an underline under the current tab. A tab marked `true` holds a setting that is not the
 * default (a dot). Arrow keys, Home and End move between tabs.
 */
export function Tabs<T extends string>({ id, value, options, onChange, label }: {
  id: string;
  value: T;
  options: [T, ReactNode, boolean?][];
  onChange: (v: T) => void;
  label?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  /** Moves to the tab a key points at; whether it was such a key. */
  const move = (key: string) => {
    const at = options.findIndex(([v]) => v === value);
    const to = ({ ArrowLeft: at - 1, ArrowRight: at + 1, Home: 0, End: options.length - 1 } as Record<string, number>)[key];
    if (to === undefined) return false;
    const next = options[(to + options.length) % options.length][0];
    onChange(next);
    ref.current?.querySelector<HTMLElement>(`#${id}-${next}`)?.focus();
    return true;
  };
  return (
    <div ref={ref} className="tabs" role="tablist" aria-label={label} onKeyDown={(e) => move(e.key) && e.preventDefault()}>
      {options.map(([v, text, marked]) => (
        <button type="button" role="tab" key={v} id={`${id}-${v}`} aria-selected={v === value} tabIndex={v === value ? 0 : -1} onClick={() => onChange(v)}>
          {text}
          {marked && <i className="mark" aria-hidden="true" />}
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

/** Modal dialog with focus trapping and Escape to close. */
export function Modal({ title, onClose, children, footer, width, sticky }: {
  title: ReactNode;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  width?: number;
  /** Clicking the backdrop does not close (answers that block a job). */
  sticky?: boolean;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  // In a standalone dialog window the header is the title bar.
  const drag = useApp((s) => !!s.shellLabel) || isShellWindow() || undefined;
  // Focus moves into the dialog once, when it opens. `onClose` is usually a new function on every
  // render; were it a dependency, each keystroke would re-run this and pull the focus back to the
  // first field (typing a password into a later field ended up in the file name).
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    const el = ref.current;
    const first = el?.querySelector<HTMLElement>('[data-autofocus], input:not([type=checkbox]):not([type=radio]):not([type=range]):not([readonly]), select, button.primary');
    first?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        closeRef.current();
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
      // A frame later: the rest of the key press that closed the dialog (Enter) must not reach
      // the button that opened it, which would open the dialog again.
      requestAnimationFrame(() => prev?.focus?.());
    };
  }, []);
  return createPortal(
    <div className="scrim" onMouseDown={(e) => !sticky && e.target === e.currentTarget && onClose()}>
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
