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
