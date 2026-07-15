// Spatial focus movement for overlay panels (Settings): lets a d-pad / arrow
// keys walk every button, input and select like a native TV UI. All four
// directions use on-screen geometry, so moving right from the category rail
// enters the detail pane instead of walking down the DOM-order category list.

interface FocusCache {
  elements: HTMLElement[];
  rects: Map<HTMLElement, DOMRect>;
  dirty: boolean;
  observer: MutationObserver;
  resize: ResizeObserver;
}

const focusCache = new WeakMap<HTMLElement, FocusCache>();

function cachedLayout(root: HTMLElement): FocusCache {
  let cached = focusCache.get(root);
  if (!cached) {
    cached = {
      elements: [],
      rects: new Map(),
      dirty: true,
      observer: new MutationObserver(() => {
        const value = focusCache.get(root);
        if (value) value.dirty = true;
      }),
      resize: new ResizeObserver(() => {
        const value = focusCache.get(root);
        if (value) value.dirty = true;
      }),
    };
    cached.observer.observe(root, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: ['class', 'style', 'hidden', 'disabled', 'tabindex'],
    });
    cached.resize.observe(root);
    focusCache.set(root, cached);
  }
  if (cached.dirty) {
    cached.elements = Array.from(
      root.querySelectorAll<HTMLElement>('button, input, select, textarea, [tabindex]:not([tabindex="-1"])'),
    ).filter((el) => el.offsetParent !== null && !el.hasAttribute('disabled'));
    cached.rects = new Map(cached.elements.map((el) => [el, el.getBoundingClientRect()]));
    cached.dirty = false;
  }
  return cached;
}

/** Focusable controls inside `root`, in document order, visible only. */
export function focusables(root: HTMLElement): HTMLElement[] {
  return cachedLayout(root).elements;
}

/** Moves focus within `root` in the given direction; focuses the first
 *  control when nothing inside is focused yet. Pass `smooth=false` for
 *  rapid held-repeat steps so overlapping smooth scrolls don't queue up
 *  and jank; defaults to smooth to preserve existing callers. */
export function moveFocus(
  root: HTMLElement,
  dir: 'up' | 'down' | 'left' | 'right',
  smooth = false,
): boolean {
  const layout = cachedLayout(root);
  const els = layout.elements;
  if (els.length === 0) return false;
  const active = document.activeElement as HTMLElement | null;
  const current = active && els.indexOf(active);
  let next: HTMLElement | undefined;

  if (current === null || current === -1) {
    next = els[0];
  } else {
    const rect = layout.rects.get(els[current]) ?? els[current].getBoundingClientRect();
    const centerX = rect.left + rect.width / 2;
    const centerY = rect.top + rect.height / 2;
    const candidates = els
      .map((el) => ({ el, r: layout.rects.get(el) ?? el.getBoundingClientRect() }))
      .filter(({ el, r }) => {
        if (el === active) return false;
        if (dir === 'down') return r.top >= rect.bottom - 1;
        if (dir === 'up') return r.bottom <= rect.top + 1;
        // Horizontal movement stays in the same visual row. Without this,
        // pressing Left on a detail control can jump to an unrelated control
        // several sections above instead of returning to the category rail.
        const overlapsRow = r.top < rect.bottom && r.bottom > rect.top;
        if (dir === 'right') return overlapsRow && r.left >= rect.right - 1;
        return overlapsRow && r.right <= rect.left + 1;
      });

    // Prefer the nearest element along the requested axis, then the element
    // whose centre is closest on the perpendicular axis. A weighted score
    // keeps movement predictable across the settings rail and grouped cards.
    candidates.sort((a, b) => {
      const score = ({ r }: { r: DOMRect }) => {
        const horizontal = dir === 'left' || dir === 'right';
        const primary = horizontal
          ? dir === 'right'
            ? r.left - rect.right
            : rect.left - r.right
          : dir === 'down'
            ? r.top - rect.bottom
            : rect.top - r.bottom;
        const perpendicular = horizontal
          ? Math.abs(r.top + r.height / 2 - centerY)
          : Math.abs(r.left + r.width / 2 - centerX);
        return primary * 4 + perpendicular;
      };
      return score(a) - score(b);
    });
    next = candidates[0]?.el;
  }

  if (next && next !== active) {
    next.focus();
    next.scrollIntoView({ block: 'nearest', behavior: smooth ? 'smooth' : 'auto' });
    return true;
  }
  return false;
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
