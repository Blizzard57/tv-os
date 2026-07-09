import { useEffect, useState } from 'react';
import { ContentItem, Row, fetchAddons, fetchSourceManifests } from './api';
import { APPS_SHELF_TITLE } from './cards';

// Synthetic-item id prefixes used only by the Apps tab's "apps & sources"
// shelf. App recognises these on confirm and routes them to Settings / an
// external URL instead of opening a details page.
export const SYS_SETTINGS = 'sys:settings';
export const SYS_OPEN = 'sys:open:';

const tile = (id: string, title: string): ContentItem => ({
  id,
  kind: 'movie', // neutral; these tiles carry no badge and open no details
  title,
  action: 'none',
});

/** Builds the "Your apps & sources" shelf for the Apps tab from installed
 *  Stremio addons and CloudStream source manifests, led by a Settings tile.
 *  Returns null until it has loaded so the tab doesn't flash an empty row. */
export function useAppsShelf(): Row | null {
  const [row, setRow] = useState<Row | null>(null);

  useEffect(() => {
    let cancelled = false;
    Promise.allSettled([fetchAddons(), fetchSourceManifests()]).then(([a, m]) => {
      if (cancelled) return;
      const items: ContentItem[] = [tile(SYS_SETTINGS, 'Settings')];
      if (a.status === 'fulfilled') {
        for (const addon of a.value) {
          if (addon.configure_url) items.push(tile(SYS_OPEN + addon.configure_url, addon.name));
        }
      }
      if (m.status === 'fulfilled') {
        for (const man of m.value) {
          if (man.source_url) items.push(tile(SYS_OPEN + man.source_url, man.name));
        }
      }
      setRow({ title: APPS_SHELF_TITLE, items });
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return row;
}
