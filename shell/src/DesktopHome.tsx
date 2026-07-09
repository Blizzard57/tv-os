import { useMemo, useState } from 'react';
import { ContentItem, Kind, Row } from './api';

// Kind filters shown in the sidebar (and used by chips via row titles).
const KIND_FILTERS: { id: Kind; label: string; icon: string }[] = [
  { id: 'movie', label: 'Movies', icon: '🎬' },
  { id: 'series', label: 'Shows', icon: '📺' },
  { id: 'game', label: 'Games', icon: '🎮' },
  { id: 'video', label: 'Videos', icon: '▶' },
];

const KIND_LABEL: Record<Kind, string> = {
  movie: 'Movie',
  series: 'Series',
  game: 'Game',
  video: 'Video',
};

interface Props {
  rows: Row[] | null;
  error: string | null;
  onOpen: (item: ContentItem) => void;
  onSearch: () => void;
  onSettings: () => void;
  onToggleTheme: () => void;
  onToggleMode: () => void;
}

/** Pointer-first home: top bar with search, collapsible sidebar, filter
 *  chips, and one big card grid over the entire library — every row the
 *  daemon knows, flattened, filterable by kind or by origin row.
 *
 *  CAVEAT: this layout is mouse/touch only — it has NO controller/d-pad
 *  focus navigation (unlike the TV home). A controller user who lands here
 *  after a mode switch can't move focus; the "TV mode" pill (and the S/T
 *  shortcuts handled upstream) are the way back to the navigable layout. */
export function DesktopHome({
  rows,
  error,
  onOpen,
  onSearch,
  onSettings,
  onToggleTheme,
  onToggleMode,
}: Props) {
  const [navOpen, setNavOpen] = useState(true);
  const [filter, setFilter] = useState<string>('All');

  // Flatten the rows once; remember each item's origin row for the sub-line.
  const entries = useMemo(() => {
    const seen = new Set<string>();
    const out: { item: ContentItem; origin: string }[] = [];
    for (const row of rows ?? []) {
      for (const item of row.items) {
        if (seen.has(item.id)) continue;
        seen.add(item.id);
        out.push({ item, origin: row.title });
      }
    }
    return out;
  }, [rows]);

  const rowTitles = useMemo(() => (rows ?? []).map((r) => r.title), [rows]);
  const isKind = (f: string): f is Kind => KIND_FILTERS.some((k) => k.id === f);
  const visible = useMemo(
    () =>
      entries.filter(({ item, origin }) =>
        filter === 'All' ? true : isKind(filter) ? item.kind === filter : origin === filter,
      ),
    [entries, filter],
  );

  const navItem = (
    icon: string,
    label: string,
    active: boolean,
    onClick: () => void,
    key?: string,
  ) => (
    <button key={key ?? label} className={`dt-nav-item ${active ? 'active' : ''}`} onClick={onClick}>
      <span className="dt-nav-icon">{icon}</span>
      <span className="dt-nav-label">{label}</span>
    </button>
  );

  return (
    <div className={`dt ${navOpen ? '' : 'dt-nav-closed'}`}>
      <header className="dt-topbar">
        <button
          className="dt-icon-btn"
          title="Toggle sidebar"
          onClick={() => setNavOpen((v) => !v)}
        >
          ☰
        </button>
        <div className="dt-logo" onClick={() => setFilter('All')}>
          <span className="dt-logo-mark">▶</span>
          TV OS
        </div>
        <button className="dt-searchbox" onClick={onSearch}>
          <span>⌕</span> Search everything — titles, actors, “k drama”, “time travel”…
          <span className="dt-kbd">/</span>
        </button>
        <div className="dt-top-actions">
          <button className="dt-icon-btn" title="Theme (T)" onClick={onToggleTheme}>
            ◐
          </button>
          <button className="dt-icon-btn" title="Settings (S)" onClick={onSettings}>
            ⚙
          </button>
          <button className="dt-pill" title="Switch to the 10-foot TV layout" onClick={onToggleMode}>
            📺 TV mode
          </button>
        </div>
      </header>

      <aside className="dt-nav">
        {navItem('⌂', 'Home', filter === 'All', () => setFilter('All'))}
        {KIND_FILTERS.map((k) => navItem(k.icon, k.label, filter === k.id, () => setFilter(k.id), k.id))}
        {rowTitles.length > 0 && (
          <>
            <div className="dt-nav-sep" />
            <div className="dt-nav-head">Your rows</div>
            {rowTitles.map((t, i) =>
              navItem('·', t, filter === t, () => setFilter(t), `row-${i}`),
            )}
          </>
        )}
        <div className="dt-nav-sep" />
        {navItem('⚙', 'Settings', false, onSettings)}
      </aside>

      <main className="dt-content">
        <div className="dt-chips">
          {['All', ...rowTitles].map((c, i) => (
            <button
              key={i === 0 ? 'all' : `chip-${i - 1}`}
              className={`dt-chip ${filter === c ? 'active' : ''}`}
              onClick={() => setFilter(c)}
            >
              {c}
            </button>
          ))}
        </div>

        {error && <div className="screen-message">{error}</div>}
        {!rows && !error && <div className="screen-message">Loading…</div>}
        {rows && visible.length === 0 && !error && (
          <div className="screen-message">
            Nothing here yet — open Settings to connect Steam, add a TMDB key, follow YouTube
            channels, or install an addon.
          </div>
        )}

        <div className="dt-grid">
          {visible.map(({ item, origin }) => (
            <div key={item.id} className="dt-card" onClick={() => onOpen(item)}>
              <div className={`dt-thumb ${item.kind === 'video' ? 'dt-thumb-wide' : ''}`}>
                {item.art ? (
                  <img src={item.art} alt={item.title} loading="lazy" />
                ) : (
                  <div className="card-placeholder">{item.title}</div>
                )}
                {item.action === 'install' && <span className="card-badge">INSTALL</span>}
              </div>
              <div className="dt-card-title">{item.title}</div>
              <div className="dt-card-sub">
                {KIND_LABEL[item.kind]} · {origin}
              </div>
            </div>
          ))}
        </div>
      </main>
    </div>
  );
}
