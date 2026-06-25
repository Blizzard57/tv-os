import { useEffect, useRef, useState } from 'react';
import { ContentItem, InstallJob, Row } from './api';

// Horizontal strip translation: card width + gap, must match the CSS.
const CARD_STEP_PX = 196 + 18;

interface Props {
  row: Row;
  /** Column with focus, or null when this row is not the active row. */
  focusedCol: number | null;
  /** Column this row rests at while inactive (its remembered position). */
  restingCol: number;
  jobs: InstallJob[];
}

export function MediaRow({ row, focusedCol, restingCol, jobs }: Props) {
  const active = focusedCol !== null;
  const col = focusedCol ?? Math.min(restingCol, row.items.length - 1);
  const ref = useRef<HTMLElement>(null);

  useEffect(() => {
    if (active) ref.current?.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }, [active]);

  return (
    <section ref={ref} className={`row ${active ? 'row-active' : ''}`}>
      <h2 className="row-title">{row.title}</h2>
      <div className="row-strip" style={{ transform: `translateX(${-col * CARD_STEP_PX}px)` }}>
        {row.items.map((item, i) => (
          <Card
            key={item.id}
            item={item}
            focused={active && i === focusedCol}
            job={jobs.find((j) => j.id === item.id && j.status === 'running')}
          />
        ))}
      </div>
    </section>
  );
}

function Card({ item, focused, job }: { item: ContentItem; focused: boolean; job?: InstallJob }) {
  const [artFailed, setArtFailed] = useState(false);

  return (
    <div className={`card ${focused ? 'card-focused' : ''}`}>
      {item.art && !artFailed ? (
        <img
          className="card-art"
          src={item.art}
          alt={item.title}
          loading="lazy"
          onError={() => setArtFailed(true)}
        />
      ) : (
        <div className="card-placeholder">{item.title}</div>
      )}
      {job ? (
        <div className="card-badge card-badge-progress">{Math.floor(job.progress)}%</div>
      ) : (
        item.action === 'install' && <div className="card-badge">INSTALL</div>
      )}
    </div>
  );
}
