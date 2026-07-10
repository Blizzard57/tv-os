import { MutableRefObject, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ContentItem, Row, searchCatalog, searchDeep } from './api';
import { NavAction } from './input';

const COLS = 6;

// On-screen keyboard layout: four 9-key rows, then a special row. A physical
// keyboard still types straight into the query; this exists so a controller
// or TV remote can search without one.
const KB_ROWS = ['abcdefghi', 'jklmnopqr', 'stuvwxyz0', '123456789'];
const SPECIAL_ROW = 4;
const SPECIALS = ['Space', '⌫ Delete', 'Clear', '🔍 Search all'] as const;
const SEARCH_ALL_KEY = 3;

/** Map a column between two rows of differing length by relative position, so
 *  moving up/down between rows lands under (roughly) the same spot instead of
 *  jumping via magic multipliers. */
export function mapCol(col: number, fromLen: number, toLen: number): number {
  if (fromLen <= 1) return 0;
  return Math.round((col / (fromLen - 1)) * (toLen - 1));
}

// Where focus lives: the query line, the on-screen keys, the quick-result
// grid, or the deep-search sections.
type Zone = 'input' | 'kb' | 'results' | 'deep';

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
  selected,
  refCb,
  onPick,
}: {
  item: ContentItem;
  selected: boolean;
  refCb: (el: HTMLDivElement | null) => void;
  onPick: (item: ContentItem) => void;
}) {
  const [artFailed, setArtFailed] = useState(false);
  const badge = badgeFor(item);
  // Google TV shows results as wide 16:9 cards (art_of now feeds landscape
  // backdrops), so every search card is landscape.
  return (
    <div
      ref={refCb}
      className={`search-card ${selected ? 'selected' : ''}`}
      onClick={() => onPick(item)}
    >
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
  const [sel, setSel] = useState(0);
  const [zone, setZone] = useState<Zone>('kb');
  const [kb, setKb] = useState({ row: 0, col: 0 });
  // Deep search: sections replace the keyboard + quick grid until dismissed.
  const [deepRows, setDeepRows] = useState<Row[] | null>(null);
  const [deepBusy, setDeepBusy] = useState(false);
  const [dSel, setDSel] = useState({ row: 0, col: 0 });
  const inputRef = useRef<HTMLInputElement>(null);
  const cardRefs = useRef<(HTMLDivElement | null)[]>([]);
  const deepRefs = useRef<Map<string, HTMLDivElement>>(new Map());
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
          setSel(0);
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
  }, [query]);

  // If the results under our feet disappear, step back to the keyboard.
  useEffect(() => {
    if (zone === 'results' && results.length === 0) setZone('kb');
    setSel((i) => Math.min(i, Math.max(0, results.length - 1)));
  }, [results, zone]);

  const runDeep = useCallback(() => {
    const q = query.trim();
    if (q.length < 2 || deepBusy) return;
    inputRef.current?.blur();
    setDeepBusy(true);
    const token = ++deepToken.current;
    searchDeep(q)
      .then((rows) => {
        if (token !== deepToken.current) return; // query changed / newer deep search
        setDeepRows(rows);
        setDSel({ row: 0, col: 0 });
        setZone(rows.length > 0 ? 'deep' : 'kb');
      })
      .catch(() => {
        if (token !== deepToken.current) return;
        setDeepRows([]);
        setZone('kb');
      })
      .finally(() => {
        if (token === deepToken.current) setDeepBusy(false);
      });
  }, [query, deepBusy]);

  const exitDeep = useCallback(() => {
    setDeepRows(null);
    setZone('kb');
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

  // One handler for every input device. The window key listener and the
  // gamepad (via actionRef) both funnel into this.
  const handle = useCallback(
    (action: NavAction) => {
      // Back peels one layer: deep sections → quick view → closed.
      if (action === 'back') {
        if (deepRows) exitDeep();
        else onClose();
        return;
      }

      if (zone === 'deep' && deepRows) {
        const row = deepRows[dSel.row];
        switch (action) {
          case 'left':
            setDSel((s) => ({ ...s, col: Math.max(0, s.col - 1) }));
            break;
          case 'right':
            setDSel((s) => ({
              ...s,
              col: Math.min((row?.items.length ?? 1) - 1, s.col + 1),
            }));
            break;
          case 'up':
            if (dSel.row === 0) {
              setZone('input');
              inputRef.current?.focus();
            } else {
              setDSel((s) => {
                const r = s.row - 1;
                return { row: r, col: Math.min(s.col, deepRows[r].items.length - 1) };
              });
            }
            break;
          case 'down':
            setDSel((s) => {
              const r = Math.min(deepRows.length - 1, s.row + 1);
              return { row: r, col: Math.min(s.col, deepRows[r].items.length - 1) };
            });
            break;
          case 'confirm': {
            const item = row?.items[dSel.col];
            if (item) onPick(item);
            break;
          }
          default:
            break;
        }
        return;
      }

      if (zone === 'input') {
        if (action === 'down') {
          inputRef.current?.blur();
          setZone(deepRows && deepRows.length > 0 ? 'deep' : 'kb');
        } else if (action === 'confirm') {
          // Enter on the query line = search the entire space.
          runDeep();
        }
        return;
      }

      if (zone === 'kb') {
        const rowLen = kb.row === SPECIAL_ROW ? SPECIALS.length : KB_ROWS[kb.row].length;
        switch (action) {
          case 'left':
            setKb((k) => ({ ...k, col: Math.max(0, k.col - 1) }));
            break;
          case 'right':
            setKb((k) => ({ ...k, col: Math.min(rowLen - 1, k.col + 1) }));
            break;
          case 'up':
            if (kb.row === 0) {
              setZone('input');
              inputRef.current?.focus();
            } else if (kb.row === SPECIAL_ROW) {
              setKb((k) => ({ row: SPECIAL_ROW - 1, col: mapCol(k.col, SPECIALS.length, KB_ROWS[SPECIAL_ROW - 1].length) }));
            } else {
              setKb((k) => ({ ...k, row: k.row - 1 }));
            }
            break;
          case 'down':
            if (kb.row === SPECIAL_ROW) {
              if (results.length > 0) setZone('results');
            } else if (kb.row === SPECIAL_ROW - 1) {
              setKb((k) => ({
                row: SPECIAL_ROW,
                col: mapCol(k.col, KB_ROWS[SPECIAL_ROW - 1].length, SPECIALS.length),
              }));
            } else {
              setKb((k) => ({ ...k, row: k.row + 1 }));
            }
            break;
          case 'confirm':
            pressKey(kb.row, kb.col);
            break;
          default:
            break;
        }
        return;
      }

      // zone === 'results' (quick grid)
      switch (action) {
        case 'left':
          setSel((i) => Math.max(0, i - 1));
          break;
        case 'right':
          setSel((i) => Math.min(results.length - 1, i + 1));
          break;
        case 'down':
          setSel((i) => Math.min(results.length - 1, i + COLS));
          break;
        case 'up':
          if (sel < COLS) setZone('kb');
          else setSel((i) => i - COLS);
          break;
        case 'confirm': {
          const item = results[sel];
          if (item) onPick(item);
          break;
        }
        default:
          break;
      }
    },
    [zone, kb, results, sel, deepRows, dSel, onPick, onClose, pressKey, runDeep, exitDeep],
  );

  // Gamepad path: App forwards d-pad / A / B here while the overlay is open.
  useEffect(() => {
    actionRef.current = handle;
    return () => {
      actionRef.current = null;
    };
  }, [actionRef, handle]);

  // Physical keyboard path (capture phase so the home grid never moves).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        handle('back');
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
        handle(action);
        return;
      }
      // Typing while focus is on the keys/results still edits the query, so a
      // keyboard user never has to navigate back up to the input line.
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
  }, [handle]);

  useEffect(() => {
    if (zone === 'results') cardRefs.current[sel]?.scrollIntoView({ block: 'nearest' });
  }, [sel, zone]);

  useEffect(() => {
    if (zone === 'deep') {
      deepRefs.current
        .get(`${dSel.row}:${dSel.col}`)
        ?.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'smooth' });
    }
  }, [dSel, zone]);

  const keyFocused = (row: number, col: number) =>
    zone === 'kb' && kb.row === row && kb.col === col;

  const renderCard = (
    item: ContentItem,
    selected: boolean,
    refCb: (el: HTMLDivElement | null) => void,
  ) => (
    <ResultCard key={item.id} item={item} selected={selected} refCb={refCb} onPick={onPick} />
  );

  const deepView = deepRows !== null;

  return (
    <div className="search-scrim" onClick={onClose}>
      <div className="search" onClick={(e) => e.stopPropagation()}>
        <input
          ref={inputRef}
          className="search-input"
          value={query}
          placeholder="Search anything — a title, an actor, “k drama”, “time travel”…"
          onChange={(e) => setQuery(e.target.value)}
          onFocus={() => setZone('input')}
        />

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
            {deepRows.length === 0 && (
              <div className="search-hint">
                Nothing found for “{query.trim()}”. Try an actor's name, a genre, or an idea
                like “korean drama” or “time travel”.
              </div>
            )}
            {deepRows.map((row, r) => (
              <section
                key={`${r}:${row.title}`}
                className={`search-section ${
                  zone === 'deep' && r === dSel.row ? 'section-active' : ''
                }`}
              >
                <h2 className="search-section-head">
                  {row.title}
                  <span className="search-section-count">{row.items.length}</span>
                </h2>
                <div className="search-section-strip">
                  {row.items.map((item, c) =>
                    renderCard(item, zone === 'deep' && r === dSel.row && c === dSel.col, (el) => {
                      if (el) deepRefs.current.set(`${r}:${c}`, el);
                      else deepRefs.current.delete(`${r}:${c}`);
                    }),
                  )}
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
                      <button
                        key={ch}
                        className={`osk-key ${keyFocused(r, c) ? 'focused' : ''}`}
                        tabIndex={-1}
                        onClick={() => pressKey(r, c)}
                      >
                        {ch}
                      </button>
                    ))}
                  </div>
                ))}
                <div className="osk-row">
                  {SPECIALS.map((label, c) => (
                    <button
                      key={label}
                      className={`osk-key osk-key-wide ${
                        c === SEARCH_ALL_KEY ? 'osk-key-accent' : ''
                      } ${keyFocused(SPECIAL_ROW, c) ? 'focused' : ''}`}
                      tabIndex={-1}
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
                results.length > 0 && (
                  <div className="search-hint">
                    {results.length} title match{results.length === 1 ? '' : 'es'} ·{' '}
                    <span className="key">Enter</span> searches everything — actors, themes,
                    genres
                  </div>
                )
              )}
            </div>

            <div className="search-grid">
              {results.map((item, i) =>
                renderCard(item, zone === 'results' && i === sel, (el) => {
                  cardRefs.current[i] = el;
                }),
              )}
              {query.trim().length >= 2 && results.length === 0 && (
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
