// Shared spatial navigation for overlay screens (Details, Search, Settings).
//
// Every navigable surface routes controller/CEC/keyboard input the same way:
// direction keys walk real on-screen controls by geometry (see focusNav's
// `moveFocus`), Enter/A activates whatever the browser has focused, and B/Esc
// runs a screen-specific "back". Because movement follows actual layout, a
// control that sits to the right is reached by pressing Right — never by an
// index that happens to increment on Down. That geometric consistency is the
// whole point: one mental model across the OS.

import { MutableRefObject, RefObject, useEffect, useRef } from 'react';
import { activateFocused, moveFocus } from './focusNav';
import { NavAction } from './input';

export interface SpatialNavHandlers {
  /** B / Esc — screen decides (collapse a sub-view, pop a stack, or close). */
  onBack: () => void;
  /** A / Enter. Return `true` to suppress the default (click focused control). */
  onConfirm?: () => boolean | void;
  /** Pre-empt a direction press. Return `true` if fully handled (skips the
   *  default geometric move) — e.g. a screen that pages content itself. */
  onDirection?: (dir: 'up' | 'down' | 'left' | 'right') => boolean | void;
  /** While this returns true, all input is swallowed (e.g. a loading screen). */
  blocked?: () => boolean;
}

/**
 * Wires an overlay's forwarded `actionRef` (App funnels controller/keyboard nav
 * into it) to geometric focus movement inside `rootRef`. Handlers are read
 * through a ref so the latest closures are always used without re-registering.
 */
export function useSpatialNav(
  actionRef: MutableRefObject<((a: NavAction) => void) | null>,
  rootRef: RefObject<HTMLElement>,
  handlers: SpatialNavHandlers,
): void {
  const latest = useRef(handlers);
  latest.current = handlers;

  useEffect(() => {
    const handle = (action: NavAction) => {
      const h = latest.current;
      if (h.blocked?.()) return;
      if (action === 'back') {
        h.onBack();
        return;
      }
      if (action === 'confirm') {
        if (h.onConfirm?.() === true) return;
        activateFocused();
        return;
      }
      // Only the four directions remain relevant here; App handles the global
      // shortcuts (theme/enhance/settings/search) before forwarding to overlays.
      if (action !== 'up' && action !== 'down' && action !== 'left' && action !== 'right') return;
      if (h.onDirection?.(action) === true) return;
      const root = rootRef.current;
      if (root) moveFocus(root, action);
    };
    actionRef.current = handle;
    return () => {
      if (actionRef.current === handle) actionRef.current = null;
    };
  }, [actionRef, rootRef]);
}

/**
 * Focuses the element marked `data-primary` inside `root` (the screen's default
 * action), used to (re)anchor focus after an intentional context change — the
 * page opening, a stream list arriving, an episode picked. Returns whether it
 * found a target.
 */
export function focusPrimary(root: HTMLElement | null): boolean {
  const target = root?.querySelector<HTMLElement>('[data-primary]');
  if (target) {
    target.focus();
    return true;
  }
  return false;
}
