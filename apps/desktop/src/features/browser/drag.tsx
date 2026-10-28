// Dragging items of an archive onto a folder: of the same archive (the list, the breadcrumb,
// the sidebar tree) or of another open one (its row in the sidebar). Folders that accept a drop
// carry `data-drop-archive="<id>"` and `data-drop-dir="<node>"`. With Ctrl held the items are
// copied, otherwise moved; items of an archive that cannot change can only be copied.
//
// Leaving the window, the drag goes on as a system drag (`onOut`): dropped on the desktop or in a
// file manager, the items are extracted there.
//
// Pointer events rather than HTML5 drag and drop: on Windows the webview handles drops itself
// (files dropped from the file manager), so HTML5 drag events never reach the page.

import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';

const TARGET = '[data-drop-dir]';
/** Pixels the pointer has to travel before a press becomes a drag. */
const THRESHOLD = 6;

export interface DropTarget {
  archive: number;
  dir: number;
}

/**
 * `source`: the archive the items come from; `canMove`: whether they can leave it (otherwise
 * they are always copied); `onOut`: takes over a drag leaving the window.
 */
export function useDragItems(
  source: number,
  canMove: boolean,
  onDrop: (nodes: number[], target: DropTarget, copy: boolean) => void,
  onOut?: (nodes: number[]) => void,
) {
  const [ghost, setGhost] = useState<{ label: string; x: number; y: number } | null>(null);
  /** Set right after a drag, for the click that ends it to be ignored. */
  const dragged = useRef(false);
  const stop = useRef<(() => void) | null>(null);
  const drop = useRef(onDrop);
  drop.current = onDrop;
  const out = useRef(onOut);
  out.current = onOut;
  useEffect(() => () => stop.current?.(), []);

  /**
   * Watches a press on an item; it turns into a drag once the pointer moves. `nodes` gives the
   * items at that moment (it may select the pressed one), `label` the text that follows the
   * pointer.
   */
  const press = (e: React.PointerEvent, nodes: () => number[], label: (nodes: number[], copy: boolean) => string) => {
    if (e.button !== 0 || e.shiftKey || e.metaKey) return;
    const x0 = e.clientX;
    const y0 = e.clientY;
    let moving: number[] | null = null;
    let over: HTMLElement | null = null;
    let pos = { x: x0, y: y0 };
    let ctrl = e.ctrlKey;
    const copying = () => ctrl || !canMove;
    const targetOf = (el: HTMLElement | null): DropTarget | null =>
      el ? { archive: Number(el.dataset.dropArchive), dir: Number(el.dataset.dropDir) } : null;
    const targetAt = (x: number, y: number) => {
      const el = document.elementFromPoint(x, y)?.closest<HTMLElement>(TARGET) ?? null;
      const t = targetOf(el);
      // Not onto one of the dragged folders themselves.
      return t && moving && !(t.archive === source && moving.includes(t.dir)) ? el : null;
    };
    const render = () => {
      if (!moving) return;
      over?.classList.remove('drop-over');
      over = targetAt(pos.x, pos.y);
      over?.classList.add('drop-over');
      setGhost({ label: label(moving, copying()), ...pos });
    };
    const end = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('keyup', onKey, true);
      over?.classList.remove('drop-over');
      document.body.classList.remove('dragging');
      setGhost(null);
      stop.current = null;
    };
    const onMove = (ev: PointerEvent) => {
      pos = { x: ev.clientX, y: ev.clientY };
      ctrl = ev.ctrlKey;
      if (!moving) {
        if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < THRESHOLD) return;
        moving = nodes();
        if (!moving.length) return end();
        window.getSelection()?.removeAllRanges();
        document.body.classList.add('dragging');
      }
      // The page keeps getting the pointer outside the window while the button is down.
      const outside = ev.clientX < 0 || ev.clientY < 0 || ev.clientX >= window.innerWidth || ev.clientY >= window.innerHeight;
      if (outside && out.current) {
        const items = moving;
        end();
        out.current(items);
        return;
      }
      render();
    };
    const onUp = (ev: PointerEvent) => {
      const items = moving;
      ctrl = ev.ctrlKey;
      const target = items ? targetOf(targetAt(ev.clientX, ev.clientY)) : null;
      const copy = copying();
      end();
      if (!items) return;
      dragged.current = true;
      setTimeout(() => (dragged.current = false), 0);
      if (target) drop.current(items, target, copy);
    };
    const onKey = (ev: KeyboardEvent) => {
      if (!moving) return;
      if (ev.key === 'Escape') {
        ev.stopPropagation();
        end();
      } else if (ev.key === 'Control') {
        ctrl = ev.type === 'keydown';
        render();
      }
    };
    stop.current?.();
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('keydown', onKey, true);
    window.addEventListener('keyup', onKey, true);
    stop.current = end;
  };

  const view =
    ghost &&
    createPortal(
      <div className="drag-ghost" style={{ left: ghost.x + 14, top: ghost.y + 14 }}>
        {ghost.label}
      </div>,
      document.body,
    );
  return { press, ghost: view, dragged };
}

/**
 * Picture of a drag out of the window, drawn like the label following the pointer inside it: a
 * PNG data URL in device pixels, the pointer at (`x`, `y`).
 */
export function dragImage(text: string): { png: string; x: number; y: number } | undefined {
  const probe = document.createElement('div');
  probe.className = 'drag-ghost';
  probe.textContent = text;
  probe.style.left = '-9999px';
  probe.style.top = '0';
  document.body.appendChild(probe);
  const cs = getComputedStyle(probe);
  const box = probe.getBoundingClientRect();
  const look = {
    radius: parseFloat(cs.borderTopLeftRadius) || 6,
    background: cs.backgroundColor,
    border: cs.borderTopColor,
    font: cs.font,
    color: cs.color,
    left: parseFloat(cs.paddingLeft) || 0,
    right: parseFloat(cs.paddingRight) || 0,
  };
  probe.remove();
  // Below right of the pointer as inside the window, with room for the shadow.
  const gap = 14;
  const room = 10;
  const dpr = window.devicePixelRatio || 1;
  const canvas = document.createElement('canvas');
  canvas.width = Math.ceil((gap + box.width + room) * dpr);
  canvas.height = Math.ceil((gap + box.height + room) * dpr);
  const g = canvas.getContext('2d');
  if (!g) return undefined;
  g.scale(dpr, dpr);
  g.beginPath();
  g.roundRect(gap + 0.5, gap + 0.5, box.width - 1, box.height - 1, look.radius);
  g.shadowColor = 'rgba(0, 0, 0, .18)';
  g.shadowBlur = 8;
  g.shadowOffsetY = 2;
  g.fillStyle = look.background;
  g.fill();
  g.shadowColor = 'transparent';
  g.strokeStyle = look.border;
  g.lineWidth = 1;
  g.stroke();
  g.font = look.font;
  g.fillStyle = look.color;
  g.textBaseline = 'middle';
  const max = box.width - look.left - look.right;
  let s = text;
  if (g.measureText(s).width > max) {
    while (s && g.measureText(s + '\u2026').width > max) s = s.slice(0, -1);
    s += '\u2026';
  }
  g.fillText(s, gap + look.left, gap + box.height / 2);
  return { png: canvas.toDataURL('image/png'), x: 0, y: 0 };
}
