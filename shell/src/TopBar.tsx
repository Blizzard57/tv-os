import { useEffect, useState } from 'react';
import { Theme } from './theme';
import { TABS, TabId } from './tabs';

// Top-bar focus order: [search, ...tabs, settings, theme]. App drives focus
// with a single index into this order; these helpers keep App and TopBar in
// agreement about what each index means. The profile avatar on the far left is
// decorative chrome (like Google TV's account chip) and takes no focus.
export const SEARCH_INDEX = 0;
export const FIRST_TAB_INDEX = 1;
export const SETTINGS_INDEX = FIRST_TAB_INDEX + TABS.length;
export const THEME_INDEX = SETTINGS_INDEX + 1;
export const TOPBAR_COUNT = THEME_INDEX + 1;

/** The topbar index that selects a given tab (so B/Home can land on it). */
export const tabIndex = (id: TabId): number =>
  FIRST_TAB_INDEX + TABS.findIndex((t) => t.id === id);

interface Props {
  activeTab: TabId;
  /** Focused topbar index, or null when focus is down in the rows. */
  focusIndex: number | null;
  theme: Theme;
  /** Tabs that currently have content — others render dimmed. */
  enabled: Set<TabId>;
  onSearch: () => void;
  onSelectTab: (id: TabId) => void;
  onSettings: () => void;
  onToggleTheme: () => void;
}

/** The Google-TV top navigation: a profile chip, a Search pill, the content
 *  tabs, then the clock, settings and a theme toggle. Purely presentational —
 *  App owns focus and routing; clicks call the same handlers a controller's
 *  confirm would. */
export function TopBar({
  activeTab,
  focusIndex,
  theme,
  enabled,
  onSearch,
  onSelectTab,
  onSettings,
  onToggleTheme,
}: Props) {
  const f = (i: number) => (focusIndex === i ? 'top-focused' : '');
  return (
    <header className="topbar">
      <div className="top-avatar" aria-hidden="true">
        <span>K</span>
      </div>

      <button
        className={`top-pill top-search ${f(SEARCH_INDEX)}`}
        onClick={onSearch}
        aria-label="Search"
      >
        <span className="top-search-glyph">⌕</span>
        <span className="top-search-label">Search</span>
      </button>

      <nav className="top-tabs">
        {TABS.map((t, i) => {
          const idx = FIRST_TAB_INDEX + i;
          const dim = !enabled.has(t.id) && t.id !== activeTab;
          return (
            <button
              key={t.id}
              className={`top-tab ${t.id === activeTab ? 'top-tab-active' : ''} ${dim ? 'top-tab-dim' : ''} ${f(idx)}`}
              onClick={() => onSelectTab(t.id)}
            >
              {t.label}
            </button>
          );
        })}
      </nav>

      <div className="top-right">
        <Clock />
        <button
          className={`top-icon ${f(SETTINGS_INDEX)}`}
          onClick={onSettings}
          aria-label="Settings"
        >
          <span className="top-icon-glyph">⚙</span>
        </button>
        <button
          className={`top-icon ${f(THEME_INDEX)}`}
          onClick={onToggleTheme}
          aria-label={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
        >
          <span className="top-icon-glyph">{theme === 'dark' ? '◐' : '◑'}</span>
        </button>
      </div>
    </header>
  );
}

/** The living-room clock — a TV home screen should always show the time. */
function Clock() {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    let timer: number;
    const schedule = () => {
      window.clearTimeout(timer);
      if (document.hidden) return;
      const current = new Date();
      setNow(current);
      timer = window.setTimeout(
        schedule,
        60_000 - current.getSeconds() * 1000 - current.getMilliseconds(),
      );
    };
    const onVisibilityChange = () => schedule();
    schedule();
    document.addEventListener('visibilitychange', onVisibilityChange);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener('visibilitychange', onVisibilityChange);
    };
  }, []);
  return (
    <div className="top-clock">
      {now.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
    </div>
  );
}
