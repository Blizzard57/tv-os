import { useEffect, useRef, useState } from 'react';
import { ContentItem, InstallJob } from './api';
import { cardSubtitle, landscapeArtSources, stateBadge } from './cards';

interface Props {
  title: string;
  items: ContentItem[];
  /** Focused card index within this shelf, or null when the shelf is inactive. */
  focused: number | null;
  jobs: InstallJob[];
  onPick: (item: ContentItem) => void;
}

/** One home shelf: a title and a horizontally-scrolling strip of Google-TV
 *  "Standard" cards — a wide 16:9 thumbnail with the title and a short subtitle
 *  below it. Every content row uses the same landscape card so the home reads as
 *  one consistent grid (see cards.ts). */
export function Shelf({ title, items, focused, jobs, onPick }: Props) {
  const stripRef = useRef<HTMLDivElement>(null);
  const active = focused !== null;

  useEffect(() => {
    if (focused === null) return;
    const card = stripRef.current?.children[focused] as HTMLElement | undefined;
    card?.scrollIntoView({ behavior: 'smooth', block: 'nearest', inline: 'center' });
  }, [focused, items.length]);

  return (
    <section className={`shelf ${active ? 'shelf-active' : ''}`}>
      <h2 className="shelf-title">{title}</h2>
      <div ref={stripRef} className="shelf-strip">
        {items.map((item, i) => (
          <Card
            key={item.id}
            item={item}
            focused={active && i === focused}
            job={jobs.find((j) => j.id === item.id && j.status === 'running')}
            onClick={() => onPick(item)}
          />
        ))}
      </div>
    </section>
  );
}

function Card({
  item,
  focused,
  job,
  onClick,
}: {
  item: ContentItem;
  focused: boolean;
  job?: InstallJob;
  onClick: () => void;
}) {
  const [artStep, setArtStep] = useState(0);
  const sources = landscapeArtSources(item);
  const src = sources[artStep];
  const badge = stateBadge(item);
  const subtitle = cardSubtitle(item);

  return (
    <div className={`card ${focused ? 'card-focused' : ''}`} onClick={onClick}>
      <div className="card-thumb">
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
