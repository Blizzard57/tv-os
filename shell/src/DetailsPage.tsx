import { MutableRefObject, useCallback, useEffect, useMemo, useState } from 'react';
import {
  ContentItem,
  Episode,
  Meta,
  Stream,
  fetchMeta,
  fetchStreams,
  launch,
  playStream,
  startInstall,
} from './api';
import { NavAction } from './input';

interface Props {
  item: ContentItem;
  onClose: () => void;
  /** Called after something plays/installs so the home screen can refresh. */
  onPlayed: () => void;
  /** App writes its forwarded nav handler here while details is open. */
  actionRef: MutableRefObject<((a: NavAction) => void) | null>;
}

type Stage = 'actions' | 'episodes' | 'streams';

const KIND_BADGE: Record<Stream['kind'], string> = {
  direct: 'DIRECT',
  youtube: 'YOUTUBE',
  external: 'OPEN APP',
  torrent: 'TORRENT',
};

const isStreamItem = (id: string) => id.startsWith('strm:') || id.startsWith('tmdb:');

export function DetailsPage({ item, onClose, onPlayed, actionRef }: Props) {
  const [meta, setMeta] = useState<Meta | null>(null);
  const [stage, setStage] = useState<Stage>('actions');
  const [season, setSeason] = useState(1);
  const [episode, setEpisode] = useState<Episode | null>(null);
  const [streams, setStreams] = useState<Stream[] | null>(null);
  const [sel, setSel] = useState(0);
  const [status, setStatus] = useState<string | null>(null);

  const series = (meta?.kind ?? item.kind) === 'series';

  // Load metadata, then decide the opening stage.
  useEffect(() => {
    let cancelled = false;
    fetchMeta(item.id)
      .then((m) => {
        if (cancelled) return;
        setMeta(m);
        const isSeries = (m.kind || item.kind) === 'series';
        if (!isStreamItem(item.id)) {
          setStage('actions');
        } else if (isSeries) {
          const seasons = [...new Set(m.episodes.map((e) => e.season))].sort((a, b) => a - b);
          setSeason(seasons.find((s) => s >= 1) ?? seasons[0] ?? 1);
          setStage('episodes');
        } else {
          setStage('streams');
          loadStreams(item.id);
        }
      })
      .catch(() => setMeta({ ...emptyMeta(item) }));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [item.id]);

  const loadStreams = useCallback((id: string) => {
    setStreams(null);
    setSel(0);
    fetchStreams(id)
      .then(setStreams)
      .catch(() => setStreams([]));
  }, []);

  const seasons = useMemo(
    () => (meta ? [...new Set(meta.episodes.map((e) => e.season))].sort((a, b) => a - b) : []),
    [meta],
  );
  const episodesInSeason = useMemo(
    () => (meta ? meta.episodes.filter((e) => e.season === season) : []),
    [meta, season],
  );

  const flash = useCallback((msg: string) => {
    setStatus(msg);
    window.setTimeout(() => setStatus((s) => (s === msg ? null : s)), 3500);
  }, []);

  const playChosen = useCallback(
    (s: Stream) => {
      flash(s.kind === 'external' ? `Opening ${s.name}…` : `Playing ${s.name.split('\n')[0]}…`);
      playStream(s, item)
        .then(onPlayed)
        .catch((e) => flash(`Could not play: ${e.message}`));
    },
    [item, onPlayed, flash],
  );

  const runAction = useCallback(() => {
    if (item.action === 'install') {
      flash(`Downloading ${item.title}…`);
      startInstall(item.id).then(onPlayed).catch((e) => flash(`Could not install: ${e.message}`));
    } else {
      flash(`Launching ${item.title}…`);
      launch(item).then(onPlayed).catch((e) => flash(`Could not launch: ${e.message}`));
    }
  }, [item, onPlayed, flash]);

  const openEpisode = useCallback(
    (ep: Episode) => {
      setEpisode(ep);
      setStage('streams');
      loadStreams(`strm:series:${ep.id}`);
    },
    [loadStreams],
  );

  // The list the current stage navigates, and how to activate an entry.
  const handleAction = useCallback(
    (action: NavAction) => {
      const list: unknown[] =
        stage === 'episodes' ? episodesInSeason : stage === 'streams' ? (streams ?? []) : [0];
      switch (action) {
        case 'down':
          setSel((i) => Math.min(list.length - 1, i + 1));
          break;
        case 'up':
          setSel((i) => Math.max(0, i - 1));
          break;
        case 'left':
        case 'right':
          if (stage === 'episodes' && seasons.length > 1) {
            const i = seasons.indexOf(season);
            const next = action === 'left' ? i - 1 : i + 1;
            if (next >= 0 && next < seasons.length) {
              setSeason(seasons[next]);
              setSel(0);
            }
          }
          break;
        case 'confirm':
          if (stage === 'actions') runAction();
          else if (stage === 'episodes') {
            const ep = episodesInSeason[sel];
            if (ep) openEpisode(ep);
          } else if (stage === 'streams') {
            const s = streams?.[sel];
            if (s) playChosen(s);
          }
          break;
        case 'back':
          if (stage === 'streams' && series) {
            setStage('episodes');
            setSel(0);
          } else onClose();
          break;
        default:
          break;
      }
    },
    [stage, episodesInSeason, streams, seasons, season, sel, series, runAction, openEpisode, playChosen, onClose],
  );

  // Register the handler so App forwards controller/keyboard nav here.
  useEffect(() => {
    actionRef.current = handleAction;
    return () => {
      actionRef.current = null;
    };
  }, [actionRef, handleAction]);

  const title = meta?.title || item.title;
  const background = meta?.background || item.art;

  return (
    <div className="details">
      {background && (
        <div className="details-bg" style={{ backgroundImage: `url(${background})` }} />
      )}
      <div className="details-scrim" />

      <button className="details-back btn" onClick={onClose}>
        ← Back (B / Esc)
      </button>

      <div className="details-body">
        <div className="details-head">
          {(meta?.poster || item.art) && (
            <img className="details-poster" src={meta?.poster || item.art} alt={title} />
          )}
          <div className="details-info">
            <h1 className="details-title">{title}</h1>
            <div className="details-sub">
              {[meta?.release_info, meta?.runtime, meta?.rating && `★ ${meta.rating}`, meta?.genres?.slice(0, 3).join(', ')]
                .filter(Boolean)
                .join('  ·  ')}
            </div>
            {meta?.description && <p className="details-desc">{meta.description}</p>}
            {!meta && <p className="details-desc">Loading…</p>}
          </div>
        </div>

        {stage === 'actions' && (
          <div className="details-actions">
            <div className="row-item selected action-button">
              {item.action === 'install' ? 'Install' : 'Play'}
            </div>
          </div>
        )}

        {stage === 'episodes' && (
          <div className="episodes">
            {seasons.length > 1 && (
              <div className="season-tabs">
                {seasons.map((s) => (
                  <span key={s} className={`season-tab ${s === season ? 'season-tab-active' : ''}`}>
                    {s === 0 ? 'Specials' : `Season ${s}`}
                  </span>
                ))}
                <span className="details-hint">◀ ▶ to change season</span>
              </div>
            )}
            <div className="ep-list">
              {episodesInSeason.map((ep, i) => (
                <div
                  key={ep.id}
                  className={`ep-item ${i === sel ? 'selected' : ''}`}
                  onClick={() => {
                    setSel(i);
                    openEpisode(ep);
                  }}
                >
                  <span className="ep-num">
                    {ep.season}×{String(ep.episode).padStart(2, '0')}
                  </span>
                  <div className="ep-text">
                    <div className="ep-title">{ep.title}</div>
                    {ep.overview && <div className="ep-overview">{ep.overview}</div>}
                  </div>
                </div>
              ))}
              {episodesInSeason.length === 0 && <div className="details-hint">No episodes listed.</div>}
            </div>
          </div>
        )}

        {stage === 'streams' && (
          <div className="streams">
            <div className="streams-head">
              {episode
                ? `${episode.season}×${String(episode.episode).padStart(2, '0')} — ${episode.title}`
                : 'Sources'}
            </div>
            {streams === null && <div className="details-hint">Finding sources…</div>}
            {streams?.length === 0 && (
              <div className="details-hint">
                No sources found. Add or configure a stream addon (e.g. Torrentio, WatchHub) in
                Settings.
              </div>
            )}
            <div className="stream-list">
              {streams?.map((s, i) => (
                <div
                  key={`${s.url}-${i}`}
                  className={`stream-item ${i === sel ? 'selected' : ''}`}
                  onClick={() => {
                    setSel(i);
                    playChosen(s);
                  }}
                >
                  <span className={`stream-badge badge-${s.kind}`}>{KIND_BADGE[s.kind]}</span>
                  <div className="stream-text">
                    <div className="stream-name">{s.name.split('\n').join(' · ')}</div>
                    {s.title && <div className="stream-detail">{s.title.split('\n').join('  ')}</div>}
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>

      {status && <div className="toast">{status}</div>}
    </div>
  );
}

function emptyMeta(item: ContentItem): Meta {
  return { id: item.id, kind: item.kind, title: item.title, genres: [], episodes: [] };
}
