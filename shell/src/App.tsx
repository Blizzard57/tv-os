import { useCallback, useEffect, useRef, useState } from 'react';
import {
  ContentItem,
  EnhanceMode,
  InstallJob,
  Meta,
  Row,
  Settings,
  fetchInstalls,
  fetchLibrary,
  fetchMeta,
  fetchSettings,
  saveSettings,
} from './api';
import { DesktopHome } from './DesktopHome';
import { DetailsPage } from './DetailsPage';
import { NavAction, useTvInput } from './input';
import { MediaRow, colsFor, navLength, shownCount } from './MediaRow';
import { SearchOverlay } from './SearchOverlay';
import { SectionPage } from './SectionPage';
import { SettingsPanel } from './SettingsPanel';
import {
  Theme,
  UiMode,
  applyAccent,
  applyMode,
  applyTheme,
  initialMode,
  initialTheme,
  otherMode,
  otherTheme,
} from './theme';

// Focus is simply "which row, which column". Each row remembers its own
// column so moving up/down returns you to where you were in that row.
// Row -1 is the left sidebar: press ◀ at the first column to reach it; its
// entries are part of the same d-pad grid (col = entry index, top to bottom).
interface Focus {
  row: number;
  col: number;
}

/** Sidebar entries, top to bottom. Collapsed it shows icons; focusing (or
 *  hovering) it expands to icons + labels. */
const SIDEBAR_ITEMS = [
  { id: 'search', icon: '⌕', label: 'Search' },
  { id: 'settings', icon: '⚙', label: 'Settings' },
  { id: 'theme', icon: '◐', label: 'Theme' },
  { id: 'enhance', icon: '✦', label: 'Enhance' },
  { id: 'desktop', icon: '🖥', label: 'Desktop layout' },
] as const;

const ENHANCE_CYCLE: EnhanceMode[] = ['auto', 'quality', 'performance', 'off'];
const ENHANCE_LABELS: Record<EnhanceMode, string> = {
  auto: 'Auto',
  quality: 'Quality',
  performance: 'Performance',
  off: 'Off',
};

const BLANK_SETTINGS: Settings = {
  enhance: 'auto',
  steam_api_key: '',
  steam_id: '',
  tmdb_key: '',
  accent: '',
  youtube_channels: '',
  youtube_account: false,
  game_region: '',
  trakt_client_id: '',
  trakt_client_secret: '',
  trakt_token: '',
  anilist_token: '',
  mal_client_id: '',
  mal_token: '',
};

