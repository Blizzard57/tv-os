import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  ApiError,
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
import { DetailsPage } from './DetailsPage';
import { Hero } from './Hero';
import { NavAction, useTvInput } from './input';
import { SearchOverlay } from './SearchOverlay';
import { SettingsPanel } from './SettingsPanel';
import { Shelf } from './Shelf';
import {
  FIRST_TAB_INDEX,
  SEARCH_INDEX,
  SETTINGS_INDEX,
  THEME_INDEX,
  TOPBAR_COUNT,
  TopBar,
  tabIndex,
} from './TopBar';
import { TABS, TabId, rowsForTab, tabHasContent } from './tabs';
import { Theme, applyAccent, applyTheme, initialTheme, otherTheme } from './theme';

// Focus lives in one of two zones. In `topbar`, `col` is an index into
// [search, ...tabs, settings, theme]. In `rows`, `row` is the shelf and `col`
// the card. Each remembers its own spot so leaving and returning is seamless.
interface Focus {
  zone: 'topbar' | 'rows';
  row: number;
  col: number;
}

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
  display_resolution: '',
  display_hdr: false,
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

// Couch-friendly error classification — never surfaces a raw exception string
// to the living-room screen. `offline` gets its own dedicated message.
type LoadError = 'offline' | 'timeout' | 'generic';

function errorKind(e: unknown): LoadError {
  if (typeof navigator !== 'undefined' && navigator.onLine === false) return 'offline';
  if (e instanceof ApiError) {
    if (e.code === 'offline') return 'offline';
    if (e.code === 'timeout') return 'timeout';
  }
  return 'generic';
}

const ERROR_COPY: Record<LoadError, string> = {
  offline: "You're offline. Check the network connection and try again.",
  timeout: "Couldn't reach TV OS in time. It may still be starting up.",
  generic: 'Something went wrong loading your library.',
};

const EMPTY_TAB_COPY: Record<TabId, string> = {
  foryou: '',
  live: 'Nothing live right now — follow a channel or connect YouTube in Settings.',
  movies: 'No movies yet — add a TMDB key in Settings to fill this tab.',
  shows: 'No shows yet — add a TMDB key in Settings to fill this tab.',
  library: 'Your library is empty — connect Steam, Epic or GOG, or start watching to fill it.',
};

