// TV input: one hook that turns every supported device into NavActions.
//
//   keyboard  — arrow keys / Enter / Escape. CEC TV remotes arrive as
//               keyboard keys too, so this covers the remote. OS key-repeat
//               provides hold-to-scroll for free.
//   gamepad   — polled via the Gamepad API: d-pad + left stick to move,
//               A (south) to confirm, B (east) to go back. Held directions
//               repeat with a typical TV cadence (delay, then steady rate).

import { useEffect, useRef } from 'react';

export type NavAction =
  | 'up'
  | 'down'
  | 'left'
  | 'right'
  | 'confirm'
  | 'back'
  | 'theme'
  | 'enhance'
  | 'settings'
  | 'search';

const KEY_MAP: Record<string, NavAction> = {
  ArrowUp: 'up',
  ArrowDown: 'down',
  ArrowLeft: 'left',
  ArrowRight: 'right',
  Enter: 'confirm',
  Escape: 'back',
  Backspace: 'back',
  t: 'theme',
  T: 'theme',
  e: 'enhance',
  E: 'enhance',
  s: 'settings',
  S: 'settings',
  '/': 'search',
};

// Standard-mapping gamepad button indices.
const BUTTON_A = 0;
const BUTTON_B = 1;
const BUTTON_X = 2; // west button: cycles the Enhance (upscaling) mode
const BUTTON_Y = 3; // north button: toggles light/dark theme
const BUTTON_START = 9; // menu/start button: opens Settings
const DPAD: [number, NavAction][] = [
  [12, 'up'],
  [13, 'down'],
  [14, 'left'],
  [15, 'right'],
];

const STICK_THRESHOLD = 0.5;
const REPEAT_DELAY_MS = 400;
const REPEAT_RATE_MS = 130;

export function useTvInput(onAction: (action: NavAction) => void) {
  // Keep the latest handler in a ref so listeners are registered only once.
  const handler = useRef(onAction);
  handler.current = onAction;

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      // Form fields (the Settings panel) need care so the d-pad behaves like a
      // TV, not a desktop form:
      //   * text inputs keep native typing + Left/Right cursor; only Up/Down
      //     escape to move focus to the next field.
      //   * checkboxes/selects are driven entirely by our nav — Arrows move
      //     focus, Enter activates (a select enters "edit mode"; see
      //     focusNav/SettingsPanel). The browser must NOT change a select's
      //     value while you're merely scrolling past it.
      const el = e.target as HTMLElement | null;
      const tag = el?.tagName;
      const key = e.key;
      const isArrow = key.startsWith('Arrow');
      if (tag === 'TEXTAREA') return;
      if (tag === 'INPUT') {
        const type = (el as HTMLInputElement).type;
        const textual = type !== 'checkbox' && type !== 'radio' && type !== 'button';
        if (textual && key !== 'ArrowUp' && key !== 'ArrowDown') return;
        if (!textual && !isArrow && key !== 'Enter') return;
      }
      if (tag === 'SELECT' && !isArrow && key !== 'Enter') return;
      const action = KEY_MAP[e.key];
      if (action) {
        e.preventDefault();
        handler.current(action);
      }
    };
    window.addEventListener('keydown', onKeyDown);

    // Gamepad state lives across frames: which direction is held and when it
    // may fire again, plus previous button states for edge detection.
    let heldDirection: NavAction | null = null;
    let nextRepeatAt = 0;
    let aWasDown = false;
    let bWasDown = false;
    let xWasDown = false;
    let yWasDown = false;
    let startWasDown = false;
    let frame = 0;

    const poll = (now: number) => {
      frame = requestAnimationFrame(poll);
      const pad = navigator.getGamepads().find((p) => p?.connected);
      if (!pad) return;

      const direction =
        DPAD.find(([i]) => pad.buttons[i]?.pressed)?.[1] ??
        axisDirection(pad.axes[0], pad.axes[1]);

      if (direction !== heldDirection) {
        heldDirection = direction;
        if (direction) {
          handler.current(direction);
          nextRepeatAt = now + REPEAT_DELAY_MS;
        }
      } else if (direction && now >= nextRepeatAt) {
        handler.current(direction);
        nextRepeatAt = now + REPEAT_RATE_MS;
      }

      const aDown = pad.buttons[BUTTON_A]?.pressed ?? false;
      const bDown = pad.buttons[BUTTON_B]?.pressed ?? false;
      const xDown = pad.buttons[BUTTON_X]?.pressed ?? false;
      const yDown = pad.buttons[BUTTON_Y]?.pressed ?? false;
      const startDown = pad.buttons[BUTTON_START]?.pressed ?? false;
      if (aDown && !aWasDown) handler.current('confirm');
      if (bDown && !bWasDown) handler.current('back');
      if (xDown && !xWasDown) handler.current('enhance');
      if (yDown && !yWasDown) handler.current('theme');
      if (startDown && !startWasDown) handler.current('settings');
      aWasDown = aDown;
      bWasDown = bDown;
      xWasDown = xDown;
      yWasDown = yDown;
      startWasDown = startDown;
    };
    frame = requestAnimationFrame(poll);

    return () => {
      window.removeEventListener('keydown', onKeyDown);
      cancelAnimationFrame(frame);
    };
  }, []);
}

function axisDirection(x = 0, y = 0): NavAction | null {
  if (Math.abs(x) < STICK_THRESHOLD && Math.abs(y) < STICK_THRESHOLD) return null;
  if (Math.abs(x) > Math.abs(y)) return x > 0 ? 'right' : 'left';
  return y > 0 ? 'down' : 'up';
}
