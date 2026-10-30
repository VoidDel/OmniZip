// Box selection with the mouse (docs/04-ui-design.md §5): dragging from blank space in the file
// list (in the table also from a row outside its name) draws a rectangle, and the items it
// touches are selected as it changes. Near the top or bottom edge the list scrolls on. The list
// is virtual, so the caller finds the items by their layout, not by their elements.

import { type RefObject, useEffect, useRef, useState } from 'react';

import { useEvent } from '../../lib/hooks';

/** A rectangle in the coordinates of an element's content. */
export interface Rect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** Distance the pointer moves before a press becomes a box selection. */
const THRESHOLD = 4;
/** Band at the top and bottom of the list where the pointer scrolls it. */
const EDGE = 36;

/**
 * `scroll`: the scrolling list (positioned, the box is drawn in it). `items`: the element the
 * items are laid out in. `hit`: the item indices a rectangle in `items` coordinates touches.
 * `apply`: selects those (called on every change).
 */
export function useMarquee(
  scroll: RefObject<HTMLElement | null>,
  items: () => HTMLElement | null,
  hit: (r: Rect) => number[],
  apply: (indices: number[]) => void,
) {
  /** The box in `scroll` content coordinates, while one is drawn. */
  const [box, setBox] = useState<Rect | null>(null);
  /** Set when a press became a box selection, for the click that ends it to be ignored. */
  const happened = useRef(false);
  const stop = useRef<(() => void) | null>(null);
  useEffect(() => () => stop.current?.(), []);
  const hitNow = useEvent(hit);
  const applyNow = useEvent(apply);

  /** Watches a press; it turns into a box selection once the pointer moves. */
  const press = useEvent((e: React.PointerEvent) => {
    const sc = scroll.current;
    if (e.button !== 0 || !sc) return;
    happened.current = false;
    // Content coordinates of a point on screen (they stay put while the list scrolls).
    const inContent = (x: number, y: number) => {
      const r = sc.getBoundingClientRect();
      return { x: x - r.left - sc.clientLeft + sc.scrollLeft, y: y - r.top - sc.clientTop + sc.scrollTop };
    };
    const from = inContent(e.clientX, e.clientY);
    let pointer = { x: e.clientX, y: e.clientY };
    let active = false;
    let frame = 0;

    const update = () => {
      const to = inContent(pointer.x, pointer.y);
      const el = items();
      // Drawn within the list's content (beyond it the box would make the list scroll further).
      const height = el ? Math.max(sc.clientHeight, el.offsetTop + el.offsetHeight) : sc.clientHeight;
      const r = {
        left: Math.max(0, Math.min(from.x, to.x)),
        top: Math.max(0, Math.min(from.y, to.y)),
        right: Math.min(sc.clientWidth, Math.max(from.x, to.x)),
        bottom: Math.min(height, Math.max(from.y, to.y)),
      };
      setBox(r);
      if (!el) return applyNow([]);
      applyNow(hitNow({ left: r.left - el.offsetLeft, top: r.top - el.offsetTop, right: r.right - el.offsetLeft, bottom: r.bottom - el.offsetTop }));
    };
    const tick = () => {
      frame = 0;
      const r = sc.getBoundingClientRect();
      const over = pointer.y < r.top + EDGE ? pointer.y - (r.top + EDGE) : pointer.y > r.bottom - EDGE ? pointer.y - (r.bottom - EDGE) : 0;
      const before = sc.scrollTop;
      if (over) sc.scrollTop += Math.max(-40, Math.min(40, over / 2));
      update();
      // Keeps scrolling while the pointer stays in the band, until the list cannot go further.
      if (over && sc.scrollTop !== before) frame = requestAnimationFrame(tick);
    };
    const schedule = () => {
      if (active && !frame) frame = requestAnimationFrame(tick);
    };
    const onMove = (ev: PointerEvent) => {
      pointer = { x: ev.clientX, y: ev.clientY };
      if (!active) {
        if (Math.hypot(ev.clientX - e.clientX, ev.clientY - e.clientY) < THRESHOLD) return;
        active = true;
        happened.current = true;
        window.getSelection()?.removeAllRanges();
        document.body.classList.add('selecting');
      }
      schedule();
    };
    const end = () => {
      // The click that follows the release still sees the flag; later ones do not.
      if (active) setTimeout(() => (happened.current = false), 0);
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', end);
      window.removeEventListener('pointercancel', end);
      sc.removeEventListener('scroll', schedule);
      cancelAnimationFrame(frame);
      document.body.classList.remove('selecting');
      setBox(null);
      stop.current = null;
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', end);
    window.addEventListener('pointercancel', end);
    // Scrolling with the wheel while selecting moves the box's far corner too.
    sc.addEventListener('scroll', schedule);
    stop.current = end;
  });

  const element = box && (
    <div
      className="marquee"
      aria-hidden="true"
      style={{ left: box.left, top: box.top, width: box.right - box.left, height: box.bottom - box.top }}
    />
  );
  return { press, element, happened };
}
