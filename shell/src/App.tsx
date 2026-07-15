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
  queueInteractions,
  fetchSettings,
  saveSettings,
} from './api';
import { DetailsPage } from './DetailsPage';
import { activateFocused, moveFocus } from './focusNav';
import { Hero } from './Hero';
import { NavAction, useTvInput } from './input';
import { SearchOverlay } from './SearchOverlay';
import { SettingsPanel } from './SettingsPanel';
import { Shelf } from './Shelf';
import { TABS, TabId, rowsForTab, tabHasContent } from './tabs';
import { TopBar } from './TopBar';
import { Theme, applyAccent, applyTheme, initialTheme, otherTheme } from './theme';

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
  live_region: '',
  live_sports: '',
  live_leagues: '',
  live_teams: '',
  iptv_playlists: '',
  epg_urls: '',
  trakt_client_id: '',
  trakt_client_secret: '',
  trakt_token: '',
  anilist_token: '',
  twitch_client_id: '',
  twitch_token: '',
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
  live: 'Nothing live right now — follow sports channels and set your region in Settings, then check back around game time.',
  movies: 'No movies yet — add a TMDB key in Settings to fill this tab.',
  shows: 'No shows yet — add a TMDB key in Settings to fill this tab.',
  creators: 'Connect YouTube or Twitch in Settings to see creators you follow.',
  games: 'Connect a game library to see installed and recommended games.',
  library: 'Your library is empty — connect Steam, Epic or GOG, or start watching to fill it.',
};

