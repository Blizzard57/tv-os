import { useEffect, useState } from 'react';
import { Theme } from './theme';
import { TABS, TabId } from './tabs';

// The profile avatar on the far left is decorative chrome (like Google TV's
// account chip) and takes no focus. Every other control is a real focus target.

interface Props {
  activeTab: TabId;
  theme: Theme;
  /** Tabs that currently have content — others render dimmed. */
  enabled: Set<TabId>;
  onSearch: () => void;
  onSelectTab: (id: TabId) => void;
  onSettings: () => void;
  onToggleTheme: () => void;
  /** Focus reached a tab (arrowing across the bar switches to it live). */
  onFocusTab: (id: TabId, el: HTMLElement) => void;
  /** Focus reached the search pill or a right-side icon. */
  onFocusChrome: (el: HTMLElement) => void;
}

/** The Google-TV top navigation: a profile chip, a Search pill, the content
 *  tabs, then the clock, settings and a theme toggle. Controls are real focus
 *  targets (spatial nav walks them by geometry); App is told which one gained
 *  focus so it can preview the tab and remember the spot. The `:focus` state
 *  paints the highlight. */
export function TopBar({
  activeTab,
  theme,
  enabled,
  onSearch,
  onSelectTab,
  onSettings,
  onToggleTheme,
  onFocusTab,
  onFocusChrome,
}: Props) {
  return (
    <header className="topbar">
      <div className="top-avatar" aria-hidden="true">
        <span>K</span>
      </div>

      <button
        data-focus-key="chrome:search"
        className="top-pill top-search"
        onClick={onSearch}
        onFocus={(e) => onFocusChrome(e.currentTarget)}
        aria-label="Search"
      >
        <span className="top-search-glyph">⌕</span>
        <span className="top-search-label">Search</span>
      </button>

      <nav className="top-tabs">
        {TABS.map((t) => {
          const dim = !enabled.has(t.id) && t.id !== activeTab;
          return (
            <button
              key={t.id}
              data-tab={t.id}
              data-focus-key={`tab:${t.id}`}
              className={`top-tab ${t.id === activeTab ? 'top-tab-active' : ''} ${dim ? 'top-tab-dim' : ''}`}
              onClick={() => onSelectTab(t.id)}
              onFocus={(e) => onFocusTab(t.id, e.currentTarget)}
            >
              {t.label}
            </button>
          );
        })}
      </nav>

      <div className="top-right">
        <Clock />
        <button
          data-focus-key="chrome:settings"
          className="top-icon"
          onClick={onSettings}
          onFocus={(e) => onFocusChrome(e.currentTarget)}
          aria-label="Settings"
        >
          <span className="top-icon-glyph">⚙</span>
        </button>
        <button
          data-focus-key="chrome:theme"
          className="top-icon"
          onClick={onToggleTheme}
          onFocus={(e) => onFocusChrome(e.currentTarget)}
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
