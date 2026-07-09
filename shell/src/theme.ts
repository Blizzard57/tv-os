// Dark/light theme: applied as data-theme on <html> (styles.css defines the
// variables for each), persisted to localStorage, defaulting to the system
// preference on first run.

export type Theme = 'dark' | 'light';

const STORAGE_KEY = 'tvos-theme';

export function initialTheme(): Theme {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved === 'dark' || saved === 'light') return saved;
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  localStorage.setItem(STORAGE_KEY, theme);
}

export const otherTheme = (theme: Theme): Theme => (theme === 'dark' ? 'light' : 'dark');

// ---- Accent color (personalization) ----

// Google-TV's signature focus blue is the default. The picker (Settings) still
// lets you change it; "" in settings falls back to this.
export const DEFAULT_ACCENT = '#8ab4f8';

/** Curated accent choices shown as swatches in Settings. */
export const ACCENT_PRESETS = [
  '#8ab4f8', // Google-TV blue (default)
  '#4285f4', // Google blue
  '#8b5cf6', // violet
  '#ec4899', // pink
  '#f43f5e', // red
  '#f59e0b', // amber
  '#22c55e', // green
  '#14b8a6', // teal
];

/** Sets the system accent CSS variable (and a translucent glow derived from it). */
export function applyAccent(accent: string): void {
  // Validate once so --accent and --accent-glow can never disagree: an invalid
  // value must fall back to the default for BOTH, not set --accent to garbage
  // while the glow silently reverts to violet.
  const trimmed = accent?.trim() ?? '';
  const rgb = parseHex(trimmed);
  const color = rgb ? trimmed : DEFAULT_ACCENT;
  const [r, g, b] = rgb ?? parseHex(DEFAULT_ACCENT)!;
  const root = document.documentElement;
  root.style.setProperty('--accent', color);
  root.style.setProperty('--accent-glow', `rgba(${r}, ${g}, ${b}, 0.35)`);
  // Legible text/icon color to sit ON an accent fill (buttons, badges, selected
  // pills). Google-TV's default light-blue wants near-black; a deep accent wants
  // white. Pick by perceived luminance so any picked colour stays readable.
  const luminance = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
  root.style.setProperty('--on-accent', luminance > 0.6 ? '#06121f' : '#ffffff');
}

/** Parses `#rgb`/`#rrggbb` into [r,g,b], or null if it isn't a valid hex. */
function parseHex(hex: string): [number, number, number] | null {
  const m = hex.replace('#', '');
  const full = m.length === 3 ? m.split('').map((c) => c + c).join('') : m;
  if (full.length !== 6 || !/^[0-9a-fA-F]{6}$/.test(full)) return null;
  const n = parseInt(full, 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}