export default function App() {
  const [rows, setRows] = useState<Row[] | null>(null);
  const [error, setError] = useState<LoadError | null>(null);
  const [activeTab, setActiveTab] = useState<TabId>('foryou');
  const [focus, setFocus] = useState<Focus>({ zone: 'rows', row: 0, col: 0 });
  const [toast, setToast] = useState<string | null>(null);
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [settings, setSettings] = useState<Settings>(BLANK_SETTINGS);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  // Details pages form a stack so "More like this" hops can be walked back
  // with B, one page at a time, all the way home.
  const [detailsStack, setDetailsStack] = useState<ContentItem[]>([]);
  const detailsItem = detailsStack.length > 0 ? detailsStack[detailsStack.length - 1] : null;
  const pushDetails = useCallback((item: ContentItem) => setDetailsStack((s) => [...s, item]), []);
  const popDetails = useCallback(() => setDetailsStack((s) => s.slice(0, -1)), []);
  const { jobs, refresh: refreshJobs } = useInstallJobs(() => loadLibrary());

  // The shelves visible under the active tab: the tab's kind-filtered rows.
  const shelves = useMemo<Row[]>(() => rowsForTab(activeTab, rows ?? []), [activeTab, rows]);

  // Which tabs currently have anything to show (dims empty tabs in the bar).
  const enabledTabs = useMemo(() => {
    const set = new Set<TabId>();
    for (const t of TABS) if (tabHasContent(t.id, rows ?? [])) set.add(t.id);
    return set;
  }, [rows]);

  // Where to land in the rows when dropping down from the top bar.
  const gridReturn = useRef<Focus>({ zone: 'rows', row: 0, col: 0 });
  const toastTimer = useRef<number>();
  // Overlays register their nav handlers here; App forwards input while open.
  const detailsActionRef = useRef<((a: NavAction) => void) | null>(null);
  const settingsActionRef = useRef<((a: NavAction) => void) | null>(null);
  const searchActionRef = useRef<((a: NavAction) => void) | null>(null);
  // Mirrors read inside event handlers (avoid stale closures / setState purity).
  const focusRef = useRef(focus);
  focusRef.current = focus;
  const shelvesRef = useRef(shelves);
  shelvesRef.current = shelves;
  const activeTabRef = useRef(activeTab);
  activeTabRef.current = activeTab;

  const loadLibrary = useCallback(() => {
    setError(null);
    fetchLibrary()
      .then((r) => {
        setRows(r);
        setError(null);
      })
      .catch((e) => setError(errorKind(e)));
  }, []);

  useEffect(loadLibrary, [loadLibrary]);
  useEffect(() => applyTheme(theme), [theme]);
  useEffect(() => applyAccent(settings.accent), [settings.accent]);
  useEffect(() => {
    fetchSettings()
      .then((s) => {
        setSettings({ ...BLANK_SETTINGS, ...s });
        setSettingsLoaded(true);
      })
      .catch(() => {});
  }, []);

  // Refetch settings + library after the panel changes something.
  const reloadAll = useCallback(() => {
    fetchSettings()
      .then((s) => setSettings({ ...BLANK_SETTINGS, ...s }))
      .catch(() => {});
    loadLibrary();
  }, [loadLibrary]);

  // Keep focus in bounds when the visible shelves change (library refresh or
  // tab switch can shrink/replace them).
  useEffect(() => {
    setFocus((f) => {
      if (f.zone !== 'rows') return f;
      if (shelves.length === 0) return { zone: 'rows', row: 0, col: 0 };
      const row = Math.min(f.row, shelves.length - 1);
      const len = shelves[row].items.length;
      const col = Math.min(f.col, Math.max(0, len - 1));
      return row === f.row && col === f.col ? f : { zone: 'rows', row, col };
    });
  }, [shelves]);

  const showToast = useCallback((message: string) => {
    setToast(message);
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 3000);
  }, []);
  useEffect(() => () => window.clearTimeout(toastTimer.current), []);

  const onPlayed = useCallback(() => {
    reloadAll();
    refreshJobs();
  }, [reloadAll, refreshJobs]);

  const toggleTheme = useCallback(() => {
    setTheme((t) => {
      const next = otherTheme(t);
      showToast(next === 'light' ? 'Light mode' : 'Dark mode');
      return next;
    });
  }, [showToast]);

  const cycleEnhance = useCallback(() => {
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

  // Switch tabs (live, as focus moves across the bar). Resets the rows return
  // spot so dropping into the new tab lands at its first card.
  const switchTab = useCallback((id: TabId) => {
    setActiveTab(id);
    gridReturn.current = { zone: 'rows', row: 0, col: 0 };
  }, []);

  // Confirm on a card opens its details page.
  const activateItem = useCallback((item: ContentItem) => pushDetails(item), [pushDetails]);

  const activateTop = useCallback(
    (col: number) => {
      if (col === SEARCH_INDEX) setSearchOpen(true);
      else if (col === SETTINGS_INDEX) setSettingsOpen(true);
      else if (col === THEME_INDEX) toggleTheme();
      else {
        // A tab: switch to it and drop focus into its content.
        const id = TABS[col - FIRST_TAB_INDEX]?.id;
        if (id) {
          switchTab(id);
          setFocus({ zone: 'rows', row: 0, col: 0 });
        }
      }
    },
    [switchTab, toggleTheme],
  );

  const onAction = useCallback(
    (action: NavAction) => {
      // ---- Overlays own input while open ----
      if (searchOpen) {
        if (action === 'search') setSearchOpen(false);
        else searchActionRef.current?.(action);
        return;
      }
      if (settingsOpen) {
        if (action === 'back' || action === 'settings') setSettingsOpen(false);
        else settingsActionRef.current?.(action);
        return;
      }
      if (detailsItem) {
        detailsActionRef.current?.(action);
        return;
      }

      // ---- Global shortcut buttons (gamepad X/Y/Start, keyboard /,s,t,e) ----
      if (action === 'search') return setSearchOpen(true);
      if (action === 'settings') return setSettingsOpen(true);
      if (action === 'theme') return toggleTheme();
      if (action === 'enhance') return cycleEnhance();

      if (error) {
        if (action === 'confirm') loadLibrary();
        return;
      }
      if (!rows) return;

      const f = focusRef.current;
      const list = shelvesRef.current;

      // ---- Top bar ----
      if (f.zone === 'topbar') {
        switch (action) {
          case 'left': {
            const col = Math.max(0, f.col - 1);
            setFocus({ zone: 'topbar', row: 0, col });
            liveTab(col);
            break;
          }
          case 'right': {
            const col = Math.min(TOPBAR_COUNT - 1, f.col + 1);
            setFocus({ zone: 'topbar', row: 0, col });
            liveTab(col);
            break;
          }
          case 'down':
            if (list.length > 0) {
              const back = gridReturn.current;
              const row = Math.min(back.row, list.length - 1);
              const col = Math.min(back.col, Math.max(0, list[row].items.length - 1));
              setFocus({ zone: 'rows', row, col });
            }
            break;
          case 'back':
            if (activeTabRef.current !== 'foryou') {
              switchTab('foryou');
              setFocus({ zone: 'topbar', row: 0, col: tabIndex('foryou') });
            }
            break;
          case 'confirm':
            activateTop(f.col);
            break;
          default:
            break;
        }
        return;
      }

      // ---- Rows ----
      const row = list[f.row];
      if (!row) {
        // Empty tab: only route back to the bar.
        if (action === 'up' || action === 'back') setFocus({ zone: 'topbar', row: 0, col: tabIndex(activeTabRef.current) });
        return;
      }
      switch (action) {
        case 'left':
          setFocus({ ...f, col: Math.max(0, f.col - 1) });
          break;
        case 'right':
          setFocus({ ...f, col: Math.min(row.items.length - 1, f.col + 1) });
          break;
        case 'up':
          if (f.row === 0) {
            gridReturn.current = f;
            setFocus({ zone: 'topbar', row: 0, col: tabIndex(activeTabRef.current) });
          } else {
            const prev = list[f.row - 1];
            setFocus({ zone: 'rows', row: f.row - 1, col: Math.min(f.col, prev.items.length - 1) });
          }
          break;
        case 'down':
          if (f.row + 1 < list.length) {
            const next = list[f.row + 1];
            setFocus({ zone: 'rows', row: f.row + 1, col: Math.min(f.col, next.items.length - 1) });
          }
          break;
        case 'confirm': {
          const item = row.items[f.col];
          if (item) activateItem(item);
          break;
        }
        case 'back':
          gridReturn.current = f;
          setFocus({ zone: 'topbar', row: 0, col: tabIndex(activeTabRef.current) });
          break;
        default:
          break;
      }

      // Live tab preview while arrowing across the top bar.
      function liveTab(col: number) {
        if (col >= FIRST_TAB_INDEX && col < FIRST_TAB_INDEX + TABS.length) {
          const id = TABS[col - FIRST_TAB_INDEX].id;
          if (id !== activeTabRef.current) switchTab(id);
        }
      }
    },
    [
      rows,
      error,
      loadLibrary,
      searchOpen,
      settingsOpen,
      detailsItem,
      toggleTheme,
      cycleEnhance,
      switchTab,
      activateTop,
      activateItem,
    ],
  );
  useTvInput(onAction);

  // The item the hero previews: the focused card, or (on the top bar) the first
  // card of the active tab.
  const inRows = focus.zone === 'rows';
  const heroItem: ContentItem | undefined = inRows
    ? shelves[Math.min(focus.row, shelves.length - 1)]?.items[focus.col]
    : shelves[0]?.items[0];
  const preview = useHeroPreview(heroItem);

  const topFocus = focus.zone === 'topbar' ? focus.col : null;

  let body: React.ReactNode;
  if (error) {
    body = (
      <div className="screen-message">
        <p>{ERROR_COPY[error]}</p>
        <button className="tv-retry" autoFocus onClick={loadLibrary}>
          Retry
        </button>
      </div>
    );
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
        <Hero item={heroItem} preview={preview} onRows={inRows} />
        <main className="home-rows">
          {shelves.length === 0 ? (
            <div className="screen-message">{EMPTY_TAB_COPY[activeTab] || 'Nothing here yet.'}</div>
          ) : (
            shelves.map((shelf, i) => (
              <Shelf
                key={`${activeTab}-${i}-${shelf.title}`}
                title={shelf.title}
                items={shelf.items}
                focused={inRows && i === focus.row ? focus.col : null}
                jobs={jobs}
                onPick={activateItem}
              />
            ))
          )}
        </main>
      </>
    );
  }

  return (
    <div className="app">
      <TopBar
        activeTab={activeTab}
        focusIndex={topFocus}
        theme={theme}
        enabled={enabledTabs}
        onSearch={() => setSearchOpen(true)}
        onSelectTab={(id) => {
          switchTab(id);
          setFocus({ zone: 'rows', row: 0, col: 0 });
        }}
        onSettings={() => setSettingsOpen(true)}
        onToggleTheme={toggleTheme}
      />

      {body}

      <DownloadsPanel jobs={jobs} />
      {toast && <div className="toast">{toast}</div>}

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
          onToggleTheme={toggleTheme}
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
function useInstallJobs(onFinished: () => void) {
  const [jobs, setJobs] = useState<InstallJob[]>([]);
  const onFinishedRef = useRef(onFinished);
  onFinishedRef.current = onFinished;
  const runningIds = useRef<Set<string>>(new Set());
  const running = jobs.some((j) => j.status === 'running');
  const idleStreak = useRef(0);

  const refresh = useCallback(() => {
    fetchInstalls()
      .then((next) => {
        if (next.some((j) => j.status === 'done' && runningIds.current.has(j.id))) {
          onFinishedRef.current();
        }
        runningIds.current = new Set(next.filter((j) => j.status === 'running').map((j) => j.id));
        idleStreak.current = next.length > 0 ? 0 : idleStreak.current + 1;
        setJobs(next);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    let timer: number;
    const tick = () => {
      refresh();
      const delay = running
        ? 2000
        : Math.min(120_000, 15_000 * Math.max(1, idleStreak.current));
      timer = window.setTimeout(tick, delay);
    };
    tick();
    return () => window.clearTimeout(timer);
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
    // System tiles (Settings / app launchers) have no meta to fetch.
    if (!id || id.startsWith('sys:')) {
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
    const controller = new AbortController();
    const t = window.setTimeout(() => {
      fetchMeta(id, controller.signal)
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
      controller.abort();
    };
  }, [id]);
  return meta;
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
