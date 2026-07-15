import { MutableRefObject, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ContentItem, Row, searchCatalog, searchDeep } from './api';
import { activateFocused, moveFocus } from './focusNav';
import { NavAction } from './input';

// On-screen keyboard layout: four 9-key rows, then a special row. A physical
// keyboard still types straight into the query; this exists so a controller
// or TV remote can search without one.
const KB_ROWS = ['abcdefghi', 'jklmnopqr', 'stuvwxyz0', '123456789'];
const SPECIAL_ROW = 4;
const SPECIALS = ['Space', '⌫ Delete', 'Clear', '🔍 Search all'] as const;
const SEARCH_ALL_KEY = 3;
const FILTERS = [
  ['all', 'All'], ['movie', 'Movies'], ['series', 'Shows'], ['creators', 'Creators'],
  ['live', 'Sports'], ['game', 'Games'],
] as const;
type SearchFilter = typeof FILTERS[number][0];

/** Map a column between two rows of differing length by relative position, so
 *  moving up/down between rows lands under (roughly) the same spot instead of
 *  jumping via magic multipliers. */
export function mapCol(col: number, fromLen: number, toLen: number): number {
  if (fromLen <= 1) return 0;
  return Math.round((col / (fromLen - 1)) * (toLen - 1));
}

// Items whose id belongs to a store/local source are things you already have —
// their badge says what pressing A does (play/install) rather than what it is.
const isLibraryItem = (item: ContentItem) => !/^(tmdb|strm):/.test(item.id);

function badgeFor(item: ContentItem): { label: string; owned: boolean } {
  if (isLibraryItem(item)) {
    return { label: item.action === 'install' ? 'Install' : 'Play', owned: true };
  }
  return { label: item.kind, owned: false };
}

interface Props {
  onClose: () => void;
  onPick: (item: ContentItem) => void;
  /** App writes its forwarded nav handler here (gamepad d-pad / A / B). */
  actionRef: MutableRefObject<((a: NavAction) => void) | null>;
}

/** One result. Videos get wide 16:9 art (their thumbnails are landscape);
 *  a broken artwork URL falls back to the titled placeholder, never alt text. */
function ResultCard({
  item,
  onPick,
}: {
  item: ContentItem;
  onPick: (item: ContentItem) => void;
}) {
  const [artFailed, setArtFailed] = useState(false);
  const badge = badgeFor(item);
  // Google TV shows results as wide 16:9 cards (art_of now feeds landscape
  // backdrops), so every search card is landscape. A real tab stop (tabIndex)
  // so spatial nav (moveFocus) can walk it; the `:focus` state paints it.
  return (
    <div className="search-card" tabIndex={0} onClick={() => onPick(item)}>
      <div className="search-art">
        {item.art && !artFailed ? (
          <img
            src={item.art}
            alt={item.title}
            loading="lazy"
            onError={() => setArtFailed(true)}
          />
        ) : (
          <div className="card-placeholder">{item.title}</div>
        )}
        <span className={`card-badge ${badge.owned ? '' : 'card-badge-progress'}`}>
          {badge.label.toUpperCase()}
        </span>
      </div>
      <div className="search-card-title">{item.title}</div>
    </div>
  );
}

/** Full-screen search. Typing gives instant title matches; Enter (or the
 *  "Search all" key) runs the deep search over the entire space — actors,
 *  plot keywords, genres, region idioms — shown as browsable sections. */
