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

/** Artwork candidates, best first. Steam games get a second chance: not every
 *  title has the portrait capsule, but header.jpg always exists — a wide banner
 *  beats an artless placeholder. */
export function artSources(item: ContentItem): string[] {
  const sources = item.art ? [item.art] : [];
  const appid = item.id.match(/^(?:steam|gshop):(\d+)$/)?.[1];
  if (appid) {
    sources.push(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appid}/header.jpg`);
  }
  return sources;
}

/** A row is "wide" (16:9 landscape cards) when every item is a video — YouTube
 *  thumbnails are landscape. Everything else uses 2:3 posters. */
export const isWideRow = (items: ContentItem[]): boolean =>
  items.length > 0 && items.every((i) => i.kind === 'video');

// ---- Google-TV mixed card shapes: 16:9 landscape vs 2:3 portrait ----

export type ShelfLayout = 'landscape' | 'poster';

const CONTINUE = /continue/i;
export const APPS_SHELF_TITLE = 'Your apps & sources';

/** Which card shape a shelf uses, the way Google TV does: 16:9 landscape for
 *  Continue watching, all-video (YouTube), and the apps/sources shelf; 2:3
 *  portrait posters for Movies / Shows / Games browse rows. */
export function shelfLayout(title: string, items: ContentItem[]): ShelfLayout {
  if (isWideRow(items)) return 'landscape';
  if (CONTINUE.test(title)) return 'landscape';
  if (title === APPS_SHELF_TITLE) return 'landscape';
  return 'poster';
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
