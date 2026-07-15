import { useState } from 'react';
import { ContentItem, Meta } from './api';
import { gameLogo } from './cards';

interface Props {
  item: ContentItem | undefined;
  preview: Meta | null;
  explanation?: string;
  onOpen: (item: ContentItem) => void;
  onFocus: (el: HTMLElement) => void;
}

/** The spotlight at the top of every tab: the focused title's backdrop washed
 *  behind a left-anchored block of logo/title, meta line and synopsis — the
 *  Google-TV "featured" panel. Games show Steam's stylized logo the way movies
 *  show their title treatment. */
export function Hero({ item, preview, explanation, onOpen, onFocus }: Props) {
  const [failedLogos, setFailedLogos] = useState<Set<string>>(() => new Set());
  if (!item) return <div className="hero hero-empty" />;

  const isGame = (preview?.kind ?? item.kind) === 'game';
  const isSys = item.id.startsWith('sys:');
  const bg = preview?.background || item.art;
  // Prefer a real title treatment: TMDB logo for movies/shows, Steam logo.png
  // for games.
  const logoCandidate = preview?.logo || gameLogo(item);
  const logo = logoCandidate && !failedLogos.has(logoCandidate) ? logoCandidate : null;
  const sub = [
    preview?.release_info,
    preview?.runtime,
    preview?.rating && (isGame ? `Metacritic ${preview.rating}` : `★ ${preview.rating}`),
    preview?.genres?.slice(0, 3).join(', '),
  ]
    .filter(Boolean)
    .join('  ·  ');

  return (
    <div className="hero">
      {bg && <div key={bg} className="hero-bg" style={{ backgroundImage: `url(${bg})` }} />}
      <div className="hero-scrim" />
      <div className="hero-content">
        {!isSys && <div className="hero-kind">{explanation || `${(preview?.kind ?? item.kind).toUpperCase()} · Featured for you`}</div>}
        {logo && !isSys ? (
          <img
            key={logo}
            className="hero-logo"
            src={logo}
            alt={preview?.title || item.title}
            onError={() => setFailedLogos((failed) => new Set(failed).add(logo))}
          />
        ) : (
          <h1 className="hero-title">{preview?.title || item.title}</h1>
        )}
        {sub && <div className="hero-sub">{sub}</div>}
        {preview?.description && <p className="hero-desc">{preview.description}</p>}
        <div className="hero-actions">
          <button className="hero-primary" onClick={() => onOpen(item)} onFocus={(e) => onFocus(e.currentTarget)}>
            <span className="material-icon" aria-hidden>▶</span>
            {item.action === 'install' ? 'Install' : item.action === 'none' ? 'View details' : 'Watch now'}
          </button>
          <button className="hero-secondary" onClick={() => onOpen(item)} onFocus={(e) => onFocus(e.currentTarget)}>
            <span className="material-icon" aria-hidden>ⓘ</span> Details
          </button>
        </div>
      </div>
    </div>
  );
}
