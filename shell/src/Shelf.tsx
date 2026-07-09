import { useEffect, useRef, useState } from 'react';
import { ContentItem, InstallJob } from './api';
import { ShelfLayout, artSources, landscapeArtSources, shelfLayout, stateBadge } from './cards';

interface Props {
  title: string;
  items: ContentItem[];
  /** Focused card index within this shelf, or null when the shelf is inactive. */
  focused: number | null;
  jobs: InstallJob[];
  onPick: (item: ContentItem) => void;
}

/** One home shelf: a title and a horizontally-scrolling strip of cards. Google
 *  TV mixes card shapes — 16:9 landscape for Continue/Apps/videos, 2:3 posters
 *  for Movies/Shows/Games — decided by `shelfLayout`. */
export function Shelf({ title, items, focused, jobs, onPick }: Props) {
  const layout = shelfLayout(title, items);
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
      <div ref={stripRef} className={`shelf-strip shelf-${layout}`}>
        {items.map((item, i) => (
          <Card
            key={item.id}
            item={item}
            layout={layout}
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
  layout,
  focused,
  job,
  onClick,
}: {
  item: ContentItem;
  layout: ShelfLayout;
  focused: boolean;
  job?: InstallJob;
  onClick: () => void;
}) {
  const [artStep, setArtStep] = useState(0);
  const sources = layout === 'landscape' ? landscapeArtSources(item) : artSources(item);
  const src = sources[artStep];
  const badge = stateBadge(item);
  const landscape = layout === 'landscape';

  return (
    <div
      className={`card card-${layout} ${focused ? 'card-focused' : ''}`}
      onClick={onClick}
    >
      <div className="card-art-box">
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
        {/* Landscape cards carry a title/gradient the way Google-TV Continue
            cards do; posters keep their baked-in title art clean. */}
        {landscape && src && (
          <div className="card-overlay">
            <span className="card-overlay-title">{item.title}</span>
          </div>
        )}
        {job && (
          <div className="card-progress">
            <div className="card-progress-fill" style={{ width: `${Math.max(4, job.progress)}%` }} />
          </div>
        )}
      </div>
    </div>
  );
}
