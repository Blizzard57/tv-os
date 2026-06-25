import { useCallback, useEffect, useRef, useState } from 'react';
import {
  ContentItem,
  EnhanceMode,
  InstallJob,
  Row,
  fetchInstalls,
  fetchLibrary,
  fetchSettings,
  launch,
  saveSettings,
  startInstall,
} from './api';
import { NavAction, useTvInput } from './input';
import { MediaRow } from './MediaRow';
import { Theme, applyTheme, initialTheme, otherTheme } from './theme';

// Focus is simply "which row, which column". Each row remembers its own
// column so moving up/down returns you to where you were in that row.
interface Focus {
  row: number;
  col: number;
}

const HERO_HINTS: Record<ContentItem['action'], string> = {
  play: 'to play',
  install: 'to install',
  none: '— not playable yet',
};

const ENHANCE_CYCLE: EnhanceMode[] = ['auto', 'quality', 'performance', 'off'];
const ENHANCE_LABELS: Record<EnhanceMode, string> = {
  auto: 'Auto',
  quality: 'Quality',
  performance: 'Performance',
  off: 'Off',
};

export default function App() {
  const [rows, setRows] = useState<Row[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [focus, setFocus] = useState<Focus>({ row: 0, col: 0 });
  const [toast, setToast] = useState<string | null>(null);
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [enhance, setEnhance] = useState<EnhanceMode>('auto');
  const { jobs, refresh: refreshJobs } = useInstallJobs(() => loadLibrary());
  const columnMemory = useRef<number[]>([]);
  const toastTimer = useRef<number>();
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
      .then((s) => setEnhance(s.enhance))
      .catch(() => {}); // daemon default is shown until it answers
  }, []);

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

  const activate = useCallback(
    (item: ContentItem) => {
      switch (item.action) {
        case 'play':
          showToast(`Launching ${item.title}…`);
          launch(item).catch((e) => showToast(`Could not launch: ${e.message}`));
          break;
        case 'install':
          showToast(`Downloading ${item.title}…`);
          startInstall(item.id)
            .then(refreshJobs)
            .catch((e) => showToast(`Could not install: ${e.message}`));
          break;
        case 'none':
          showToast('Not playable yet — stream sources arrive in a later phase');
          break;
      }
    },
    [showToast, refreshJobs],
  );

  const onAction = useCallback(
    (action: NavAction) => {
      if (action === 'theme') {
        const next = otherTheme(theme);
        setTheme(next);
        showToast(next === 'light' ? 'Light mode' : 'Dark mode');
        return;
      }
      if (action === 'enhance') {
        const next = ENHANCE_CYCLE[(ENHANCE_CYCLE.indexOf(enhance) + 1) % ENHANCE_CYCLE.length];
        setEnhance(next);
        showToast(`Enhance: ${ENHANCE_LABELS[next]}`);
        saveSettings({ enhance: next }).catch((e) => showToast(`Could not save: ${e.message}`));
        return;
      }
      if (!rows || rows.length === 0) return;
      if (action === 'confirm') {
        const f = focusRef.current;
        const item = rows[f.row]?.items[f.col];
        if (item) activate(item);
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
    [rows, activate, showToast, theme, enhance],
  );
  useTvInput(onAction);

  if (error) return <div className="screen-message">{error}</div>;
  if (!rows) return <div className="screen-message">Loading…</div>;
  if (rows.length === 0) {
    return <div className="screen-message">Nothing here yet — install a game or add videos.</div>;
  }

  const focusedItem: ContentItem | undefined = rows[focus.row]?.items[focus.col];

  return (
    <div className="app">
      {focusedItem?.art && (
        <div
          key={focusedItem.id}
          className="ambient"
          style={{ backgroundImage: `url(${focusedItem.art})` }}
        />
      )}

      <div className="status-chip" title="Press E / X to change">
        ENHANCE · {ENHANCE_LABELS[enhance].toUpperCase()}
      </div>

      <header className="hero">
        <div className="hero-kind">{focusedItem?.kind.toUpperCase()}</div>
        <h1 className="hero-title">{focusedItem?.title}</h1>
        <div className="hero-hint">
          Press <span className="key">A</span> / <span className="key">Enter</span>{' '}
          {HERO_HINTS[focusedItem?.action ?? 'play']}
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

      <DownloadsPanel jobs={jobs} />
      {toast && <div className="toast">{toast}</div>}
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