export default function App() {
  const [rows, setRows] = useState<Row[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [focus, setFocus] = useState<Focus>({ row: 0, col: 0 });
  const [toast, setToast] = useState<string | null>(null);
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [mode, setMode] = useState<UiMode>(initialMode);
  const [settings, setSettings] = useState<Settings>(BLANK_SETTINGS);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  // "Show all" page for one home section (expand tile).
  const [expandedRow, setExpandedRow] = useState<Row | null>(null);
  // Details pages form a stack so "More like this" hops can be walked back
  // with B, one page at a time, all the way home.
  const [detailsStack, setDetailsStack] = useState<ContentItem[]>([]);
  const detailsItem = detailsStack.length > 0 ? detailsStack[detailsStack.length - 1] : null;
  const pushDetails = useCallback(
    (item: ContentItem) => setDetailsStack((s) => [...s, item]),
    [],
  );
  const popDetails = useCallback(() => setDetailsStack((s) => s.slice(0, -1)), []);
  const { jobs, refresh: refreshJobs } = useInstallJobs(() => loadLibrary());
  // Where to land when leaving the sidebar (the grid spot you came from).
  const gridReturn = useRef<Focus>({ row: 0, col: 0 });
  const toastTimer = useRef<number>();
  // Overlays register their nav handlers here; App forwards input while open.
  const detailsActionRef = useRef<((a: NavAction) => void) | null>(null);
  const settingsActionRef = useRef<((a: NavAction) => void) | null>(null);
  const searchActionRef = useRef<((a: NavAction) => void) | null>(null);
  const sectionActionRef = useRef<((a: NavAction) => void) | null>(null);
  // Mirror of `focus` for event handlers: 'confirm' must read the current
  // position without doing side effects inside a setState updater (React
  // double-invokes updaters in dev to assert purity).
  const focusRef = useRef(focus);
  focusRef.current = focus;

  const loadLibrary = useCallback(() => {
    fetchLibrary()
      .then(setRows)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(loadLibrary, [loadLibrary]);
  useEffect(() => applyTheme(theme), [theme]);
  useEffect(() => applyMode(mode), [mode]);
  useEffect(() => applyAccent(settings.accent), [settings.accent]);
  useEffect(() => {
    fetchSettings()
      .then((s) => {
        setSettings({ ...BLANK_SETTINGS, ...s });
        setSettingsLoaded(true);
      })
      .catch(() => {}); // daemon default is shown until it answers
  }, []);

  // Refetch settings + library after the panel changes something, so the
  // home screen and the Enhance chip reflect it immediately.
  const reloadAll = useCallback(() => {
    fetchSettings()
      .then((s) => setSettings({ ...BLANK_SETTINGS, ...s }))
      .catch(() => {});
    loadLibrary();
  }, [loadLibrary]);

  // Library refreshes can shrink or reorder rows (e.g. an installed game
  // moves from "Ready to Install" into "Games") — keep focus in bounds.
  useEffect(() => {
    if (!rows || rows.length === 0) return;
    setFocus((f) => {
      if (f.row < 0) return f; // sidebar focus is independent of the rows
      const row = Math.min(f.row, rows.length - 1);
      const col = Math.min(f.col, Math.max(0, navLength(rows[row]) - 1));
      return row === f.row && col === f.col ? f : { row, col };
    });
  }, [rows]);

  const showToast = useCallback((message: string) => {
    setToast(message);
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 3000);
  }, []);

  // Refreshes background data after a play/install (recommender rows, jobs).
  const onPlayed = useCallback(() => {
    reloadAll();
    refreshJobs();
  }, [reloadAll, refreshJobs]);

  const toggleMode = useCallback(() => {
    setMode((m) => {
      const next = otherMode(m);
      showToast(next === 'desktop' ? 'Desktop layout' : 'TV layout');
      return next;
    });
  }, [showToast]);

  const cycleEnhance = useCallback(() => {
    // Never save before the real settings arrive — a blank form would
    // overwrite the saved Steam/TMDB keys on disk.
    if (!settingsLoaded) {
      showToast('Settings still loading…');
      return;
    }
    const next =
      ENHANCE_CYCLE[(ENHANCE_CYCLE.indexOf(settings.enhance) + 1) % ENHANCE_CYCLE.length];
    const updated = { ...settings, enhance: next };
    setSettings(updated);
    showToast(`Enhance: ${ENHANCE_LABELS[next]}`);
    saveSettings(updated).catch((e) => showToast(`Could not save: ${e.message}`));
  }, [settings, settingsLoaded, showToast]);

  const onAction = useCallback(
    (action: NavAction) => {
      // The search overlay owns the keyboard itself; the search key closes it,
      // and everything else (d-pad, A, B) is forwarded — B first steps out of
      // deep results, and the overlay closes itself when there's nothing to
      // step out of.
      if (searchOpen) {
        if (action === 'search') setSearchOpen(false);
        else searchActionRef.current?.(action);
        return;
      }
      // While the Settings panel is open it owns input; B/Start close it, the
      // rest is forwarded so a controller can walk the panel's controls.
      if (settingsOpen) {
        if (action === 'back' || action === 'settings') setSettingsOpen(false);
        else settingsActionRef.current?.(action);
        return;
      }
      // While the details page is open it owns input (it closes itself on back).
      if (detailsItem) {
        detailsActionRef.current?.(action);
        return;
      }
      // "Show all" section page — under details, over the home grid.
      if (expandedRow) {
        sectionActionRef.current?.(action);
        return;
      }
      if (action === 'search') {
        setSearchOpen(true);
        return;
      }
      if (action === 'settings') {
        setSettingsOpen(true);
        return;
      }
      if (action === 'theme') {
        const next = otherTheme(theme);
        setTheme(next);
        showToast(next === 'light' ? 'Light mode' : 'Dark mode');
        return;
      }
      if (action === 'enhance') {
        cycleEnhance();
        return;
      }
      if (!rows || rows.length === 0) return;

      // Focus is in the left sidebar: ▲▼ walk the entries, A activates,
      // ▶ / B return to the grid exactly where you left it. This is what
      // makes Settings & co reachable by d-pad.
      if (focusRef.current.row < 0) {
        const col = focusRef.current.col;
        switch (action) {
          case 'up':
            setFocus({ row: -1, col: Math.max(0, col - 1) });
            break;
          case 'down':
            setFocus({ row: -1, col: Math.min(SIDEBAR_ITEMS.length - 1, col + 1) });
            break;
          case 'right':
          case 'back': {
            const back = gridReturn.current;
            const row = rows[Math.min(back.row, rows.length - 1)];
            setFocus({
              row: Math.min(back.row, rows.length - 1),
              col: Math.min(back.col, navLength(row) - 1),
            });
            break;
          }
          case 'confirm': {
            const entry = SIDEBAR_ITEMS[col].id;
            if (entry === 'search') setSearchOpen(true);
            else if (entry === 'settings') setSettingsOpen(true);
            else if (entry === 'theme') {
              const next = otherTheme(theme);
              setTheme(next);
              showToast(next === 'light' ? 'Light mode' : 'Dark mode');
            } else if (entry === 'desktop') toggleMode();
            else cycleEnhance();
            break;
          }
          default:
            break;
        }
        return;
      }

      if (action === 'confirm') {
        const f = focusRef.current;
        const row = rows[f.row];
        if (!row) return;
        // The "Show all" tile sits after the shown items.
        if (row.items.length > shownCount(row) && f.col === shownCount(row)) {
          setExpandedRow(row);
          return;
        }
        const item = row.items[f.col];
        if (item) pushDetails(item); // open the details page for every entry
        return;
      }
      if (action === 'back') return;
      // Sections are wrapped grids (up to 3 lines, `colsFor` per line, an
      // expand tile at the end) — the d-pad moves by cell and by line.
      setFocus((f) => {
        const row = rows[f.row];
        const cols = colsFor(row);
        const len = navLength(row);
        switch (action) {
          case 'left':
            // At the left edge of a line, slide into the sidebar.
            if (f.col % cols === 0) {
              gridReturn.current = f;
              return { row: -1, col: 0 };
            }
            return { ...f, col: f.col - 1 };
          case 'right':
            return { ...f, col: Math.min(len - 1, f.col + 1) };
          case 'up': {
            if (f.col - cols >= 0) return { ...f, col: f.col - cols };
            if (f.row === 0) return f;
            const prev = rows[f.row - 1];
            const pcols = colsFor(prev);
            const plen = navLength(prev);
            const lastLine = Math.floor((plen - 1) / pcols);
            return {
              row: f.row - 1,
              col: Math.min(lastLine * pcols + Math.min(f.col % cols, pcols - 1), plen - 1),
            };
          }
          case 'down': {
            if (f.col + cols < len) return { ...f, col: f.col + cols };
            const line = Math.floor(f.col / cols);
            const lastLine = Math.floor((len - 1) / cols);
            // A shorter final line still exists below — land on its end.
            if (line < lastLine) return { ...f, col: len - 1 };
            if (f.row + 1 >= rows.length) return f;
            const next = rows[f.row + 1];
            return {
              row: f.row + 1,
              col: Math.min(Math.min(f.col % cols, colsFor(next) - 1), navLength(next) - 1),
            };
          }
          default:
            return f;
        }
      });
    },
    [rows, showToast, theme, settings, settingsLoaded, settingsOpen, searchOpen, detailsItem, expandedRow, cycleEnhance, toggleMode, pushDetails],
  );
  useTvInput(onAction);

  // While the sidebar is focused, the hero keeps previewing the item you
  // left the grid on instead of blanking out; on the expand tile it shows
  // the section's last visible item.
  const heroRow = focus.row >= 0 ? focus.row : gridReturn.current.row;
  const heroCol = focus.row >= 0 ? focus.col : gridReturn.current.col;
  const heroItems = rows?.[heroRow]?.items;
  const focusedItem: ContentItem | undefined =
    heroItems && heroItems.length > 0
      ? heroItems[Math.min(heroCol, heroItems.length - 1)]
      : undefined;
  const sbFocus = focus.row < 0 ? focus.col : null;
  const preview = useHeroPreview(focusedItem);
  // Images for the preview panel: a game's screenshots, else the backdrop.
  const previewImages: string[] =
    preview?.screenshots && preview.screenshots.length > 0
      ? preview.screenshots.slice(0, 4)
      : preview?.background
        ? [preview.background]
        : [];
  const previewSub = [
    preview?.release_info,
    preview?.runtime,
    preview?.rating && (focusedItem?.kind === 'game' ? `Metacritic ${preview.rating}` : `★ ${preview.rating}`),
    preview?.genres?.slice(0, 3).join(', '),
  ]
    .filter(Boolean)
    .join('  ·  ');

  // The chrome (chips, Settings) is always present so the panel is reachable
  // even on a fresh, empty install. Only the central body changes.
  let body;
  if (error) {
    body = <div className="screen-message">{error}</div>;
  } else if (!rows) {
    body = <div className="screen-message">Loading…</div>;
  } else if (rows.length === 0) {
    body = (
      <div className="screen-message">
        Nothing here yet — open Settings (press <span className="key">S</span> /{' '}
        <span className="key">Start</span>) to connect Steam, add a TMDB key, or install an addon.
      </div>
    );
  } else {
    body = (
      <>
        <header className="hero">
          <div className="hero-text">
            <div className="hero-kind">{focusedItem?.kind.toUpperCase()}</div>
            <h1 className="hero-title">{focusedItem?.title}</h1>
            {previewSub && <div className="hero-sub">{previewSub}</div>}
            {preview?.description && <p className="hero-desc">{preview.description}</p>}
            <div className="hero-hint">
              Press <span className="key">A</span> / <span className="key">Enter</span> to open
            </div>
          </div>
          {previewImages.length > 0 && (
            <div className="hero-images">
              {previewImages.map((src) => (
                <img key={src} className="hero-shot" src={src} alt="" loading="lazy" />
              ))}
            </div>
          )}
        </header>

        <main className="rows">
          {rows.map((row, i) => (
            <MediaRow
              key={row.title}
              row={row}
              focusedCol={i === focus.row ? focus.col : null}
              jobs={jobs}
              onExpand={() => setExpandedRow(row)}
              onPick={pushDetails}
            />
          ))}
        </main>
      </>
    );
  }

  return (
    <div className="app">
      {mode === 'tv' && focusedItem?.art && (
        <div
          key={focusedItem.id}
          className="ambient"
          style={{ backgroundImage: `url(${focusedItem.art})` }}
        />
      )}

      {mode === 'tv' && (
        <>
          <nav className={`tv-sidebar ${sbFocus !== null ? 'open' : ''}`}>
            {SIDEBAR_ITEMS.map((entry, i) => (
              <button
                key={entry.id}
                className={`tv-sb-item ${sbFocus === i ? 'focused' : ''}`}
                onClick={() => {
                  if (entry.id === 'search') setSearchOpen(true);
                  else if (entry.id === 'settings') setSettingsOpen(true);
                  else if (entry.id === 'theme') setTheme((t) => otherTheme(t));
                  else if (entry.id === 'desktop') toggleMode();
                  else cycleEnhance();
                }}
              >
                <span className="tv-sb-icon">{entry.icon}</span>
                <span className="tv-sb-label">
                  {entry.id === 'enhance'
                    ? `Enhance · ${ENHANCE_LABELS[settings.enhance]}`
                    : entry.label}
                </span>
              </button>
            ))}
            <div className="tv-sb-hint">◀ from the first card</div>
          </nav>
          <div className="tv-clock">
            <Clock />
          </div>
        </>
      )}

      {mode === 'tv' ? (
        body
      ) : (
        <DesktopHome
          rows={rows}
          error={error}
          onOpen={pushDetails}
          onSearch={() => setSearchOpen(true)}
          onSettings={() => setSettingsOpen(true)}
          onToggleTheme={() => setTheme((t) => otherTheme(t))}
          onToggleMode={toggleMode}
        />
      )}

      <DownloadsPanel jobs={jobs} />
      {toast && <div className="toast">{toast}</div>}
      {expandedRow && (
        <SectionPage
          row={expandedRow}
          onPick={pushDetails}
          onClose={() => setExpandedRow(null)}
          actionRef={sectionActionRef}
        />
      )}
      {detailsItem && (
        <DetailsPage
          key={`${detailsStack.length}-${detailsItem.id}`}
          item={detailsItem}
          onClose={popDetails}
          onOpen={pushDetails}
          onPlayed={onPlayed}
          actionRef={detailsActionRef}
        />
      )}
      {settingsOpen && (
        <SettingsPanel
          onClose={() => setSettingsOpen(false)}
          reload={reloadAll}
          theme={theme}
          onToggleTheme={() => setTheme((t) => otherTheme(t))}
          mode={mode}
          onToggleMode={toggleMode}
          actionRef={settingsActionRef}
        />
      )}
      {searchOpen && (
        <SearchOverlay
          onClose={() => setSearchOpen(false)}
          onPick={(item) => {
            setSearchOpen(false);
            pushDetails(item);
          }}
          actionRef={searchActionRef}
        />
      )}
    </div>
  );
}

/// Polls download jobs while any are running; calls onFinished when a job
/// completes so the library can refresh (the game moves to "Games").
/// `refresh` polls immediately — used right after starting an install.
function useInstallJobs(onFinished: () => void) {
  const [jobs, setJobs] = useState<InstallJob[]>([]);
  const onFinishedRef = useRef(onFinished);
  onFinishedRef.current = onFinished;
  const runningIds = useRef<Set<string>>(new Set());
  const running = jobs.some((j) => j.status === 'running');

  const refresh = useCallback(() => {
    fetchInstalls()
      .then((next) => {
        if (next.some((j) => j.status === 'done' && runningIds.current.has(j.id))) {
          onFinishedRef.current();
        }
        runningIds.current = new Set(
          next.filter((j) => j.status === 'running').map((j) => j.id),
        );
        setJobs(next);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    // Poll fast while a download runs, lazily otherwise.
    const interval = window.setInterval(refresh, running ? 2000 : 15000);
    return () => window.clearInterval(interval);
  }, [running, refresh]);

  return { jobs, refresh };
}

/// Loads rich metadata (summary + images) for the focused item, debounced so
/// scrolling doesn't spam the network, and cached so revisiting is instant.
function useHeroPreview(item: ContentItem | undefined): Meta | null {
  const [meta, setMeta] = useState<Meta | null>(null);
  const cache = useRef<Map<string, Meta>>(new Map());
  const id = item?.id;
  useEffect(() => {
    if (!id) {
      setMeta(null);
      return;
    }
    const cached = cache.current.get(id);
    if (cached) {
      setMeta(cached);
      return;
    }
    setMeta(null);
    let cancelled = false;
    const t = window.setTimeout(() => {
      fetchMeta(id)
        .then((m) => {
          if (cancelled) return;
          cache.current.set(id, m);
          setMeta(m);
        })
        .catch(() => {});
    }, 350);
    return () => {
      cancelled = true;
      window.clearTimeout(t);
    };
  }, [id]);
  return meta;
}

/// The living-room clock — a TV home screen should always show the time.
function Clock() {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const t = window.setInterval(() => setNow(new Date()), 15_000);
    return () => window.clearInterval(t);
  }, []);
  return (
    <div className="status-chip status-clock">
      {now.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
    </div>
  );
}

function DownloadsPanel({ jobs }: { jobs: InstallJob[] }) {
  const visible = jobs.filter((j) => j.status !== 'done');
  if (visible.length === 0) return null;
  return (
    <aside className="downloads">
      <div className="downloads-title">Downloads</div>
      {visible.map((job) => (
        <div key={job.id} className="download">
          <div className="download-row">
            <span className="download-name">{job.title}</span>
            <span className="download-pct">
              {job.status === 'failed' ? 'Failed' : `${Math.floor(job.progress)}%`}
            </span>
          </div>
          <div className="download-bar">
            <div
              className={`download-fill ${job.status === 'failed' ? 'download-fill-failed' : ''}`}
              style={{ width: `${Math.max(2, job.progress)}%` }}
            />
          </div>
        </div>
      ))}
    </aside>
  );
}
