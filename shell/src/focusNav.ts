// Spatial focus movement for overlay panels (Settings): lets a d-pad / arrow
// keys walk every button, input and select like a native TV UI. Left/Right
// step linearly (covers horizontal groups like the accent swatches); Up/Down
// jump to the nearest element in the previous/next visual row.

/** Focusable controls inside `root`, in document order, visible only. */
export function focusables(root: HTMLElement): HTMLElement[] {
  return Array.from(
    root.querySelectorAll<HTMLElement>('button, input, select, [tabindex]'),
  ).filter((el) => el.offsetParent !== null && !el.hasAttribute('disabled'));
}

/** Moves focus within `root` in the given direction; focuses the first
 *  control when nothing inside is focused yet. */
export function moveFocus(root: HTMLElement, dir: 'up' | 'down' | 'left' | 'right'): void {
  const els = focusables(root);
  if (els.length === 0) return;
  const active = document.activeElement as HTMLElement | null;
  const current = active && els.indexOf(active);
  let next: HTMLElement | undefined;

  if (current === null || current === -1) {
    next = els[0];
  } else if (dir === 'left' || dir === 'right') {
    next = els[Math.min(els.length - 1, Math.max(0, current + (dir === 'right' ? 1 : -1)))];
  } else {
    const rect = els[current].getBoundingClientRect();
    const centerX = rect.left + rect.width / 2;
    const candidates = els
      .map((el) => ({ el, r: el.getBoundingClientRect() }))
      .filter(({ r }) => (dir === 'down' ? r.top > rect.bottom - 1 : r.bottom < rect.top + 1));
    // Nearest row first; within (roughly) the same row, nearest horizontally.
    candidates.sort((a, b) => {
      const rowDist = (c: { r: DOMRect }) =>
        dir === 'down' ? c.r.top - rect.bottom : rect.top - c.r.bottom;
      const d = rowDist(a) - rowDist(b);
      if (Math.abs(d) > 4) return d;
      const xDist = (c: { r: DOMRect }) => Math.abs(c.r.left + c.r.width / 2 - centerX);
      return xDist(a) - xDist(b);
    });
    next = candidates[0]?.el;
  }

  if (next && next !== active) {
    next.focus();
    next.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }
}

/** Activates the focused control the way A/Enter should on a TV: buttons and
 *  checkboxes are clicked, inputs just keep focus. Selects are handled by the
 *  caller as an edit mode (see [`stepSelect`]) so scrolling past one never
 *  changes its value. */
export function activateFocused(): void {
  const active = document.activeElement as HTMLElement | null;
  if (!active || active.tagName === 'SELECT') return;
  active.click();
}

/** Moves a `<select>` by one option (`dir` = -1 previous / +1 next), clamped,
 *  firing `change` so React updates. Used while a select is in edit mode. */
export function stepSelect(select: HTMLSelectElement, dir: -1 | 1): void {
  const next = select.selectedIndex + dir;
  if (next < 0 || next >= select.options.length) return;
  select.selectedIndex = next;
  select.dispatchEvent(new Event('change', { bubbles: true }));
}
