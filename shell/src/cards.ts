// Shared card helpers used by every shelf/grid/strip that renders a
// ContentItem — kept in one place so the home, search, details and section
// views badge and source artwork identically.

import { ContentItem } from './api';

/** A live *channel* card (IPTV) shows a logo, which must be contained on a tile
 *  (never cover-cropped) — that's the Google-TV live look. YouTube-live and
 *  schedule cards carry real 16:9 thumbnails and use cover like everything else. */
export function isLiveChannelLogo(item: ContentItem): boolean {
  return item.kind === 'live' && item.id.startsWith('live:iptv:');
}

/** A deterministic, tasteful dark gradient from a seed (channel/event name), so
 *  every live tile has a gentle unique tint behind its logo — and a logo-less
 *  channel still gets an intentional-looking tile instead of a blank box. */
export function tileTint(seed: string): string {
  let h = 0;
  for (let i = 0; i < seed.length; i++) h = (Math.imul(h, 31) + seed.charCodeAt(i)) >>> 0;
  const hue = h % 360;
  return `linear-gradient(150deg, hsl(${hue} 34% 18%), hsl(${(hue + 42) % 360} 30% 9%))`;
}

/** A schedule fixture card encodes its state + start time in the id:
 *  `live:sched:<state>:<epoch>:<eventId>` (state = "live" | "up"). */
function schedInfo(item: ContentItem): { state: string; epoch: number } | null {
  if (!item.id.startsWith('live:sched:')) return null;
  const parts = item.id.split(':');
  return { state: parts[2] ?? 'up', epoch: Number(parts[3]) || 0 };
}

/** Human "Starts in 2h" from a unix-second start time (recomputed each render,
 *  so it stays fresh as kickoff approaches). */
function relativeStart(epoch: number): string {
  if (!epoch) return 'Upcoming';
  const mins = Math.round((epoch * 1000 - Date.now()) / 60000);
  if (mins <= 0) return 'Starting soon';
  if (mins < 60) return `Starts in ${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `Starts in ${hours}h`;
  return `Starts in ${Math.floor(hours / 24)}d`;
}

/** The state a game card wears: installed, owned-but-not-installed, or a
 *  to-buy recommendation. Non-games only badge installability. */
export function stateBadge(item: ContentItem): { label: string; cls: string } | null {
  if (item.kind === 'game') {
    if (item.action === 'play') return { label: 'INSTALLED', cls: 'badge-installed' };
    if (item.action === 'install') return { label: 'OWNED', cls: 'badge-owned' };
    return { label: 'BUY', cls: 'badge-buy' };
  }
  if (item.kind === 'live') {
    const sched = schedInfo(item);
    // A schedule fixture carries its own state; a stream is LIVE when playable.
    const live = sched ? sched.state === 'live' : item.action === 'play';
    return live
      ? { label: 'LIVE', cls: 'badge-live' }
      : { label: 'UPCOMING', cls: 'badge-owned' };
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
  // An explicit note (e.g. a fixture's carrier channel) always wins. For an
  // upcoming fixture, pair it with the live countdown: "On Sky Sports · in 2h".
  if (item.note) {
    const sched = schedInfo(item);
    if (sched && sched.state === 'up') return `${item.note} · ${relativeStart(sched.epoch)}`;
    return item.note;
  }
  if (item.kind === 'game') {
    if (item.action === 'play') return 'Installed';
    if (item.action === 'install') return 'In library';
    return 'Game';
  }
  if (item.kind === 'live') {
    const sched = schedInfo(item);
    if (sched) return sched.state === 'live' ? 'Live now' : relativeStart(sched.epoch);
    return item.action === 'play' ? 'Live now' : 'Upcoming';
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
