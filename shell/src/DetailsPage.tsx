import {
  MutableRefObject,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from 'react';
import {
  ContentItem,
  Episode,
  GameExtras,
  Meta,
  PlaybackStatus,
  PreferenceAction,
  PreferenceStatus,
  ResumeInfo,
  Stream,
  fetchGameExtras,
  fetchMeta,
  fetchPlayback,
  fetchPreference,
  fetchResume,
  fetchSimilar,
  fetchStreams,
  launch,
  playStream,
  recordInteraction,
  resolveLive,
  setPreference,
  startInstall,
} from './api';
import { gameLogo } from './cards';
import { NavAction } from './input';
import { focusPrimary, useSpatialNav } from './spatialNav';

interface Props {
  item: ContentItem;
  onClose: () => void;
  /** Opens another item's details page ("More like this"). */
  onOpen: (item: ContentItem) => void;
  /** Called after something plays/installs so the home screen can refresh. */
  onPlayed: () => void;
  /** App writes its forwarded nav handler here while details is open. */
  actionRef: MutableRefObject<((a: NavAction) => void) | null>;
}

type Stage = 'actions' | 'episodes' | 'streams';
type PrefButton = { action: PreferenceAction; icon: string; label: string; active: boolean };

const isGameId = (id: string) => id.startsWith('steam:') || id.startsWith('gshop:');

const KIND_BADGE: Record<Stream['kind'], string> = {
  direct: 'DIRECT',
  youtube: 'YOUTUBE',
  external: 'OPEN APP',
  torrent: 'TORRENT',
};

const isStreamItem = (id: string) => id.startsWith('strm:') || id.startsWith('tmdb:');
// Unowned games (GameHub): their "sources" are store offers, cheapest first.
const isShopItem = (id: string) => id.startsWith('gshop:');

export function DetailsPage({ item, onClose, onOpen, onPlayed, actionRef }: Props) {
  const [meta, setMeta] = useState<Meta | null>(null);
  const [stage, setStage] = useState<Stage>('actions');
  const [season, setSeason] = useState(1);
  const [episode, setEpisode] = useState<Episode | null>(null);
  const [streams, setStreams] = useState<Stream[] | null>(null);
  // Sources are compact by default (auto-chosen best pick + a toggle); this
  // expands the full ranked list.
  const [showAllSources, setShowAllSources] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [loading, setLoading] = useState<{ label: string; kind: LoadingKind } | null>(null);
  const [similar, setSimilar] = useState<ContentItem[]>([]);
  const [extras, setExtras] = useState<GameExtras>({});
  const [metaError, setMetaError] = useState(false);
  // Which control the on-screen highlight paints, mirrored from real DOM focus
  // (browser focus is the source of truth; geometry in `moveFocus` drives it).
  const [focusedKey, setFocusedKey] = useState<string | null>(null);
  // Monotonic tokens so a late stream/meta response for a previous item can
  // never overwrite the current one.
  const streamToken = useRef(0);
  const metaToken = useRef(0);
  // Track pending status/flash timeouts so we can clear them on unmount.
  const flashTimer = useRef<number | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [resume, setResume] = useState<ResumeInfo | null>(null);
  const [logoFailed, setLogoFailed] = useState(false);
  const [pref, setPref] = useState<PreferenceStatus>({
    watchlist: false,
    watched: false,
    liked: false,
    disliked: false,
  });
  // Bumped whenever focus should re-anchor to the page's primary action (page
  // opened, streams arrived, episode picked, sub-view toggled).
  const [refocusKey, bumpRefocus] = useReducer((n: number) => n + 1, 0);

  const series = (meta?.kind ?? item.kind) === 'series';
  const isGame = (meta?.kind ?? item.kind) === 'game';
  // A "Resume" entry leads the list when there's a saved spot to continue from.
  // The saved position is keyed by the show (item.id), so it belongs on the
  // episode *overview* (continue the last-watched episode) and on a movie's
  // sources — but NOT inside one specific episode's source list, where it would
  // advertise a position that may belong to a different episode entirely.
  const resumeShown =
    !!resume &&
    isStreamItem(item.id) &&
    (stage === 'episodes' || (stage === 'streams' && !episode));
  // Only real stream sources (Torrentio &c.) get the "best pick + reveal the
  // rest" treatment. Shop offers are always shown in full (cheapest first).
  const canCompactSources = isStreamItem(item.id) && !isShopItem(item.id);
  const sourcesExpanded = !canCompactSources || showAllSources;
  const bestStream = streams && streams.length > 0 ? streams[0] : null;

  // Load metadata, then decide the opening stage.
  // Is there a saved spot/source to continue from?
  useEffect(() => {
    setResume(null);
    if (isStreamItem(item.id)) {
      fetchResume(item.id)
        .then((r) => setResume(r && r.position > 0 ? r : null))
        .catch(() => setResume(null));
    }
  }, [item.id]);

  // "More like this" — arrives lazily; the strip appears when it has items.
  useEffect(() => {
    fetchSimilar(item.id)
      .then(setSimilar)
      .catch(() => setSimilar([]));
  }, [item.id]);

  // Game extras: playtime, HowLongToBeat, achievements.
  useEffect(() => {
    setExtras({});
    if (isGameId(item.id)) {
      fetchGameExtras(item.id)
        .then(setExtras)
        .catch(() => setExtras({}));
    }
  }, [item.id]);

  useEffect(() => {
    const token = ++metaToken.current;
    // Reset per-item view state so a stale stage/selection can't leak across.
    setMeta(null);
    setMetaError(false);
    setEpisode(null);
    fetchMeta(item.id)
      .then((m) => {
        if (token !== metaToken.current) return; // superseded by a newer item
        setMeta(m);
        const isSeries = (m.kind || item.kind) === 'series';
        if (isShopItem(item.id)) {
          // Not owned — the page is about where to buy it.
          setStage('streams');
          loadStreams(item.id);
        } else if (!isStreamItem(item.id)) {
          setStage('actions');
          bumpRefocus();
        } else if (isSeries) {
          const seasons = [...new Set(m.episodes.map((e) => e.season))].sort((a, b) => a - b);
          setSeason(seasons.find((s) => s >= 1) ?? seasons[0] ?? 1);
          setStage('episodes');
          bumpRefocus();
        } else {
          setStage('streams');
          loadStreams(item.id);
        }
      })
      .catch(() => {
        if (token !== metaToken.current) return;
        setMetaError(true);
        setMeta({ ...emptyMeta(item) });
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [item.id]);

  useEffect(() => setLogoFailed(false), [item.id]);

  useEffect(() => {
    setPref({ watchlist: false, watched: false, liked: false, disliked: false });
    fetchPreference(item.id)
      .then(setPref)
      .catch(() => {});
  }, [item.id]);

  const loadStreams = useCallback((id: string) => {
    const token = ++streamToken.current;
    setStreams(null);
    setShowAllSources(false);
    fetchStreams(id)
      .then((s) => {
        if (token !== streamToken.current) return; // a newer load won
        setStreams(s);
        bumpRefocus();
      })
      .catch(() => {
        if (token !== streamToken.current) return;
        setStreams([]);
      });
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
    if (flashTimer.current !== null) window.clearTimeout(flashTimer.current);
    flashTimer.current = window.setTimeout(() => {
      flashTimer.current = null;
      setStatus((s) => (s === msg ? null : s));
    }, 3500);
  }, []);

  // Clear any pending status timeout so we never setState after unmount.
  useEffect(
    () => () => {
      if (flashTimer.current !== null) window.clearTimeout(flashTimer.current);
    },
    [],
  );

  // The precise id being watched: for an episode it's `strm:series:<imdb>:s:e`
  // so Trakt/AniList scrobble the exact episode (the show `item` still drives
  // "Continue"). Movies just watch under their own id.
  const trackId = episode ? `strm:series:${episode.id}` : item.id;
  const nextTrackId = episode && meta?.episodes
    ? (() => {
        const ordered = [...meta.episodes].sort((a, b) => a.season - b.season || a.episode - b.episode);
        const index = ordered.findIndex((candidate) => candidate.id === episode.id);
        return index >= 0 && ordered[index + 1] ? `strm:series:${ordered[index + 1].id}` : undefined;
      })()
    : undefined;

  const playChosen = useCallback(
    (s: Stream) => {
      const playbackItem: ContentItem = {
        ...item,
        title: episode?.title
          ? `${meta?.title || item.title} - ${episode.title}`
          : meta?.title || item.title,
        art: meta?.poster || meta?.background || item.art,
      };
      // External links just open elsewhere — no loading screen needed.
      recordInteraction({ item_id: trackId, kind: 'play', context: 'details' }).catch(() => {});
      if (s.kind === 'external') {
        flash(`Opening ${s.name.split('\n')[0]}…`);
        playStream(s, playbackItem, trackId, nextTrackId, meta?.genres)
          .then(waitForPlayback)
          .then(onPlayed)
          .catch((e) => flash(`Could not open: ${e.message}`));
        return;
      }
      // The daemon acknowledges quickly, then reports the real start/fail state
      // through /api/playback so slow torrents do not hit the shell timeout.
      setLoading({ label: s.name.split('\n')[0] || meta?.title || item.title, kind: 'stream' });
      playStream(s, playbackItem, trackId, nextTrackId, meta?.genres)
        .then(waitForPlayback)
        .then(() => {
          setLoading(null);
          onPlayed();
        })
        .catch((e) => {
          setLoading(null);
          flash(`Could not play: ${e.message}`);
        });
    },
    [item, episode, onPlayed, flash, meta, trackId, nextTrackId],
  );

  const runAction = useCallback(async () => {
    if (item.action === 'install') {
      flash(`Downloading ${item.title}…`);
      startInstall(item.id).then(onPlayed).catch((e) => flash(`Could not install: ${e.message}`));
    } else if (item.id.startsWith('live:sched:')) {
      setLoading({ label: 'Checking verified broadcasts…', kind: 'stream' });
      try {
        const result = await resolveLive(item.id);
        setLoading(null);
        if (!result.resolved || !result.item) { flash(result.reason || 'No verified broadcast is available yet.'); return; }
        recordInteraction({ item_id: result.item.id, kind: 'play', context: 'live_guide' }).catch(() => {});
        setLoading({ label: result.item.title, kind: 'stream' });
        await launch(result.item).then(waitForPlayback);
        setLoading(null); onPlayed();
      } catch (e) { setLoading(null); flash(`Could not resolve broadcast: ${(e as Error).message}`); }
    } else if (item.action === 'none') {
      flash('This event is not available to play yet.');
    } else {
      recordInteraction({ item_id: item.id, kind: 'play', context: 'details' }).catch(() => {});
      setLoading({ label: item.title, kind: 'app' });
      launch(item)
        .then(waitForPlayback)
        .then(() => {
          setLoading(null);
          onPlayed();
        })
        .catch((e) => {
          setLoading(null);
          flash(`Could not launch: ${e.message}`);
        });
    }
  }, [item, onPlayed, flash]);

  const runPreference = useCallback(
    (action: PreferenceAction) => {
      const preferenceItem: ContentItem = {
        ...item,
        title: meta?.title || item.title,
        art: meta?.poster || meta?.background || item.art,
      };
      setPreference(action, preferenceItem)
        .then((next) => {
          recordInteraction({ item_id: item.id, kind: action === 'watched' ? 'complete' : action, context: 'details' }).catch(() => {});
          setPref(next);
          const label =
            action === 'watchlist'
              ? next.watchlist
                ? 'Added to watchlist'
                : 'Removed from watchlist'
              : action === 'watched'
                ? next.watched
                  ? 'Marked as watched'
                  : 'Marked as unwatched'
                : action === 'like'
                  ? next.liked
                    ? 'Liked'
                    : 'Like removed'
                  : next.disliked
                    ? 'Disliked'
                    : 'Dislike removed';
          flash(label);
          onPlayed();
        })
        .catch((e) => flash(`Could not save preference: ${e.message}`));
    },
    [item, meta, flash, onPlayed],
  );

  const openEpisode = useCallback(
    (ep: Episode) => {
      setEpisode(ep);
      setStage('streams');
      loadStreams(`strm:series:${ep.id}`);
    },
    [loadStreams],
  );

  const expandSources = useCallback(() => {
    setShowAllSources(true);
    bumpRefocus();
  }, []);

  const pickSeason = useCallback((s: number) => {
    setSeason(s);
    bumpRefocus();
  }, []);

  // B / Esc peels one layer: expanded sources → compact, series streams →
  // episode list, otherwise close the page.
  const handleBack = useCallback(() => {
    if (stage === 'streams' && showAllSources && canCompactSources) {
      setShowAllSources(false);
      bumpRefocus();
    } else if (stage === 'streams' && series) {
      setStage('episodes');
      bumpRefocus();
    } else {
      onClose();
    }
  }, [stage, showAllSources, canCompactSources, series, onClose]);

  // One nav model for the whole OS: directions walk on-screen controls by
  // geometry, A activates the focused one, B runs `handleBack`.
  useSpatialNav(actionRef, rootRef, {
    onBack: handleBack,
    blocked: () => !!loading,
  });

  // Re-anchor focus on the primary action after an intentional context change.
  useLayoutEffect(() => {
    focusPrimary(rootRef.current);
  }, [refocusKey]);

  const title = meta?.title || item.title;
  const background = meta?.background || item.art;
  const boxArt = meta?.poster || item.art;
  const titleLogo = !logoFailed ? meta?.logo || gameLogo(item) : null;
  const kindLabel = (meta?.kind ?? item.kind).toUpperCase();
  const detailChips = [
    meta?.rating && {
      kind: isGame ? 'score' : 'rating',
      text: isGame ? `Metacritic ${meta.rating}` : meta.rating,
    },
    meta?.release_info && { kind: 'text', text: meta.release_info },
    meta?.runtime && { kind: 'text', text: meta.runtime },
    series &&
      seasons.length > 0 && {
        kind: 'text',
        text: `${seasons.length} season${seasons.length > 1 ? 's' : ''}`,
      },
    ...(meta?.genres ?? []).slice(0, 2).map((g) => ({ kind: 'text', text: g })),
  ].filter(Boolean) as { kind: string; text: string }[];
  const facts = [
    meta?.rating && {
      label: isGame ? 'Metacritic' : 'Rating',
      value: isGame ? String(meta.rating) : meta.rating,
    },
    meta?.release_info && { label: 'Released', value: meta.release_info },
    meta?.runtime && { label: 'Runtime', value: meta.runtime },
    series &&
      seasons.length > 0 && {
        label: 'Seasons',
        value: `${seasons.length} · ${meta?.episodes.length ?? 0} episodes`,
      },
    !!meta?.genres?.length && { label: 'Genres', value: meta.genres.join(', ') },
    meta?.developer && { label: 'Developer', value: meta.developer },
    meta?.publisher &&
      meta.publisher !== meta.developer && { label: 'Publisher', value: meta.publisher },
  ].filter(Boolean) as { label: string; value: string }[];
  const prefButtons: PrefButton[] = [
    { action: 'watchlist', icon: '+', label: 'Watchlist', active: pref.watchlist },
    { action: 'watched', icon: '○', label: 'Watched', active: pref.watched },
    { action: 'like', icon: '↑', label: 'Like', active: pref.liked },
    { action: 'dislike', icon: '↓', label: 'Dislike', active: pref.disliked },
  ];

  const achAll = extras.achievements
    ? [...extras.achievements.unlocked, ...extras.achievements.locked]
    : [];

  // Which control gets `data-primary` (the focus target after a context reset).
  const primaryKey: string | null = resumeShown
    ? 'resume'
    : stage === 'actions'
      ? 'action'
      : stage === 'episodes'
        ? episodesInSeason.length > 0
          ? 'ep:0'
          : null
        : stage === 'streams'
          ? sourcesExpanded
            ? streams && streams.length > 0
              ? 'source:0'
              : null
            : bestStream
              ? 'source:best'
              : null
          : null;

  // Focus wiring shared by every navigable control: makes it a real tab stop,
  // mirrors focus into `focusedKey` for the highlight, and flags the primary.
  const nav = (key: string) => ({
    tabIndex: 0,
    'data-primary': key === primaryKey ? true : undefined,
    onFocus: () => setFocusedKey(key),
  });
  const on = (key: string) => (focusedKey === key ? 'selected' : '');
  const focusZone = focusedKey?.split(':')[0];

  return (
    <div className="details" ref={rootRef}>
      {background && (
        <div className="details-bg" style={{ backgroundImage: `url(${background})` }} />
      )}
      <div className="details-scrim" />

      <button className="details-back btn" onClick={onClose}>
        ← Back (B / Esc)
      </button>

      <div className="details-body">
        <div className="details-head">
          <div className="details-info">
            <div className="details-kind">{kindLabel}</div>
            {titleLogo ? (
              <img
                key={titleLogo}
                className="details-title-logo"
                src={titleLogo}
                alt={title}
                onError={() => setLogoFailed(true)}
              />
            ) : (
              <h1 className="details-title">{title}</h1>
            )}
            {!!detailChips.length && (
              <div className="details-sub">
                {detailChips.map((chip, i) => (
                  <span key={`${chip.text}-${i}`} className={`details-chip chip-${chip.kind}`}>
                    {chip.text}
                  </span>
                ))}
              </div>
            )}
            {(extras.playtime_minutes != null || extras.hltb || extras.achievements) && (
              <div className="game-stats">
                {extras.playtime_minutes != null && (
                  <span className="game-stat">
                    <span className="game-stat-label">Played</span>
                    {formatHours(extras.playtime_minutes / 60)}
                  </span>
                )}
                {extras.hltb && (
                  <>
                    <span className="game-stat">
                      <span className="game-stat-label">Story</span>
                      {formatHours(extras.hltb.main)}
                    </span>
                    <span className="game-stat">
                      <span className="game-stat-label">Story + extras</span>
                      {formatHours(extras.hltb.main_extra)}
                    </span>
                    <span className="game-stat">
                      <span className="game-stat-label">Completionist</span>
                      {formatHours(extras.hltb.completionist)}
                    </span>
                  </>
                )}
                {extras.achievements && (
                  <span className="game-stat">
                    <span className="game-stat-label">Achievements</span>
                    {extras.achievements.unlocked.length} /{' '}
                    {extras.achievements.unlocked.length + extras.achievements.locked.length}
                  </span>
                )}
              </div>
            )}
            {metaError ? (
              <p className="details-desc">
                Couldn't load the details for this title — the daemon may be busy or offline.
                Press <span className="key">B</span> / Esc to go back and try again.
              </p>
            ) : (
              <>
                {meta?.description && <p className="details-desc">{meta.description}</p>}
                {!meta && <p className="details-desc">Loading…</p>}
              </>
            )}

            <div className="details-quick-actions">
              {!!meta?.trailers?.length && (
                <button type="button" className={`details-round-action ${on('trailer:0')}`} {...nav('trailer:0')}
                  onClick={() => playChosen({ kind: 'youtube', url: meta.trailers![0], name: 'Trailer', title: meta.title })}>
                  <span className="round-action-icon">▶</span><span>Trailer</span>
                </button>
              )}
              {prefButtons.map((button, i) => (
                <button
                  key={button.action}
                  type="button"
                  className={`details-round-action ${button.active ? 'active' : ''} ${on(`pref:${i}`)}`}
                  {...nav(`pref:${i}`)}
                  onClick={() => runPreference(button.action)}
                >
                  <span className="round-action-icon">{button.icon}</span>
                  <span>{button.label}</span>
                </button>
              ))}
            </div>
          </div>
        </div>

        {boxArt && (
          <aside className="details-box-panel" aria-hidden="true">
            <img className="details-box-art" src={boxArt} alt="" />
          </aside>
        )}

        {stage === 'actions' && (
          <div className="details-actions">
            <div
              className={`row-item action-button ${on('action')}`}
              {...nav('action')}
              onClick={runAction}
            >
              {item.action === 'install' ? 'Install' : item.id.startsWith('live:sched:') ? 'Check broadcast' : item.action === 'none' ? 'Coming soon' : 'Play'}
            </div>
          </div>
        )}

        {resumeShown && resume && (
          <div
            className={`resume-btn ${on('resume')}`}
            {...nav('resume')}
            onClick={() => playChosen(resume.stream)}
          >
            <span className="resume-play">▶</span>
            <span>
              <span className="resume-label">Resume</span>
              <span className="resume-detail">{formatTime(resume.position)} · same source</span>
            </span>
          </div>
        )}

        {stage === 'episodes' && (
          <div className="episodes">
            {seasons.length > 1 && (
              <div className="season-tabs">
                {seasons.map((s) => (
                  <button
                    key={s}
                    type="button"
                    className={`season-tab ${s === season ? 'season-tab-active' : ''} ${on(`season:${s}`)}`}
                    {...nav(`season:${s}`)}
                    onClick={() => pickSeason(s)}
                  >
                    {s === 0 ? 'Specials' : `Season ${s}`}
                  </button>
                ))}
              </div>
            )}
            <div className="ep-list">
              {episodesInSeason.map((ep, i) => (
                <div
                  key={ep.id}
                  className={`ep-item ${on(`ep:${i}`)}`}
                  {...nav(`ep:${i}`)}
                  onClick={() => openEpisode(ep)}
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
              {isShopItem(item.id)
                ? 'Where to buy — cheapest first'
                : episode
                  ? `${episode.season}×${String(episode.episode).padStart(2, '0')} — ${episode.title}`
                  : resumeShown
                    ? 'Play from the start'
                    : 'Play'}
            </div>
            {streams === null && (
              <div className="details-hint">
                {isShopItem(item.id) ? 'Comparing store prices…' : 'Finding the best source…'}
              </div>
            )}
            {streams?.length === 0 && (
              <div className="details-hint">
                {isShopItem(item.id)
                  ? 'No store offers found right now.'
                  : 'No sources found. Add or configure a stream addon (e.g. Torrentio, WatchHub) in Settings.'}
              </div>
            )}

            {/* Compact view: the auto-chosen best pick + a reveal toggle. */}
            {!sourcesExpanded && bestStream && (
              <div className="source-picker">
                {(() => {
                  const d = describeSource(bestStream);
                  return (
                    <div
                      className={`best-source ${on('source:best')}`}
                      {...nav('source:best')}
                      onClick={() => playChosen(bestStream)}
                    >
                      <span className="best-play">▶</span>
                      <div className="best-text">
                        <div className="best-line">
                          {/* The "Play from the start" section header already says
                              this when a Resume entry is present, so keep the card
                              label short rather than repeating the phrase. */}
                          <span className="best-label">Play now</span>
                          {d.chips.map((c) => (
                            <span key={c} className="source-chip">
                              {c}
                            </span>
                          ))}
                          <span className={`stream-badge badge-${bestStream.kind}`}>
                            {KIND_BADGE[bestStream.kind]}
                          </span>
                        </div>
                        {d.detail && <div className="best-detail">{d.detail}</div>}
                      </div>
                    </div>
                  );
                })()}
                {streams && streams.length > 1 && (
                  <div
                    className={`source-toggle ${on('source:toggle')}`}
                    {...nav('source:toggle')}
                    onClick={expandSources}
                  >
                    Show all {streams.length} sources ▾
                  </div>
                )}
              </div>
            )}

            {/* Expanded view: the full ranked list, scrollable. */}
            {sourcesExpanded && !!streams?.length && (
              <div className="stream-list">
                {streams.map((s, i) => (
                  <div
                    key={`${s.url}-${i}`}
                    className={`stream-item ${on(`source:${i}`)} ${
                      i === 0 && canCompactSources ? 'stream-best' : ''
                    }`}
                    {...nav(`source:${i}`)}
                    onClick={() => playChosen(s)}
                  >
                    <span className={`stream-badge badge-${s.kind}`}>{KIND_BADGE[s.kind]}</span>
                    <div className="stream-text">
                      <div className="stream-name">
                        {s.name.split('\n').join(' · ')}
                        {i === 0 && canCompactSources && <span className="best-tag">BEST</span>}
                      </div>
                      {s.title && <div className="stream-detail">{s.title.split('\n').join('  ')}</div>}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}

        {(facts.length > 0 || !!meta?.tags?.length) && (
          <div className="details-facts">
            {facts.length > 0 && (
              <section className="details-fact-panel">
                <div className="details-facts-head">More information</div>
                <dl className="details-facts-list">
                  {facts.map((f) => (
                    <div key={f.label} className="fact-row">
                      <dt>{f.label}</dt>
                      <dd>{f.value}</dd>
                    </div>
                  ))}
                </dl>
              </section>
            )}
            {!!meta?.tags?.length && (
              <section className="details-fact-panel">
                <div className="details-facts-head">Tags</div>
                <div className="details-tags details-facts-tags">
                  {meta.tags.map((t) => (
                    <span key={t} className="details-tag">
                      {t}
                    </span>
                  ))}
                </div>
              </section>
            )}
          </div>
        )}

        {!!meta?.cast?.length && (
          <section className="details-cast">
            <div className="shots-head">Cast &amp; creators</div>
            <div className="cast-strip">{meta.cast.map((name) => <span key={name} className="cast-chip">{name}</span>)}</div>
          </section>
        )}

        {!!meta?.screenshots?.length && (
          <div className="shots">
            <div className={`shots-head ${focusZone === 'shot' ? 'focused' : ''}`}>
              Screenshots
            </div>
            <div className="shots-strip">
              {meta.screenshots.map((src, i) => (
                <img
                  key={src}
                  className={`shot ${focusedKey === `shot:${i}` ? 'focused' : ''}`}
                  src={src}
                  alt=""
                  loading="lazy"
                  {...nav(`shot:${i}`)}
                />
              ))}
            </div>
          </div>
        )}

        {achAll.length > 0 && extras.achievements && (
          <div className="shots">
            <div className={`shots-head ${focusZone === 'ach' ? 'focused' : ''}`}>
              Achievements
              <span className="search-section-count">
                {extras.achievements.unlocked.length} of {achAll.length} earned
              </span>
            </div>
            <div className="shots-strip">
              {achAll.map((a, i) => (
                <div
                  key={`${a.name}-${i}`}
                  className={`ach-tile ${a.unlocked_at == null ? 'ach-locked' : ''} ${
                    focusedKey === `ach:${i}` ? 'focused' : ''
                  }`}
                  {...nav(`ach:${i}`)}
                >
                  {a.icon && <img src={a.icon} alt="" loading="lazy" />}
                  <div className="ach-text">
                    <div className="ach-name">{a.name}</div>
                    {a.description && <div className="ach-desc">{a.description}</div>}
                    {a.unlocked_at != null && a.unlocked_at > 0 && (
                      <div className="ach-date">
                        {new Date(a.unlocked_at * 1000).toLocaleDateString()}
                      </div>
                    )}
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        {similar.length > 0 && (
          <div className="shots similar-block">
            <div className={`shots-head ${focusZone === 'sim' ? 'focused' : ''}`}>
              More like this <span className="details-hint">A to open</span>
            </div>
            <div className="shots-strip">
              {similar.map((s, i) => (
                <div
                  key={s.id}
                  className={`sim-card ${focusedKey === `sim:${i}` ? 'focused' : ''}`}
                  {...nav(`sim:${i}`)}
                  onClick={() => onOpen(s)}
                >
                  {s.art ? (
                    <img src={s.art} alt={s.title} loading="lazy" />
                  ) : (
                    <div className="card-placeholder">{s.title}</div>
                  )}
                  <div className="sim-card-title">{s.title}</div>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>

      <div className="details-legend">
        <span>
          <span className="key">↑↓←→</span> Move
        </span>
        <span>
          <span className="key">A</span> Select
        </span>
        <span>
          <span className="key">B</span> Back
        </span>
      </div>

      {status && <div className="toast">{status}</div>}
      {loading && (
        <LoadingOverlay
          label={loading.label}
          kind={loading.kind}
          art={meta?.background || item.art}
          poster={meta?.poster || item.art}
        />
      )}
    </div>
  );
}

type LoadingKind = 'stream' | 'app';

const sleep = (ms: number) => new Promise((resolve) => window.setTimeout(resolve, ms));

async function waitForPlayback(initial: PlaybackStatus): Promise<void> {
  let status = initial;
  const deadline = Date.now() + 90_000;
  while (status.state === 'starting' && Date.now() < deadline) {
    await sleep(700);
    status = await fetchPlayback(status.id);
  }
  if (status.state === 'started') return;
  if (status.state === 'failed') {
    throw new Error(status.message || 'Playback failed');
  }
  throw new Error('Playback is still starting. Check the player window or try another source.');
}

const LOADING_MESSAGES: Record<LoadingKind, string[]> = {
  stream: ['Finding the best source…', 'Connecting to peers…', 'Buffering — almost there…'],
  app: ['Launching…', 'Still starting — big games take a moment…', 'Almost there…'],
};

/** Full-screen "it's working, not frozen" overlay shown while a stream/app
 *  starts: the item's own artwork washed into the theme, its poster, and a
 *  quiet indeterminate bar with staged reassurance messages. */
function LoadingOverlay({
  label,
  kind,
  art,
  poster,
}: {
  label: string;
  kind: LoadingKind;
  art?: string;
  poster?: string;
}) {
  const [phase, setPhase] = useState(0);
  useEffect(() => {
    const t1 = window.setTimeout(() => setPhase(1), 4000);
    const t2 = window.setTimeout(() => setPhase(2), 9000);
    return () => {
      window.clearTimeout(t1);
      window.clearTimeout(t2);
    };
  }, []);
  return (
    <div className="loading-screen">
      {art && <div className="loading-art" style={{ backgroundImage: `url(${art})` }} />}
      <div className="loading-content">
        {poster && <img className="loading-poster" src={poster} alt="" />}
        <div className="loading-kicker">{kind === 'app' ? 'Launching' : 'Now playing'}</div>
        <div className="loading-title">{label}</div>
        <div className="loading-bar">
          <div className="loading-bar-fill" />
        </div>
        {/* key remounts the line so each new message fades in */}
        <div className="loading-msg" key={phase}>
          {LOADING_MESSAGES[kind][phase]}
        </div>
      </div>
    </div>
  );
}

function emptyMeta(item: ContentItem): Meta {
  return { id: item.id, kind: item.kind, title: item.title, genres: [], episodes: [] };
}

/** Pulls a couch-readable summary out of a stream's name/title: quality/size/
 *  seeder chips for the "best pick" card, plus a cleaned filename detail line.
 *  Addon labels (Torrentio &c.) pack this into name + emoji-laden title. */
function describeSource(s: Stream): { chips: string[]; detail: string } {
  const blob = `${s.name} ${s.title}`;
  const chips: string[] = [];
  const quality = blob.match(/\b(2160p|4k|1440p|1080p|720p|480p)\b/i)?.[1];
  if (quality) chips.push(quality.toUpperCase());
  const hdr = blob.match(/\b(HDR10\+?|HDR|Dolby\s?Vision|DV)\b/i)?.[1];
  if (hdr) chips.push(/dv|dolby/i.test(hdr) ? 'Dolby Vision' : hdr.toUpperCase());
  const size = blob.match(/([\d.]+\s?(?:GB|MB))/i)?.[1];
  if (size) chips.push(size.replace(/\s+/g, ' '));
  const seeders = s.title.match(/(?:👤|seeders?[:\s])\s*([\d]+)/i)?.[1];
  if (seeders) chips.push(`${seeders} seeders`);
  // Detail line: the filename (first line of title) minus the stats line.
  const detail = (s.title.split('\n')[0] || s.name.split('\n').slice(1).join(' ')).trim();
  return { chips, detail };
}

function formatHours(hours: number): string {
  if (hours <= 0) return '—';
  if (hours < 10) return `${Math.round(hours * 2) / 2} h`;
  return `${Math.round(hours)} h`;
}

function formatTime(seconds: number): string {
  const s = Math.floor(seconds % 60);
  const m = Math.floor((seconds / 60) % 60);
  const h = Math.floor(seconds / 3600);
  const pad = (n: number) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}
