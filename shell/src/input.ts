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

// Deadzone hysteresis: cross ENTER to engage a direction, and the stick must
// fall back below RELEASE before it lets go. A resting/worn stick idling near
// the edge therefore can't flicker and phantom-scroll.
const STICK_ENTER = 0.5;
const STICK_RELEASE = 0.3;
const REPEAT_DELAY_MS = 400;
const REPEAT_RATE_MS = 130;

// Directional actions may auto-repeat (hold-to-scroll); one-shot actions
// (confirm/back/settings/…) must fire once per physical press, so we drop
// OS/CEC key-repeat events for them.
const REPEATABLE: Record<NavAction, boolean> = {
  up: true,
  down: true,
  left: true,
  right: true,
  confirm: false,
  back: false,
  theme: false,
  enhance: false,
  settings: false,
  search: false,
};

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
        // Ignore held-key auto-repeat for one-shot actions so holding Enter/
        // Esc doesn't over-push/over-pop the nav stack. Directional keys keep
        // their hold-to-scroll repeat.
        if (e.repeat && !REPEATABLE[action]) return;
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
    // Remembered axis direction for deadzone hysteresis (see axisDirection).
    let axisHeld: NavAction | null = null;

    // Clears all held/edge state so a fresh (or reconnected) pad starts clean
    // and can't leave a phantom held direction or a stuck button edge.
    const resetPadState = () => {
      heldDirection = null;
      axisHeld = null;
      aWasDown = bWasDown = xWasDown = yWasDown = startWasDown = false;
    };

    const poll = (now: number) => {
      frame = requestAnimationFrame(poll);
      const pad = navigator.getGamepads().find((p) => p?.connected);
      if (!pad) {
        resetPadState();
        return;
      }

      const dpadDir = DPAD.find(([i]) => pad.buttons[i]?.pressed)?.[1] ?? null;
      // The d-pad wins; only fall back to the stick, and reset stick hysteresis
      // whenever the d-pad is driving so releasing it doesn't linger.
      if (dpadDir) axisHeld = null;
      const stickDir = dpadDir ? null : axisDirection(pad.axes[0], pad.axes[1], axisHeld);
      axisHeld = dpadDir ? null : stickDir;
      const direction = dpadDir ?? stickDir;

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

    // Nice-to-have: clear stale held state the moment a pad is (un)plugged.
    const onGamepadChange = () => resetPadState();
    window.addEventListener('gamepadconnected', onGamepadChange);
    window.addEventListener('gamepaddisconnected', onGamepadChange);

    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('gamepadconnected', onGamepadChange);
      window.removeEventListener('gamepaddisconnected', onGamepadChange);
      cancelAnimationFrame(frame);
    };
  }, []);
}

// Deadzone hysteresis: `held` is the direction currently engaged (or null).
// Engaging needs an axis past STICK_ENTER; staying engaged only needs it above
// STICK_RELEASE, so a stick resting just under the enter threshold — or a worn
// one jittering around it — won't flicker on and off.
function axisDirection(x = 0, y = 0, held: NavAction | null = null): NavAction | null {
  const threshold = held ? STICK_RELEASE : STICK_ENTER;
  if (Math.abs(x) < threshold && Math.abs(y) < threshold) return null;
  if (Math.abs(x) > Math.abs(y)) return x > 0 ? 'right' : 'left';
  return y > 0 ? 'down' : 'up';
}
