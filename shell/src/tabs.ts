// The Google-TV top-bar tabs. Tabs are a purely client-side view over the flat
// Row[] the daemon returns from /api/library: each content tab keeps the rows
// but narrows their items to the tab's kind(s), dropping rows that empty out.
//
// The set + order mirror the real Google TV home:
//   For you · Live · Movies · Shows · Library
// "For you" is the full, unfiltered recommendation home. Games and YouTube have
// no dedicated tab — they live under Library and Live the way they do on a real
// device.

import { ContentItem, Kind, Row } from './api';

export type TabId = 'foryou' | 'live' | 'movies' | 'shows' | 'creators' | 'games' | 'library';

export interface TabDef {
  id: TabId;
  label: string;
}

export const TABS: TabDef[] = [
  { id: 'foryou', label: 'For you' },
  { id: 'live', label: 'Live' },
  { id: 'movies', label: 'Movies' },
  { id: 'shows', label: 'Shows' },
  { id: 'creators', label: 'Creators' },
  { id: 'games', label: 'Games' },
  { id: 'library', label: 'Library' },
];

// Rows whose title marks them as "your stuff" — surfaced under Library
// alongside your games, the way Google TV's Library gathers what you're part-way
// through and what you own.
const LIBRARY_ROW = /^(continue|ready to|watchlist|my )/i;

// Which item kinds belong under each simple content tab. "Library" is
// special-cased in rowsForTab.
const TAB_KINDS: Record<'live' | 'movies' | 'shows' | 'games', Kind[]> = {
  live: ['live'],
  movies: ['movie'],
  shows: ['series'],
  games: ['game'],
};

/** The rows to show for a tab. "For you" is every row as-is. Movies/Shows
 *  narrow each row to the tab's kinds and drop rows that empty out. "Library"
 *  gathers your Continue/owned rows plus every game row. Item order is always
 *  preserved. */
export function rowsForTab(tab: TabId, rows: Row[]): Row[] {
  if (tab === 'foryou') return forYouRows(rows);
  if (tab === 'library') return libraryRows(rows);
  if (tab === 'creators') {
    const seen = new Set<string>();
    return rows.map((row) => {
      const items = row.items.filter((item) => {
        const creator = item.domain === 'youtube' || item.domain === 'twitch' || item.id.startsWith('yt:') || item.id.startsWith('twitch:');
        if (!creator || seen.has(item.id)) return false;
        seen.add(item.id);
        return true;
      });
      const channelOnly = items.length > 0 && items.every((item) => item.creator_type === 'channel');
      return { ...row, destination: 'creators' as const, purpose: 'creators' as const, layout: channelOnly ? 'circle' as const : 'landscape' as const, items };
    }).filter((row) => row.items.length > 0);
  }
  const kinds = TAB_KINDS[tab];
  const out: Row[] = [];
  for (const row of rows) {
    const items = row.items.filter((i: ContentItem) => kinds.includes(i.kind));
    if (items.length > 0) out.push({ ...row, items });
  }
  return out;
}

function forYouRows(rows: Row[]): Row[] {
  const priority = (row: Row) => {
    if (row.purpose === 'continue_watching') return 0;
    if (row.purpose === 'top_picks') return 1;
    if (row.purpose === 'because_you_watched') return 2;
    if (/^new episodes/i.test(row.title)) return 3;
    if (/^new & noteworthy/i.test(row.title)) return 4;
    return 5;
  };
  const seen = new Set<string>();
  return rows
    .filter((row) => row.purpose !== 'creators' && row.purpose !== 'games' && !row.title.match(/indian spotlight/i))
    .sort((a, b) => priority(a) - priority(b))
    .map((row) => ({ ...row, items: row.items.filter((item) => {
      if (item.kind !== 'movie' && item.kind !== 'series') return false;
      if (seen.has(item.id)) return false;
      seen.add(item.id);
      return true;
    }) }))
    .filter((row) => row.items.length > 0)
    .slice(0, 8);
}

/** Library = things you're watching or own: Continue/Ready/Watchlist rows kept
 *  whole (mixed kinds), then every row narrowed to your games. */
function libraryRows(rows: Row[]): Row[] {
  const out: Row[] = [];
  for (const row of rows) {
    if (LIBRARY_ROW.test(row.title)) {
      out.push(row);
      continue;
    }
    const games = row.items.filter((i) => i.kind === 'game');
    if (games.length > 0) out.push({ title: row.title, items: games });
  }
  return out;
}

/** Does this tab have anything at all? Used to dim empty tabs (Google TV greys
 *  a tab with no content) and to keep focus from landing on a dead tab. */
export function tabHasContent(tab: TabId, rows: Row[]): boolean {
  if (tab === 'foryou') return rows.length > 0;
  return rowsForTab(tab, rows).length > 0;
}