export function SearchOverlay({ onClose, onPick, actionRef }: Props) {
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<ContentItem[]>([]);
  // Deep search: sections replace the keyboard + quick grid until dismissed.
  const [deepRows, setDeepRows] = useState<Row[] | null>(null);
  const [deepBusy, setDeepBusy] = useState(false);
  const [filter, setFilter] = useState<SearchFilter>('all');
  const [recent, setRecent] = useState<string[]>(() => {
    try { return JSON.parse(localStorage.getItem('tvos.recentSearches') || '[]').slice(0, 6); }
    catch { return []; }
  });
  const inputRef = useRef<HTMLInputElement>(null);
  // The whole overlay; spatial nav (moveFocus) walks its focusable controls.
  const rootRef = useRef<HTMLDivElement>(null);
  // Monotonic request ids so an older in-flight fetch can't clobber newer
  // results (the debounce only clears the timer, not the pending request).
  const quickToken = useRef(0);
  const deepToken = useRef(0);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Debounced quick search (titles) while typing.
  useEffect(() => {
    const handle = window.setTimeout(() => {
      // Invalidate any earlier in-flight quick search regardless of length.
      const token = ++quickToken.current;
      if (query.trim().length < 2) {
        setResults([]);
        return;
      }
      searchCatalog(query)
        .then((r) => {
          if (token !== quickToken.current) return; // a newer query superseded us
          setResults(r);
        })
        .catch(() => {
          if (token !== quickToken.current) return;
          setResults([]);
        });
    }, 250);
    return () => window.clearTimeout(handle);
  }, [query]);

  // Editing the query invalidates a previous deep search (and any in-flight one).
  useEffect(() => {
    deepToken.current++;
    setDeepRows(null);
    setDeepBusy(false);
  }, [query]);

  const runDeep = useCallback(() => {
    const q = query.trim();
    if (q.length < 2 || deepBusy) return;
    inputRef.current?.blur();
    setDeepBusy(true);
    const nextRecent = [q, ...recent.filter((v) => v.toLowerCase() !== q.toLowerCase())].slice(0, 6);
    setRecent(nextRecent);
    localStorage.setItem('tvos.recentSearches', JSON.stringify(nextRecent));
    const token = ++deepToken.current;
    searchDeep(q)
      .then((rows) => {
        if (token !== deepToken.current) return; // query changed / newer deep search
        // Empty sections cannot hold focus, so keep them out of the UI entirely.
        const navigableRows = rows.filter((row) => row.items.length > 0);
        setDeepRows(navigableRows);
      })
      .catch(() => {
        if (token !== deepToken.current) return;
        setDeepRows([]);
      })
      .finally(() => {
        if (token === deepToken.current) setDeepBusy(false);
      });
  }, [query, deepBusy, recent]);

  const exitDeep = useCallback(() => {
    setDeepRows(null);
  }, []);

  const pressKey = useCallback(
    (row: number, col: number) => {
      if (row === SPECIAL_ROW) {
        if (col === 0) setQuery((q) => q + ' ');
        else if (col === 1) setQuery((q) => q.slice(0, -1));
        else if (col === 2) setQuery('');
        else runDeep();
        return;
      }
      const ch = KB_ROWS[row]?.[col];
      if (ch) setQuery((q) => q + ch);
    },
    [runDeep],
  );

  // Spatial nav: directions walk the overlay's real controls by geometry, A
  // activates the focused one, B peels a layer. Assigned to `actionRef` for the
  // gamepad and called from the capture keydown listener for keyboard/CEC.
  const navHandle = useCallback(
    (action: NavAction) => {
      if (deepBusy) return; // nothing focusable behind the spinner
      if (action === 'back') {
        if (deepRows) exitDeep();
        else onClose();
        return;
      }
      if (action === 'confirm') {
        // Enter on the query line runs the deep search; anywhere else it
        // activates the focused key or result card.
        if (document.activeElement === inputRef.current) runDeep();
        else activateFocused();
        return;
      }
      if (action === 'up' || action === 'down' || action === 'left' || action === 'right') {
        // The query line spans the full width, so geometry alone would send
        // Down to whichever key sits under its centre (a right-ish letter).
        // Anchor Down from the query line to the first control below it instead.
        if (action === 'down' && document.activeElement === inputRef.current) {
          const first = rootRef.current?.querySelector<HTMLElement>('.search-filter, .osk-key, .search-card');
          if (first) {
            first.focus();
            return;
          }
        }
        if (rootRef.current) moveFocus(rootRef.current, action);
      }
    },
    [deepBusy, deepRows, exitDeep, onClose, runDeep],
  );

  // Gamepad path: App forwards d-pad / A / B here while the overlay is open.
  useEffect(() => {
    actionRef.current = navHandle;
    return () => {
      if (actionRef.current === navHandle) actionRef.current = null;
    };
  }, [actionRef, navHandle]);

  // Physical keyboard path (capture phase so the home grid never moves and
  // typing lands here first). Letters/Backspace edit the query from anywhere —
  // a keyboard user never has to return to the input line — while arrows/Enter
  // drive the same spatial nav as the controller.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        navHandle('back');
        return;
      }
      const inInput = document.activeElement === inputRef.current;
      const nav: Record<string, NavAction> = {
        ArrowUp: 'up',
        ArrowDown: 'down',
        ArrowLeft: 'left',
        ArrowRight: 'right',
        Enter: 'confirm',
      };
      const action = nav[e.key];
      if (action) {
        // In the query line, left/right must keep moving the caret.
        if (inInput && (action === 'left' || action === 'right')) return;
        e.stopPropagation();
        e.preventDefault();
        navHandle(action);
        return;
      }
      if (!inInput) {
        if (e.key === 'Backspace') {
          e.stopPropagation();
          e.preventDefault();
          setQuery((q) => q.slice(0, -1));
        } else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
          e.stopPropagation();
          e.preventDefault();
          setQuery((q) => q + e.key);
        }
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [navHandle]);

  // Re-anchor focus when deep sections appear or clear: land on the first
  // result so ◀▶ browses immediately; drop back to the query line when a
  // search finds nothing, or is dismissed (deepRows → null), so the user can
  // refine it rather than losing focus to nowhere.
  useEffect(() => {
    const firstCard =
      deepRows && deepRows.length > 0
        ? rootRef.current?.querySelector<HTMLElement>('.search-card')
        : null;
    (firstCard ?? inputRef.current)?.focus();
  }, [deepRows]);

  const renderCard = (item: ContentItem) => (
    <ResultCard key={item.id} item={item} onPick={onPick} />
  );

  const deepView = deepRows !== null;
  const matchesFilter = useCallback((item: ContentItem) => {
    if (filter === 'all') return true;
    if (filter === 'creators') return item.domain === 'youtube' || item.domain === 'twitch' || item.id.startsWith('yt:') || item.id.startsWith('twitch:');
    return item.kind === filter;
  }, [filter]);
  const visibleResults = results.filter(matchesFilter);
  const visibleDeepRows = deepRows?.map((row) => ({ ...row, items: row.items.filter(matchesFilter) })).filter((row) => row.items.length > 0) ?? null;

  return (
    <div className="search-scrim" onClick={onClose}>
      <div className="search" ref={rootRef} onClick={(e) => e.stopPropagation()}>
        <input
          ref={inputRef}
          className="search-input"
          value={query}
          placeholder="Search anything — a title, an actor, “k drama”, “time travel”…"
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="search-filters" aria-label="Search filters">
          {FILTERS.map(([id, label]) => (
            <button key={id} className={`search-filter ${filter === id ? 'active' : ''}`} onClick={() => setFilter(id)}>{label}</button>
          ))}
        </div>
        {!query && recent.length > 0 && (
          <div className="recent-searches"><span>Recent</span>{recent.map((value) => (
            <button key={value} onClick={() => setQuery(value)}>↺ {value}</button>
          ))}</div>
        )}

        {deepBusy && (
          <div className="search-busy">
            <div className="loading-bar">
              <div className="loading-bar-fill" />
            </div>
            Searching the entire catalog — titles, people, themes…
          </div>
        )}

        {/* Deep sections take over the whole overlay below the query line. */}
        {deepView && !deepBusy && (
          <div className="search-sections">
            {visibleDeepRows?.length === 0 && (
              <div className="search-hint">
                Nothing found for “{query.trim()}”. Try an actor's name, a genre, or an idea
                like “korean drama” or “time travel”.
              </div>
            )}
            {visibleDeepRows?.map((row, r) => (
              <section key={`${r}:${row.title}`} className="search-section">
                <h2 className="search-section-head">
                  {row.title}
                  <span className="search-section-count">{row.items.length}</span>
                </h2>
                <div className="search-section-strip">
                  {row.items.map((item) => renderCard(item))}
                </div>
              </section>
            ))}
            <div className="search-hint">Press B / Esc to refine the search.</div>
          </div>
        )}

        {!deepView && !deepBusy && (
          <>
            <div className="search-top">
              <div className="osk">
                {KB_ROWS.map((row, r) => (
                  <div key={row} className="osk-row">
                    {row.split('').map((ch, c) => (
                      <button key={ch} className="osk-key" onClick={() => pressKey(r, c)}>
                        {ch}
                      </button>
                    ))}
                  </div>
                ))}
                <div className="osk-row">
                  {SPECIALS.map((label, c) => (
                    <button
                      key={label}
                      className={`osk-key osk-key-wide ${c === SEARCH_ALL_KEY ? 'osk-key-accent' : ''}`}
                      onClick={() => pressKey(SPECIAL_ROW, c)}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              </div>

              {query.trim().length < 2 ? (
                <div className="search-hint">
                  Type for instant title matches from your library, movies &amp; shows, and
                  add-on catalogs.
                  <br />
                  <br />
                  Press <span className="key">Enter</span> to search the <em>entire</em> space —
                  actors' filmographies, plot ideas (“time travel”), genres and vibes (“k drama”,
                  “romcom”).
                </div>
              ) : (
                visibleResults.length > 0 && (
                  <div className="search-hint">
                    {visibleResults.length} title match{visibleResults.length === 1 ? '' : 'es'} ·{' '}
                    <span className="key">Enter</span> searches everything — actors, themes,
                    genres
                  </div>
                )
              )}
            </div>

            <div className="search-grid">
              {visibleResults.map((item) => renderCard(item))}
              {query.trim().length >= 2 && visibleResults.length === 0 && (
                <div className="details-hint">No title matches — press Enter to search deeper.</div>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

// ---- Reusable on-screen keyboard for editing a single text field ----

// Letters + digits, with a symbols row so URLs and API keys are typable on a
// controller. Uppercase is a shift toggle applied to the letter rows.
const OSK_LETTERS = ['abcdefghi', 'jklmnopqr', 'stuvwxyz'];
const OSK_DIGITS = '0123456789';
const OSK_SYMBOLS = '.:/-_@+=?&%#';
const OSK_ACTIONS = ['Shift', 'Space', 'Paste', '⌫ Delete', 'Clear', 'Done'] as const;

/** Full-screen on-screen keyboard for entering/editing one field's value with
 *  a controller or remote. A physical keyboard still types straight into the
 *  value. The parent forwards controller NavActions by storing the handler
 *  written to `actionRef`. Commit happens on Done; Back/Esc cancels. */
export function OnScreenKeyboard({
  label,
  initialValue,
  masked = false,
  onCommit,
  onCancel,
  actionRef,
}: {
  label: string;
  initialValue: string;
  masked?: boolean;
  onCommit: (value: string) => void;
  onCancel: () => void;
  actionRef: MutableRefObject<((a: NavAction) => void) | null>;
}) {
  const [value, setValue] = useState(initialValue);
  const [shift, setShift] = useState(false);
  const [pos, setPos] = useState({ row: 0, col: 0 });

  // The key grid: three letter rows, a digits row, a symbols row, then actions.
  const rows = useMemo(() => {
    const letters = OSK_LETTERS.map((r) =>
      (shift ? r.toUpperCase() : r).split(''),
    );
    return [...letters, OSK_DIGITS.split(''), OSK_SYMBOLS.split(''), [...OSK_ACTIONS]];
  }, [shift]);
  const actionRow = rows.length - 1;

  const press = useCallback(
    (row: number, col: number) => {
      if (row === actionRow) {
        const kind = OSK_ACTIONS[col];
        if (kind === 'Shift') setShift((s) => !s);
        else if (kind === 'Space') setValue((v) => v + ' ');
        else if (kind === 'Paste')
          navigator.clipboard
            ?.readText()
            .then((clip) => clip && setValue((v) => v + clip))
            .catch(() => {});
        else if (kind === '⌫ Delete') setValue((v) => v.slice(0, -1));
        else if (kind === 'Clear') setValue('');
        else if (kind === 'Done') onCommit(value);
        return;
      }
      const ch = rows[row]?.[col];
      if (typeof ch === 'string') setValue((v) => v + ch);
    },
    [actionRow, rows, value, onCommit],
  );

  const handle = useCallback(
    (action: NavAction) => {
      if (action === 'back') {
        onCancel();
        return;
      }
      if (action === 'confirm') {
        press(pos.row, pos.col);
        return;
      }
      setPos((p) => {
        const rowLen = rows[p.row].length;
        switch (action) {
          case 'left':
            return { ...p, col: Math.max(0, p.col - 1) };
          case 'right':
            return { ...p, col: Math.min(rowLen - 1, p.col + 1) };
          case 'up': {
            if (p.row === 0) return p;
            const r = p.row - 1;
            return { row: r, col: mapCol(p.col, rowLen, rows[r].length) };
          }
          case 'down': {
            if (p.row === rows.length - 1) return p;
            const r = p.row + 1;
            return { row: r, col: mapCol(p.col, rowLen, rows[r].length) };
          }
          default:
            return p;
        }
      });
    },
    [pos, press, rows, onCancel],
  );

  // Controller path: the parent forwards d-pad / A / B here.
  useEffect(() => {
    actionRef.current = handle;
    return () => {
      actionRef.current = null;
    };
  }, [actionRef, handle]);

  // Physical keyboard path — typing still works directly for keyboard users.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        e.preventDefault();
        onCancel();
        return;
      }
      if (e.key === 'Enter') {
        e.stopPropagation();
        e.preventDefault();
        onCommit(value);
        return;
      }
      if (e.key === 'Backspace') {
        e.stopPropagation();
        e.preventDefault();
        setValue((v) => v.slice(0, -1));
        return;
      }
      // Paste (Ctrl/Cmd+V): pull from the clipboard and append. The native
      // `paste` event below also covers middle-click / context-menu paste.
      if ((e.ctrlKey || e.metaKey) && (e.key === 'v' || e.key === 'V')) {
        e.stopPropagation();
        e.preventDefault();
        navigator.clipboard
          ?.readText()
          .then((clip) => clip && setValue((v) => v + clip))
          .catch(() => {});
        return;
      }
      if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
        e.stopPropagation();
        e.preventDefault();
        setValue((v) => v + e.key);
      }
    };
    const onPaste = (e: ClipboardEvent) => {
      const clip = e.clipboardData?.getData('text');
      if (clip) {
        e.stopPropagation();
        e.preventDefault();
        setValue((v) => v + clip);
      }
    };
    window.addEventListener('keydown', onKey, true);
    window.addEventListener('paste', onPaste, true);
    return () => {
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('paste', onPaste, true);
    };
  }, [value, onCommit, onCancel]);

  const shown = masked ? '•'.repeat(value.length) : value;

  return (
    <div className="osk-scrim" onClick={onCancel}>
      <div className="osk-modal" onClick={(e) => e.stopPropagation()}>
        <div className="osk-modal-label">{label}</div>
        <div className="osk-modal-value">
          {shown}
          <span className="osk-caret" />
        </div>
        <div className="osk">
          {rows.map((row, r) => (
            <div key={r} className={`osk-row ${r === actionRow ? 'osk-row-actions' : ''}`}>
              {row.map((ch, c) => (
                <button
                  key={`${r}:${c}`}
                  className={`osk-key ${r === actionRow ? 'osk-key-wide' : ''} ${
                    pos.row === r && pos.col === c ? 'focused' : ''
                  } ${r === actionRow && ch === 'Shift' && shift ? 'osk-key-accent' : ''}`}
                  tabIndex={-1}
                  onClick={() => press(r, c)}
                >
                  {ch}
                </button>
              ))}
            </div>
          ))}
        </div>
        <div className="search-hint">
          <span className="key">A</span> Type · <span className="key">B</span>/Esc Cancel · Done to save
        </div>
      </div>
    </div>
  );
}
