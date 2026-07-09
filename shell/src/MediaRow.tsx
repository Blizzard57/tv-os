import { useEffect, useRef, useState } from 'react';
import { ContentItem, InstallJob, Row } from './api';

/** A section shows at most this many wrapped lines before "Show all". */
export const LINES = 2;

/** Cards per line: wide 16:9 thumbs for all-video rows (YouTube), posters
 *  otherwise. Five per line makes a video card's width land on the poster
 *  cards' height, so mixed sections share one visual rhythm. */
export const colsFor = (row: Row): number =>
  row.items.length > 0 && row.items.every((i) => i.kind === 'video') ? 5 : 8;

/** Items rendered before the "Show all" tile takes the last cell. */
export const shownCount = (row: Row): number => {
  const max = colsFor(row) * LINES;
  return row.items.length > max ? max - 1 : row.items.length;
};

/** D-pad positions in the section: shown items + the expand tile if any. */
export const navLength = (row: Row): number =>
  shownCount(row) + (row.items.length > shownCount(row) ? 1 : 0);

interface Props {
  row: Row;
  /** Focused position within the section (may be the expand tile), or null. */
  focusedCol: number | null;
  jobs: InstallJob[];
  /** Opens the full-section page ("Show all"). */
  onExpand: () => void;
  onPick: (item: ContentItem) => void;
}

/** One home section: items wrap into up to three lines (nothing scrolls
 *  sideways); overflow lives behind a "Show all N" tile. */
export function MediaRow({ row, focusedCol, jobs, onExpand, onPick }: Props) {
  const active = focusedCol !== null;
  const ref = useRef<HTMLElement>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  const cols = colsFor(row);
  const shown = shownCount(row);
  const hasMore = row.items.length > shown;
  const wide = cols !== 8; // any non-poster column count is a wide-card row

  // Scroll ONLY the rows container. Always anchor the whole section (title
  // first — never a half-cropped line of the previous artwork above); only
  // when the section genuinely doesn't fit does the view slide just enough
  // to keep the focused card fully visible.
  useEffect(() => {
    if (!active || focusedCol === null) return;
    const section = ref.current;
    const box = section?.parentElement; // .rows — the one scrollable area
    if (!section || !box) return;
    const sectionTop = Math.max(0, section.offsetTop - 12);
    const card = gridRef.current?.children[focusedCol] as HTMLElement | undefined;
    let target = sectionTop;
    if (card) {
      const needed = card.offsetTop + card.offsetHeight - box.clientHeight + 20;
      target = Math.max(sectionTop, needed);
    }
    box.scrollTo({ top: target, behavior: 'smooth' });
  }, [active, focusedCol, cols]);

  return (
    <section ref={ref} className={`row ${active ? 'row-active' : ''}`}>
      <h2 className="row-title">
        {row.title}
        {hasMore && <span className="row-count">{row.items.length}</span>}
      </h2>
      <div ref={gridRef} className={`row-grid ${wide ? 'row-grid-wide' : ''}`}>
        {row.items.slice(0, shown).map((item, i) => (
          <Card
            key={item.id}
            item={item}
            wide={wide}
            focused={active && i === focusedCol}
            job={jobs.find((j) => j.id === item.id && j.status === 'running')}
            onClick={() => onPick(item)}
          />
        ))}
        {hasMore && (
          <div
            className={`card ${wide ? 'card-wide' : ''} expand-card ${active && focusedCol === shown ? 'card-focused' : ''}`}
            onClick={onExpand}
          >
            <div className="expand-count">+{row.items.length - shown}</div>
            <div className="expand-label">Show all {row.items.length}</div>
          </div>
        )}
      </div>
    </section>
  );
}

/** The state a game card wears: installed, owned-but-not-installed, or a
 *  to-buy recommendation. Non-games only badge installability. */
export function stateBadge(item: ContentItem): { label: string; cls: string } | null {
  if (item.kind === 'game') {
    if (item.action === 'play') return { label: 'INSTALLED', cls: 'card-badge-progress' };
    if (item.action === 'install') return { label: 'OWNED', cls: '' };
    return { label: 'BUY', cls: 'card-badge-buy' };
  }
  if (item.action === 'install') return { label: 'INSTALL', cls: '' };
  return null;
}

/** Artwork candidates, best first. Steam games get a second chance: not
 *  every title has the portrait capsule, but header.jpg always exists —
 *  a wide banner beats an artless placeholder. */
function artSources(item: ContentItem): string[] {
  const sources = item.art ? [item.art] : [];
  const appid = item.id.match(/^(?:steam|gshop):(\d+)$/)?.[1];
  if (appid) {
    sources.push(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appid}/header.jpg`);
  }
  return sources;
}

function Card({
  item,
  wide,
  focused,
  job,
  onClick,
}: {
  item: ContentItem;
  wide: boolean;
  focused: boolean;
  job?: InstallJob;
  onClick: () => void;
}) {
  // Index into the artwork fallback chain; past the end = placeholder.
  const [artStep, setArtStep] = useState(0);
  const src = artSources(item)[artStep];
  // A lone video inside a poster row (e.g. Continue): letterbox its 16:9
  // thumbnail inside the poster-sized cell so the row stays aligned.
  const letterbox = !wide && item.kind === 'video';

  return (
    <div
      className={`card ${wide ? 'card-wide' : ''} ${letterbox ? 'card-letterbox' : ''} ${focused ? 'card-focused' : ''}`}
      onClick={onClick}
    >
      {src ? (
        <img
          className="card-art"
          src={src}
          alt={item.title}
          loading="lazy"
          onError={() => setArtStep((s) => s + 1)}
        />
      ) : (
        <div className="card-placeholder">{item.title}</div>
      )}
      {job ? (
        <div className="card-badge card-badge-progress">{Math.floor(job.progress)}%</div>
      ) : (
        (() => {
          const badge = stateBadge(item);
          return badge && <div className={`card-badge ${badge.cls}`}>{badge.label}</div>;
        })()
      )}
      {(wide || letterbox) && src && <div className="card-caption">{item.title}</div>}
    </div>
  );
}
