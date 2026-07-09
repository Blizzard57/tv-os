import { MutableRefObject, useCallback, useEffect, useRef, useState } from 'react';
import { ContentItem, Row } from './api';
import { colsFor, stateBadge } from './MediaRow';
import { NavAction } from './input';

interface Props {
  row: Row;
  onPick: (item: ContentItem) => void;
  onClose: () => void;
  /** App writes its forwarded nav handler here while the page is open. */
  actionRef: MutableRefObject<((a: NavAction) => void) | null>;
}

/** Full-section page ("Show all"): every item of one home row in a big
 *  wrapped grid, d-pad navigable, B/Esc returns to the home screen. */
export function SectionPage({ row, onPick, onClose, actionRef }: Props) {
  const [sel, setSel] = useState(0);
  const cols = colsFor(row);
  const wide = cols !== 8; // any non-poster column count is a wide-card row
  const selRef = useRef<HTMLDivElement | null>(null);

  const handle = useCallback(
    (action: NavAction) => {
      const len = row.items.length;
      switch (action) {
        case 'left':
          setSel((i) => Math.max(0, i - 1));
          break;
        case 'right':
          setSel((i) => Math.min(len - 1, i + 1));
          break;
        case 'up':
          setSel((i) => (i - cols >= 0 ? i - cols : i));
          break;
        case 'down':
          setSel((i) => (i + cols < len ? i + cols : Math.min(len - 1, i)));
          break;
        case 'confirm': {
          const item = row.items[sel];
          if (item) onPick(item);
          break;
        }
        case 'back':
          onClose();
          break;
        default:
          break;
      }
    },
    [row, sel, cols, onPick, onClose],
  );

  useEffect(() => {
    actionRef.current = handle;
    return () => {
      actionRef.current = null;
    };
  }, [actionRef, handle]);

  useEffect(() => {
    selRef.current?.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }, [sel]);

  return (
    <div className="section-page">
      <div className="section-page-head">
        <h1>{row.title}</h1>
        <span className="search-section-count">{row.items.length}</span>
        <button className="btn section-page-back" onClick={onClose}>
          ← Back (B / Esc)
        </button>
      </div>
      <div className={`row-grid ${wide ? 'row-grid-wide' : ''} section-page-grid`}>
        {row.items.map((item, i) => (
          <div
            key={item.id}
            ref={i === sel ? selRef : undefined}
            className={`card ${wide ? 'card-wide' : ''} ${i === sel ? 'card-focused' : ''}`}
            onClick={() => onPick(item)}
          >
            {item.art ? (
              <img className="card-art" src={item.art} alt={item.title} loading="lazy" />
            ) : (
              <div className="card-placeholder">{item.title}</div>
            )}
            {(() => {
              const badge = stateBadge(item);
              return badge && <div className={`card-badge ${badge.cls}`}>{badge.label}</div>;
            })()}
            {wide && <div className="card-caption">{item.title}</div>}
          </div>
        ))}
      </div>
    </div>
  );
}
