import { MutableRefObject, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  ContentItem,
  Episode,
  GameExtras,
  Meta,
  ResumeInfo,
  Stream,
  fetchGameExtras,
  fetchMeta,
  fetchResume,
  fetchSimilar,
  fetchStreams,
  launch,
  playStream,
  startInstall,
} from './api';
import { NavAction } from './input';

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

/// The horizontal strips below the main list, in navigation order.
type StripZone = 'shots' | 'ach' | 'similar';

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
  const [sel, setSel] = useState(0);
  const [status, setStatus] = useState<string | null>(null);
  const [loading, setLoading] = useState<{ label: string; kind: LoadingKind } | null>(null);
  // When non-null, focus is in a horizontal strip (screenshots / similar).
  const [strip, setStrip] = useState<{ zone: StripZone; idx: number } | null>(null);
  const [similar, setSimilar] = useState<ContentItem[]>([]);
  const [extras, setExtras] = useState<GameExtras>({});
  const [metaError, setMetaError] = useState(false);
  // Monotonic tokens so a late stream/meta response for a previous item can
  // never overwrite the current one.
  const streamToken = useRef(0);
  const metaToken = useRef(0);
  // Track pending status/flash timeouts so we can clear them on unmount.
  const flashTimer = useRef<number | null>(null);
  const shotRefs = useRef<(HTMLImageElement | null)[]>([]);
  const simRefs = useRef<(HTMLDivElement | null)[]>([]);
  const achRefs = useRef<(HTMLDivElement | null)[]>([]);
  // The selected entry of the active list, so navigation scrolls the page.
  const selRef = useRef<HTMLDivElement | null>(null);
  const [resume, setResume] = useState<ResumeInfo | null>(null);

  const series = (meta?.kind ?? item.kind) === 'series';
  const isGame = (meta?.kind ?? item.kind) === 'game';
  // A "Resume" entry leads the episodes/streams list when there's a saved spot.
  const resumeShown = !!resume && isStreamItem(item.id) && (stage === 'streams' || stage === 'episodes');
  const resumeOffset = resumeShown ? 1 : 0;
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
    setStrip(null);
    setMeta(null);
    setMetaError(false);
    setEpisode(null);
    setSel(0);
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
        } else if (isSeries) {
          const seasons = [...new Set(m.episodes.map((e) => e.season))].sort((a, b) => a - b);
          setSeason(seasons.find((s) => s >= 1) ?? seasons[0] ?? 1);
          setStage('episodes');
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

  const loadStreams = useCallback((id: string) => {
    const token = ++streamToken.current;
    setStreams(null);
    setSel(0);
    setStrip(null);
    setShowAllSources(false);
    fetchStreams(id)
      .then((s) => {
        if (token !== streamToken.current) return; // a newer load won
        setStreams(s);
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

  const playChosen = useCallback(
    (s: Stream) => {
      // External links just open elsewhere — no loading screen needed.
      if (s.kind === 'external') {
        flash(`Opening ${s.name.split('\n')[0]}…`);
        playStream(s, item, trackId)
          .then(onPlayed)
          .catch((e) => flash(`Could not open: ${e.message}`));
        return;
      }
      // The play request blocks until playback actually starts (or fails), so
      // show a loading screen meanwhile — never a frozen-looking blank.
      setLoading({ label: s.name.split('\n')[0] || meta?.title || item.title, kind: 'stream' });
      playStream(s, item, trackId)
        .then(() => {
          setLoading(null);
          onPlayed();
        })
        .catch((e) => {
          setLoading(null);
          flash(`Could not play: ${e.message}`);
        });
    },
    [item, onPlayed, flash, meta, trackId],
  );

  const runAction = useCallback(() => {
    if (item.action === 'install') {
      flash(`Downloading ${item.title}…`);
      startInstall(item.id).then(onPlayed).catch((e) => flash(`Could not install: ${e.message}`));
    } else {
      setLoading({ label: item.title, kind: 'app' });
      launch(item)
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
      if (loading) return; // ignore input while a stream is starting
      const shots = meta?.screenshots ?? [];
      const achAll = extras.achievements
        ? [...extras.achievements.unlocked, ...extras.achievements.locked]
        : [];
      // The strips below the list, top to bottom, skipping empty ones.
      const strips: StripZone[] = [
        ...(shots.length > 0 ? (['shots'] as const) : []),
        ...(achAll.length > 0 ? (['ach'] as const) : []),
        ...(similar.length > 0 ? (['similar'] as const) : []),
      ];

      // Focus is in a horizontal strip: ◀▶ browse, ↑↓ move between strips.
      if (strip) {
        const items =
          strip.zone === 'shots' ? shots : strip.zone === 'ach' ? achAll : similar;
        const at = strips.indexOf(strip.zone);
        switch (action) {
          case 'left':
            setStrip((s) => s && { ...s, idx: Math.max(0, s.idx - 1) });
            break;
          case 'right':
            setStrip((s) => s && { ...s, idx: Math.min(items.length - 1, s.idx + 1) });
            break;
          case 'up':
            if (at > 0) setStrip({ zone: strips[at - 1], idx: 0 });
            else setStrip(null); // back up into the main list
            break;
          case 'down':
            if (at < strips.length - 1) setStrip({ zone: strips[at + 1], idx: 0 });
            break;
          case 'confirm':
            if (strip.zone === 'similar') {
              const next = similar[strip.idx];
              if (next) onOpen(next);
            }
            break;
          case 'back':
            setStrip(null);
            break;
          default:
            break;
        }
        return;
      }

      // In the streams stage the navigable rows depend on whether the compact
      // "best pick + reveal" view or the full ranked list is showing.
      const streamRows = !streams
        ? 0
        : sourcesExpanded
          ? streams.length
          : Math.min(streams.length, 2); // best pick + "show all" toggle
      const base: unknown[] =
        stage === 'episodes'
          ? episodesInSeason
          : stage === 'streams'
            ? new Array(streamRows)
            : [0];
      // A "Resume" entry takes index 0 when we have somewhere to continue from.
      const offset = resumeShown ? 1 : 0;
      const navLen = base.length + offset;
      switch (action) {
        case 'down':
          // At the bottom of the list, drop into the strips if any exist.
          if (sel >= navLen - 1 && strips.length > 0) {
            setStrip({ zone: strips[0], idx: 0 });
          } else {
            setSel((i) => Math.min(navLen - 1, i + 1));
          }
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
          if (resumeShown && sel === 0) {
            if (resume) playChosen(resume.stream);
          } else if (stage === 'actions') {
            runAction();
          } else if (stage === 'episodes') {
            const ep = episodesInSeason[sel - offset];
            if (ep) openEpisode(ep);
          } else if (stage === 'streams') {
            const idx = sel - offset;
            if (sourcesExpanded) {
              const s = streams?.[idx];
              if (s) playChosen(s);
            } else if (idx === 0) {
              if (streams?.[0]) playChosen(streams[0]);
            } else if (idx === 1) {
              // Reveal the full ranked list, landing on the best pick.
              setShowAllSources(true);
              setSel(offset);
            }
          }
          break;
        case 'back':
          if (stage === 'streams' && showAllSources && canCompactSources) {
            // Collapse back to the compact best-pick view.
            setShowAllSources(false);
            setSel(offset);
          } else if (stage === 'streams' && series) {
            setStage('episodes');
            setSel(0);
          } else onClose();
          break;
        default:
          break;
      }
    },
    [stage, episodesInSeason, streams, seasons, season, sel, series, runAction, openEpisode, playChosen, onClose, onOpen, loading, strip, similar, extras, meta, resume, resumeShown, sourcesExpanded, showAllSources, canCompactSources],
  );

  // Navigation must always keep the selection on screen: the selected list
  // entry when walking the list, the focused card when in a strip.
  useEffect(() => {
    if (strip) return;
    selRef.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  }, [sel, stage, strip, streams, episode, episodesInSeason, showAllSources]);

  useEffect(() => {
    if (!strip) return;
    const el =
      strip.zone === 'shots'
        ? shotRefs.current[strip.idx]
        : strip.zone === 'ach'
          ? achRefs.current[strip.idx]
          : simRefs.current[strip.idx];
    el?.scrollIntoView({ behavior: 'smooth', inline: 'center', block: 'center' });
  }, [strip]);

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
            <div className="details-kind">{(meta?.kind ?? item.kind).toUpperCase()}</div>
            <h1 className="details-title">{title}</h1>
            <div className="details-sub">
              {[
                meta?.release_info,
                meta?.runtime,
                meta?.rating && (isGame ? `Metacritic ${meta.rating}` : `★ ${meta.rating}`),
                series && seasons.length > 0 && `${seasons.length} season${seasons.length > 1 ? 's' : ''}`,
              ]
                .filter(Boolean)
                .join('  ·  ')}
            </div>
            {!!meta?.genres?.length && (
              <div className="details-tags">
                {meta.genres.map((g) => (
                  <span key={g} className="details-tag">
                    {g}
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
          </div>

          {(() => {
            const facts: { label: string; value: string }[] = [
              meta?.rating && {
                label: isGame ? 'Metacritic' : 'Rating',
                value: isGame ? String(meta.rating) : `★ ${meta.rating}`,
              },
              meta?.release_info && { label: 'Released', value: meta.release_info },
              meta?.runtime && { label: 'Runtime', value: meta.runtime },
              series &&
                seasons.length > 0 && {
                  label: 'Seasons',
                  value: `${seasons.length} · ${meta?.episodes.length ?? 0} episodes`,
                },
              !!meta?.genres?.length && { label: 'Genres', value: meta!.genres.join(', ') },
              meta?.developer && { label: 'Developer', value: meta.developer },
              meta?.publisher &&
                meta.publisher !== meta.developer && { label: 'Publisher', value: meta.publisher },
            ].filter(Boolean) as { label: string; value: string }[];
            if (facts.length === 0 && !meta?.tags?.length) return null;
            return (
              <aside className="details-facts">
                <div className="details-facts-head">Details</div>
                <dl className="details-facts-list">
                  {facts.map((f) => (
                    <div key={f.label} className="fact-row">
                      <dt>{f.label}</dt>
                      <dd>{f.value}</dd>
                    </div>
                  ))}
                </dl>
                {!!meta?.tags?.length && (
                  <div className="details-tags details-facts-tags">
                    {meta.tags.map((t) => (
                      <span key={t} className="details-tag">
                        {t}
                      </span>
                    ))}
                  </div>
                )}
              </aside>
            );
          })()}
        </div>

        {stage === 'actions' && (
          <div className="details-actions">
            <div
              ref={!strip ? selRef : undefined}
              className={`row-item action-button ${!strip ? 'selected' : ''}`}
              onClick={runAction}
            >
              {item.action === 'install' ? 'Install' : 'Play'}
            </div>
          </div>
        )}

        {resumeShown && resume && (
          <div
            ref={sel === 0 && !strip ? selRef : undefined}
            className={`resume-btn ${sel === 0 && !strip ? 'selected' : ''}`}
            onClick={() => playChosen(resume.stream)}
          >
            ▶ Resume · {formatTime(resume.position)} · same source
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
                  ref={i === sel - resumeOffset && !strip ? selRef : undefined}
                  className={`ep-item ${i === sel - resumeOffset && !strip ? 'selected' : ''}`}
                  onClick={() => {
                    setSel(i + resumeOffset);
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
              {isShopItem(item.id)
                ? 'Where to buy — cheapest first'
                : episode
                  ? `${episode.season}×${String(episode.episode).padStart(2, '0')} — ${episode.title}`
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
                      ref={sel - resumeOffset === 0 && !strip ? selRef : undefined}
                      className={`best-source ${sel - resumeOffset === 0 && !strip ? 'selected' : ''}`}
                      onClick={() => {
                        setSel(resumeOffset);
                        playChosen(bestStream);
                      }}
                    >
                      <span className="best-play">▶</span>
                      <div className="best-text">
                        <div className="best-line">
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
                    ref={sel - resumeOffset === 1 && !strip ? selRef : undefined}
                    className={`source-toggle ${sel - resumeOffset === 1 && !strip ? 'selected' : ''}`}
                    onClick={() => {
                      setShowAllSources(true);
                      setSel(resumeOffset);
                    }}
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
                    ref={i === sel - resumeOffset && !strip ? selRef : undefined}
                    className={`stream-item ${i === sel - resumeOffset && !strip ? 'selected' : ''} ${
                      i === 0 && canCompactSources ? 'stream-best' : ''
                    }`}
                    onClick={() => {
                      setSel(i + resumeOffset);
                      playChosen(s);
                    }}
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

        {!!meta?.screenshots?.length && (
          <div className="shots">
            <div className={`shots-head ${strip?.zone === 'shots' ? 'focused' : ''}`}>
              Screenshots <span className="details-hint">↓ then ◀ ▶ to browse</span>
            </div>
            <div className="shots-strip">
              {meta.screenshots.map((src, i) => (
                <img
                  key={src}
                  ref={(el) => {
                    shotRefs.current[i] = el;
                  }}
                  className={`shot ${strip?.zone === 'shots' && i === strip.idx ? 'focused' : ''}`}
                  src={src}
                  alt=""
                  loading="lazy"
                  onClick={() => setStrip({ zone: 'shots', idx: i })}
                />
              ))}
            </div>
          </div>
        )}

        {extras.achievements &&
          extras.achievements.unlocked.length + extras.achievements.locked.length > 0 && (
            <div className="shots">
              <div className={`shots-head ${strip?.zone === 'ach' ? 'focused' : ''}`}>
                Achievements
                <span className="search-section-count">
                  {extras.achievements.unlocked.length} of{' '}
                  {extras.achievements.unlocked.length + extras.achievements.locked.length} earned
                </span>{' '}
                <span className="details-hint">↓ then ◀ ▶ to browse</span>
              </div>
              <div className="shots-strip">
                {[...extras.achievements.unlocked, ...extras.achievements.locked].map((a, i) => (
                  <div
                    key={`${a.name}-${i}`}
                    ref={(el) => {
                      achRefs.current[i] = el;
                    }}
                    className={`ach-tile ${a.unlocked_at == null ? 'ach-locked' : ''} ${
                      strip?.zone === 'ach' && i === strip.idx ? 'focused' : ''
                    }`}
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
            <div className={`shots-head ${strip?.zone === 'similar' ? 'focused' : ''}`}>
              More like this <span className="details-hint">↓ then ◀ ▶ · A to open</span>
            </div>
            <div className="shots-strip">
              {similar.map((s, i) => (
                <div
                  key={s.id}
                  ref={(el) => {
                    simRefs.current[i] = el;
                  }}
                  className={`sim-card ${
                    strip?.zone === 'similar' && i === strip.idx ? 'focused' : ''
                  }`}
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
          <span className="key">↑↓</span> Browse
        </span>
        <span>
          <span className="key">A</span> Select
        </span>
        <span>
          <span className="key">B</span> Back
        </span>
        {stage === 'episodes' && seasons.length > 1 && (
          <span>
            <span className="key">◀▶</span> Season
          </span>
        )}
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