export default function App() {
  const [rows, setRows] = useState<Row[] | null>(null);
  const [error, setError] = useState<LoadError | null>(null);
  const [activeTab, setActiveTab] = useState<TabId>('foryou');
  // Navigation moves real DOM focus (spatial nav, shared with Details/Search/
  // Settings). These mirror the focused element for the hero preview only.
  const [heroItem, setHeroItem] = useState<ContentItem | undefined>(undefined);
  const [zone, setZone] = useState<'topbar' | 'rows'>('rows');
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
  const overlayOpen = detailsItem !== null || settingsOpen || searchOpen;
  const pushDetails = useCallback((item: ContentItem) => setDetailsStack((s) => [...s, item]), []);
  const popDetails = useCallback(() => setDetailsStack((s) => s.slice(0, -1)), []);
  const { jobs, refresh: refreshJobs } = useInstallJobs(() => loadLibrary());

  // The shelves visible under the active tab: the tab's kind-filtered rows.
  const tabRows = useMemo(() => Object.fromEntries(
    TABS.map((tab) => [tab.id, rowsForTab(tab.id, rows ?? [])]),
  ) as Record<TabId, Row[]>, [rows]);
  const shelves = tabRows[activeTab];
  const featured = useMemo(() => {
    const preferred = shelves.find((r) => r.purpose === 'top_picks') || shelves[0];
    const items = preferred?.items.filter((item) => activeTab !== 'foryou' || item.kind === 'movie' || item.kind === 'series') ?? [];
    return items.slice(0, 6);
  }, [shelves]);
  const [featuredIndex, setFeaturedIndex] = useState(0);
  const lastNavigationAt = useRef(performance.now());
  useEffect(() => setFeaturedIndex(0), [activeTab]);
  useEffect(() => {
    if (featured.length < 2 || overlayOpen) return;
    const timer = window.setInterval(() => {
      if (performance.now() - lastNavigationAt.current >= 8_000) {
        setFeaturedIndex((i) => (i + 1) % featured.length);
      }
    }, 12_000);
    return () => window.clearInterval(timer);
  }, [featured.length, overlayOpen]);
  const featuredItem = featured[featuredIndex] || heroItem;
  const featuredRow = shelves.find((r) => r.items.some((i) => i.id === featuredItem?.id));

  // Which tabs currently have anything to show (dims empty tabs in the bar).
  const enabledTabs = useMemo(() => {
    const set = new Set<TabId>();
    for (const t of TABS) if (tabHasContent(t.id, rows ?? [])) set.add(t.id);
    return set;
  }, [rows]);

  const appRef = useRef<HTMLDivElement>(null);
  const toastTimer = useRef<number>();
  // Overlays register their nav handlers here; App forwards input while open.
  const detailsActionRef = useRef<((a: NavAction) => void) | null>(null);
  const settingsActionRef = useRef<((a: NavAction) => void) | null>(null);
  const searchActionRef = useRef<((a: NavAction) => void) | null>(null);
  // The last home control that held focus, restored when an overlay closes.
  const lastHomeFocus = useRef<string | null>(null);
  const focusDwell = useRef<number>();
  // Mirrors read inside event handlers (avoid stale closures).
  const overlayOpenRef = useRef(overlayOpen);
  overlayOpenRef.current = overlayOpen;
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

  const switchTab = useCallback((id: TabId) => setActiveTab(id), []);

  // Confirm on a card opens its details page.
  const activateItem = useCallback((item: ContentItem) => pushDetails(item), [pushDetails]);

  // ---- Home focus helpers (DOM focus, geometry nav) ----
  const rowsRoot = useCallback(
    () => appRef.current?.querySelector<HTMLElement>('.home-content, .screen-message') ?? null,
    [],
  );
  const topbarRoot = useCallback(
    () => appRef.current?.querySelector<HTMLElement>('.topbar') ?? null,
    [],
  );
  const focusFirstCard = useCallback(() => {
    const target = rowsRoot()?.querySelector<HTMLElement>(
      '.hero-primary, .card, .tv-retry, button, [tabindex]:not([tabindex="-1"])',
    );
    target?.focus();
  }, [rowsRoot]);
  const focusShelfCard = useCallback((shelfIndex: number, itemIndex: number) => {
    const cards = rowsRoot()?.querySelectorAll<HTMLElement>(`.card[data-shelf-index="${shelfIndex}"]`);
    if (!cards?.length) return false;
    const target = cards[Math.min(itemIndex, cards.length - 1)];
    target.focus();
    return true;
  }, [rowsRoot]);
  const focusActiveTab = useCallback(() => {
    (topbarRoot()?.querySelector<HTMLElement>('.top-tab-active') ??
      topbarRoot()?.querySelector<HTMLElement>('.top-tab'))?.focus();
  }, [topbarRoot]);

  // Report focus from home controls: keeps the hero in sync and remembers the
  // spot so returning from an overlay lands where you left.
  const onCardFocus = useCallback((item: ContentItem, el: HTMLElement) => {
    setZone('rows');
    lastHomeFocus.current = el.dataset.focusKey ?? `card:${item.id}`;
    window.clearTimeout(focusDwell.current);
    focusDwell.current = window.setTimeout(() => {
      queueInteractions([{ item_id: item.id, kind: 'focus', context: activeTabRef.current }]);
    }, 600);
  }, []);
  useEffect(() => () => window.clearTimeout(focusDwell.current), []);
  const firstItemOfTab = useCallback(
    (id: TabId) => tabRows[id][0]?.items[0],
    [tabRows],
  );
  const onTabFocus = useCallback(
    (id: TabId, el: HTMLElement) => {
      // Moving across the bar switches the tab live (its content previews below).
      switchTab(id);
      setHeroItem(firstItemOfTab(id));
      setZone('topbar');
      lastHomeFocus.current = el.dataset.focusKey ?? `tab:${id}`;
    },
    [switchTab, firstItemOfTab],
  );
  const onChromeFocus = useCallback(
    (el: HTMLElement) => {
      setHeroItem(firstItemOfTab(activeTabRef.current));
      setZone('topbar');
      lastHomeFocus.current = el.dataset.focusKey ?? null;
    },
    [firstItemOfTab],
  );

  // B / Home: from the rows, jump to the active tab; on the bar, fall back to
  // the For-you tab (then a second B on For-you is a no-op — nowhere higher).
  // Focusing the For-you tab directly lets its onFocus do the switch, avoiding
  // a switch-then-refocus race against React's commit.
  const homeBack = useCallback(() => {
    if (topbarRoot()?.contains(document.activeElement)) {
      if (activeTabRef.current !== 'foryou') {
        topbarRoot()?.querySelector<HTMLElement>('.top-tab[data-tab="foryou"]')?.focus();
      }
    } else {
      focusActiveTab();
    }
  }, [topbarRoot, focusActiveTab]);

  const onAction = useCallback(
    (action: NavAction) => {
      lastNavigationAt.current = performance.now();
      // ---- Overlays own input while open ----
      if (searchOpen) {
        if (action === 'search') setSearchOpen(false);
        else searchActionRef.current?.(action);
        return;
      }
      if (settingsOpen) {
        settingsActionRef.current?.(action);
        return;
      }

      // Global shortcuts, identical from home and from a details page.
      if (action === 'search') return setSearchOpen(true);
      if (action === 'settings') return setSettingsOpen(true);
      if (action === 'theme') return toggleTheme();
      if (action === 'enhance') return cycleEnhance();

      if (detailsItem) {
        detailsActionRef.current?.(action);
        return;
      }

      // ---- Home: real DOM focus, moved by geometry (same as every overlay) ----
      const root = appRef.current;
      if (!root) return;
      if (action === 'back') return homeBack();
      if (action === 'confirm') return activateFocused();

      const inTopbar = topbarRoot()?.contains(document.activeElement) ?? false;
      if (inTopbar) {
        // The bar is a widget with its own semantics: ◀▶ walks it (switching
        // tabs live via onFocus), ▼ drops into the grid, ▲ stays put.
        if (action === 'left' || action === 'right') moveFocus(topbarRoot()!, action);
        else if (action === 'down') focusFirstCard();
        return;
      }
      // In the grid: geometry within the rows. ▲ off the top row (no card above)
      // returns to the active tab rather than jumping to an arbitrary one.
      const rows = rowsRoot();
      if (!rows) return;
      const active = document.activeElement as HTMLElement | null;
      const card = active?.closest<HTMLElement>('.card[data-shelf-index][data-item-index]');
      if (card) {
        const shelfIndex = Number(card.dataset.shelfIndex);
        const itemIndex = Number(card.dataset.itemIndex);
        if (action === 'left') focusShelfCard(shelfIndex, Math.max(0, itemIndex - 1));
        else if (action === 'right') focusShelfCard(shelfIndex, itemIndex + 1);
        else if (action === 'down') focusShelfCard(shelfIndex + 1, itemIndex);
        else if (action === 'up') {
          if (shelfIndex > 0) focusShelfCard(shelfIndex - 1, itemIndex);
          else rows.querySelector<HTMLElement>('.hero-primary')?.focus();
        }
        return;
      }
      if (active?.closest('.hero-actions')) {
        if (action === 'down') focusShelfCard(0, 0);
        else if (action === 'left' || action === 'right') moveFocus(rows, action);
        else if (action === 'up') focusActiveTab();
        return;
      }
      if (action === 'up') {
        if (!moveFocus(rows, 'up')) focusActiveTab();
      } else {
        moveFocus(rows, action);
      }
    },
    [
      searchOpen,
      settingsOpen,
      detailsItem,
      toggleTheme,
      cycleEnhance,
      homeBack,
      topbarRoot,
      rowsRoot,
      focusFirstCard,
      focusShelfCard,
      focusActiveTab,
    ],
  );
  useTvInput(onAction);

  // Initial focus, and re-anchoring when returning from an overlay: land on the
  // remembered home control, else the first card. Skipped while an overlay owns
  // input so it can manage its own focus.
  useEffect(() => {
    if (overlayOpen) return;
    if (error) {
      rowsRoot()?.querySelector<HTMLElement>('.tv-retry')?.focus();
      return;
    }
    if (!rows) return;
    const key = lastHomeFocus.current;
    const el = key ? Array.from(appRef.current?.querySelectorAll<HTMLElement>('[data-focus-key]') ?? [])
      .find((candidate) => candidate.dataset.focusKey === key) : null;
    if (el) el.focus(); else focusFirstCard();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [overlayOpen, rows === null, error]);

  // Keep focus valid when the visible shelves change under us (library refresh
  // or a live tab switch can replace/shrink them): if the focused card fell out
  // of the document, drop to the first card so nav never dead-ends.
  useEffect(() => {
    if (overlayOpenRef.current || zone !== 'rows') return;
    const active = document.activeElement as HTMLElement | null;
    if (!active || active === document.body || !active.isConnected) {
      requestAnimationFrame(() => {
        if (!overlayOpenRef.current) focusFirstCard();
      });
    }
  }, [shelves, zone, focusFirstCard]);

  const preview = useHeroPreview(featuredItem);

  let body: React.ReactNode;
  if (error) {
    body = (
      <div className="screen-message">
        <p>{ERROR_COPY[error]}</p>
        <button className="tv-retry" onClick={loadLibrary} onFocus={(e) => onChromeFocus(e.currentTarget)}>
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
        <div className="home-content">
        <Hero item={featuredItem} preview={preview} explanation={featuredRow?.explanation} onOpen={activateItem} onFocus={(el) => onCardFocus(featuredItem!, el)} />
        <main className="home-rows">
          {shelves.length === 0 ? (
            <div className="screen-message">{EMPTY_TAB_COPY[activeTab] || 'Nothing here yet.'}</div>
          ) : (
            shelves.map((shelf, i) => (
              <Shelf
                key={`${activeTab}-${shelf.id || shelf.title}`}
                title={shelf.title}
                rowId={shelf.id}
                layout={shelf.layout}
                explanation={shelf.explanation}
                items={shelf.items}
                onPick={activateItem}
                onFocusItem={onCardFocus}
                jobs={jobs}
                shelfIndex={i}
              />
            ))
          )}
        </main>
        </div>
      </>
    );
  }

  return (
    <div className="app" ref={appRef}>
      <TopBar
        activeTab={activeTab}
        theme={theme}
        enabled={enabledTabs}
        onSearch={() => setSearchOpen(true)}
        onSelectTab={(id) => {
          switchTab(id);
          requestAnimationFrame(focusFirstCard);
        }}
        onSettings={() => setSettingsOpen(true)}
        onToggleTheme={toggleTheme}
        onFocusTab={onTabFocus}
        onFocusChrome={onChromeFocus}
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

  const refresh = useCallback(() => {
    fetchInstalls()
      .then((next) => {
        if (next.some((j) => j.status === 'done' && runningIds.current.has(j.id))) {
          onFinishedRef.current();
        }
        runningIds.current = new Set(next.filter((j) => j.status === 'running').map((j) => j.id));
        setJobs(next);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (!running) return;
    let timer: number | null = null;
    const tick = () => {
      refresh();
      timer = window.setTimeout(tick, 2000);
    };
    const onVisibilityChange = () => {
      if (timer !== null) window.clearTimeout(timer);
      timer = null;
      if (!document.hidden) {
        tick();
      }
    };
    if (!document.hidden) tick();
    document.addEventListener('visibilitychange', onVisibilityChange);
    return () => {
      if (timer !== null) window.clearTimeout(timer);
      document.removeEventListener('visibilitychange', onVisibilityChange);
    };
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
