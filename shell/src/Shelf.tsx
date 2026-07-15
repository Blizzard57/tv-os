import { useState } from 'react';
import { useEffect } from 'react';
import { ContentItem, InstallJob, Row, recordInteraction } from './api';
import { cardSubtitle, isLiveChannelLogo, landscapeArtSources, stateBadge, tileTint } from './cards';

interface Props {
  title: string;
  rowId?: string;
  layout?: Row['layout'];
  explanation?: string;
  items: ContentItem[];
  jobs: InstallJob[];
  onPick: (item: ContentItem) => void;
  /** Focus reached a card — App syncs the hero and remembers the spot. */
  onFocusItem: (item: ContentItem, el: HTMLElement) => void;
}

/** One home shelf: a title and a horizontally-scrolling strip of Google-TV
 *  "Standard" cards — a wide 16:9 thumbnail with the title and a short subtitle
 *  below it. Cards are real focus targets so spatial nav walks them by geometry
 *  (scroll-into-view and the `:focus` highlight follow real DOM focus). Every
 *  content row uses the same landscape card so the home reads as one consistent
 *  grid (see cards.ts). */
export function Shelf({ title, rowId, layout = 'landscape', explanation, items, jobs, onPick, onFocusItem }: Props) {
  useEffect(() => {
    for (const item of items.slice(0, 12)) {
      recordInteraction({ item_id: item.id, kind: 'impression', context: rowId || title }).catch(() => {});
    }
  }, [rowId, title, items]);
  return (
    <section className={`shelf shelf--${layout}`}>
      <div className="shelf-heading"><h2 className="shelf-title">{title}</h2>{explanation && <span>{explanation}</span>}</div>
      <div className="shelf-strip">
        {items.map((item) => (
          <Card
            key={item.id}
            item={item}
            job={jobs.find((j) => j.id === item.id && j.status === 'running')}
            onClick={() => onPick(item)}
            onFocus={(el) => onFocusItem(item, el)}
          />
        ))}
      </div>
    </section>
  );
}

function Card({
  item,
  job,
  onClick,
  onFocus,
}: {
  item: ContentItem;
  job?: InstallJob;
  onClick: () => void;
  onFocus: (el: HTMLElement) => void;
}) {
  const [artStep, setArtStep] = useState(0);
  const sources = landscapeArtSources(item);
  const src = sources[artStep];
  const badge = stateBadge(item);
  const subtitle = cardSubtitle(item);
  // Live tiles: contain the logo on a tinted tile (Google-TV look), and give
  // logo-less channels an intentional gradient tile instead of a blank box.
  const logoTile = isLiveChannelLogo(item);
  const tint = item.kind === 'live' ? tileTint(item.title) : undefined;

  return (
    <div
      className="card"
      tabIndex={0}
      onClick={onClick}
      onFocus={(e) => {
        onFocus(e.currentTarget);
        e.currentTarget.scrollIntoView({ behavior: 'smooth', block: 'nearest', inline: 'center' });
      }}
    >
      <div
        className={`card-thumb${logoTile ? ' card-thumb--logo' : ''}`}
        style={logoTile && src ? { background: tint } : undefined}
      >
        {src ? (
          <img
            className={`card-art${logoTile ? ' card-art--logo' : ''}`}
            src={src}
            alt={item.title}
            loading="lazy"
            onError={() => setArtStep((s) => s + 1)}
          />
        ) : (
          <div className="card-placeholder" style={tint ? { background: tint } : undefined}>
            {item.title}
          </div>
        )}
        {job ? (
          <div className="card-badge badge-installed">{Math.floor(job.progress)}%</div>
        ) : (
          badge && <div className={`card-badge ${badge.cls}`}>{badge.label}</div>
        )}
        {job && (
          <div className="card-progress">
            <div className="card-progress-fill" style={{ width: `${Math.max(4, job.progress)}%` }} />
          </div>
        )}
      </div>
      <div className="card-label">
        <div className="card-title">{item.title}</div>
        {subtitle && <div className="card-subtitle">{subtitle}</div>}
      </div>
    </div>
  );
}
