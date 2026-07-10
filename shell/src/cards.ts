// Shared card helpers used by every shelf/grid/strip that renders a
// ContentItem — kept in one place so the home, search, details and section
// views badge and source artwork identically.

import { ContentItem } from './api';

/** The state a game card wears: installed, owned-but-not-installed, or a
 *  to-buy recommendation. Non-games only badge installability. */
export function stateBadge(item: ContentItem): { label: string; cls: string } | null {
  if (item.kind === 'game') {
    if (item.action === 'play') return { label: 'INSTALLED', cls: 'badge-installed' };
    if (item.action === 'install') return { label: 'OWNED', cls: 'badge-owned' };
    return { label: 'BUY', cls: 'badge-buy' };
  }
  if (item.action === 'install') return { label: 'INSTALL', cls: 'badge-owned' };
  return null;
}

// Google TV shows content as wide 16:9 "Standard" cards (a thumbnail with the
// title below), not portrait posters — see the TV design guidelines. Every home
// row uses the same landscape tile: movie/show backdrops, Steam header banners
// and YouTube thumbnails are all 16:9, so the whole home reads as one consistent
// grid. `landscapeArtSources` supplies the wide artwork.

/** A short metadata line shown under a card label (kind, and for games their
 *  state). Kept intentionally terse — the card is small. */
export function cardSubtitle(item: ContentItem): string {
  if (item.kind === 'game') {
    if (item.action === 'play') return 'Installed';
    if (item.action === 'install') return 'In library';
    return 'Game';
  }
  return { movie: 'Movie', series: 'TV Show', video: 'Video' }[item.kind] ?? '';
}

const appidOf = (id: string) => id.match(/^(?:steam|gshop):(\d+)$/)?.[1];

/** Landscape (16:9) artwork candidates, best first. Steam games have a wide
 *  header banner; a movie/show only has its poster, which the card center-crops.
 */
export function landscapeArtSources(item: ContentItem): string[] {
  const appid = appidOf(item.id);
  const out: string[] = [];
  if (appid) out.push(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appid}/header.jpg`);
  if (item.art) out.push(item.art);
  return out;
}

/** Steam's stylized title treatment (logo.png) for a game, else null — used to
 *  show the game's real logo in the hero the way Google TV shows title art. */
export function gameLogo(item: ContentItem): string | null {
  const appid = appidOf(item.id);
  return appid ? `https://cdn.cloudflare.steamstatic.com/steam/apps/${appid}/logo.png` : null;
}
