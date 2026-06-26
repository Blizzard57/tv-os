import { useCallback, useEffect, useRef, useState } from 'react';
import {
  ContentItem,
  EnhanceMode,
  InstallJob,
  Row,
  Settings,
  fetchInstalls,
  fetchLibrary,
  fetchSettings,
  saveSettings,
} from './api';
import { DetailsPage } from './DetailsPage';
import { NavAction, useTvInput } from './input';
import { MediaRow } from './MediaRow';
import { SettingsPanel } from './SettingsPanel';
import { Theme, applyTheme, initialTheme, otherTheme } from './theme';

// Focus is simply "which row, which column". Each row remembers its own
// column so moving up/down returns you to where you were in that row.
interface Focus {
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
};

export default function App() {
  const [rows, setRows] = useState<Row[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [focus, setFocus] = useState<Focus>({ row: 0, col: 0 });
  const [toast, setToast] = useState<string | null>(null);
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [settings, setSettings] = useState<Settings>(BLANK_SETTINGS);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [detailsItem, setDetailsItem] = useState<ContentItem | null>(null);
  const { jobs, refresh: refreshJobs } = useInstallJobs(() => loadLibrary());
  const columnMemory = useRef<number[]>([]);
  const toastTimer = useRef<number>();
  // DetailsPage registers its nav handler here; App forwards input while open.
  const detailsActionRef = useRef<((a: NavAction) => void) | null>(null);
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
  useEffect(() => {
    fetchSettings()
      .then((s) => setSettings({ ...BLANK_SETTINGS, ...s }))
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
      const row = Math.min(f.row, rows.length - 1);
      const col = Math.min(f.col, Math.max(0, rows[row].items.length - 1));
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

  const onAction = useCallback(
    (action: NavAction) => {
      // While the Settings panel is open it owns input; only let B/Start close it.
      if (settingsOpen) {
        if (action === 'back' || action === 'settings') setSettingsOpen(false);
        return;
      }
      // While the details page is open it owns input (it closes itself on back).
      if (detailsItem) {
        detailsActionRef.current?.(action);
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
        const next =
          ENHANCE_CYCLE[(ENHANCE_CYCLE.indexOf(settings.enhance) + 1) % ENHANCE_CYCLE.length];
        const updated = { ...settings, enhance: next };
        setSettings(updated);
        showToast(`Enhance: ${ENHANCE_LABELS[next]}`);
        saveSettings(updated).catch((e) => showToast(`Could not save: ${e.message}`));
        return;
      }
      if (!rows || rows.length === 0) return;
      if (action === 'confirm') {
        const f = focusRef.current;
        const item = rows[f.row]?.items[f.col];
        if (item) setDetailsItem(item); // open the details page for every entry
        return;
      }
      if (action === 'back') return;
      setFocus((f) => {
        switch (action) {
          case 'left':
            return { ...f, col: Math.max(0, f.col - 1) };
          case 'right':
            return { ...f, col: Math.min(rows[f.row].items.length - 1, f.col + 1) };
          case 'up':
          case 'down': {
            const row = action === 'up' ? f.row - 1 : f.row + 1;
            if (row < 0 || row >= rows.length) return f;
            columnMemory.current[f.row] = f.col;
            const col = Math.min(columnMemory.current[row] ?? 0, rows[row].items.length - 1);
            return { row, col };
          }
          default:
            return f;
        }
      });
    },
    [rows, showToast, theme, settings, settingsOpen, detailsItem],
  );
  useTvInput(onAction);

  const focusedItem: ContentItem | undefined = rows?.[focus.row]?.items[focus.col];

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
          <div className="hero-kind">{focusedItem?.kind.toUpperCase()}</div>
          <h1 className="hero-title">{focusedItem?.title}</h1>
          <div className="hero-hint">
            Press <span className="key">A</span> / <span className="key">Enter</span> to open
          </div>
        </header>

        <main className="rows">
          {rows.map((row, i) => (
            <MediaRow
              key={row.title}
              row={row}
              focusedCol={i === focus.row ? focus.col : null}
              restingCol={columnMemory.current[i] ?? 0}
              jobs={jobs}
            />
          ))}
        </main>
      </>
    );
  }

  return (
    <div className="app">
      {focusedItem?.art && (
        <div
          key={focusedItem.id}
          className="ambient"
          style={{ backgroundImage: `url(${focusedItem.art})` }}
        />
      )}

      <div className="status-chips">
        <button
          className="status-chip status-chip-button"
          onClick={() => setSettingsOpen(true)}
          title="Settings (Start / S)"
        >
          ⚙ SETTINGS
        </button>
        <div className="status-chip" title="Press E / X to change">
          ENHANCE · {ENHANCE_LABELS[settings.enhance].toUpperCase()}
        </div>
      </div>

      {body}

      <DownloadsPanel jobs={jobs} />
      {toast && <div className="toast">{toast}</div>}
      {detailsItem && (
        <DetailsPage
          item={detailsItem}
          onClose={() => setDetailsItem(null)}
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
