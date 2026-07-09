// The Google-TV top-bar tabs. Tabs are a purely client-side view over the flat
// Row[] the daemon returns from /api/library: each content tab keeps the rows
// but filters their items to the tab's kind(s), dropping rows that empty out.
// "For you" is the full, unfiltered home.

import { ContentItem, Kind, Row } from './api';

export type TabId = 'foryou' | 'movies' | 'shows' | 'games' | 'apps';

export interface TabDef {
  id: TabId;
  label: string;
}

export const TABS: TabDef[] = [
  { id: 'foryou', label: 'For you' },
  { id: 'movies', label: 'Movies' },
  { id: 'shows', label: 'Shows' },
  { id: 'games', label: 'Games' },
  { id: 'apps', label: 'Apps' },
];

// Which item kinds belong under each content tab. "Apps" collects videos
// (YouTube) here; AppsTab adds a synthesized apps/sources shelf on top.
const TAB_KINDS: Record<Exclude<TabId, 'foryou'>, Kind[]> = {
  movies: ['movie'],
  shows: ['series'],
  games: ['game'],
  apps: ['video'],
};

/** The rows to show for a tab. For "For you" that's every row as-is; for a
 *  content tab it's each row with its items narrowed to the tab's kinds, and
 *  rows that end up empty removed. Item order within a row is preserved. */
export function rowsForTab(tab: TabId, rows: Row[]): Row[] {
  if (tab === 'foryou') return rows;
  const kinds = TAB_KINDS[tab];
  const out: Row[] = [];
  for (const row of rows) {
    const items = row.items.filter((i: ContentItem) => kinds.includes(i.kind));
    if (items.length > 0) out.push({ title: row.title, items });
  }
  return out;
}

/** Does this tab have anything at all? Used to grey-out empty tabs and to keep
 *  focus from landing on a dead tab. */
export function tabHasContent(tab: TabId, rows: Row[]): boolean {
  if (tab === 'apps') return true; // always has the apps/sources shelf
  return rowsForTab(tab, rows).length > 0;
}
