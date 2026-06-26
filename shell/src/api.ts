// Types and calls for the tvosd JSON API. Mirrors tvosd/src/model.rs and
// tvosd/src/install.rs.

export type Kind = 'game' | 'video' | 'movie' | 'series';

// What pressing A/Enter on the item does — decided by the daemon.
export type Action = 'play' | 'install' | 'none';

export interface ContentItem {
  id: string;
  kind: Kind;
  title: string;
  art?: string;
  action: Action;
}

export interface Row {
  title: string;
  items: ContentItem[];
}

// Details-page metadata — mirrors tvosd/src/media.rs.
export interface Episode {
  id: string;
  title: string;
  season: number;
  episode: number;
  overview?: string;
  thumbnail?: string;
  released?: string;
}

export interface Meta {
  id: string;
  kind: string; // movie | series | game
  title: string;
  poster?: string;
  background?: string;
  logo?: string;
  description?: string;
  release_info?: string;
  rating?: string;
  runtime?: string;
  genres: string[];
  episodes: Episode[];
}

export type StreamKind = 'direct' | 'youtube' | 'external' | 'torrent';

export interface Stream {
  kind: StreamKind;
  url: string;
  name: string;
  title: string;
  file_idx?: number;
}

export interface InstallJob {
  id: string;
  title: string;
  status: 'running' | 'done' | 'failed';
  progress: number; // 0–100
  detail: string;
}

// Video upscaling preference — mirrors tvosd/src/settings.rs.
export type EnhanceMode = 'auto' | 'quality' | 'performance' | 'off';

// Mirrors tvosd/src/settings.rs (snake_case to match the wire format).
export interface Settings {
  enhance: EnhanceMode;
  steam_api_key: string;
  steam_id: string;
  tmdb_key: string;
}

export interface SteamStatus {
  connected: boolean;
  count?: number;
  error?: string;
}

// A Stremio-compatible addon, as returned by /api/addons.
export interface Addon {
  url: string;
  base: string;
  name: string;
  catalogs: { type: string; id: string; name: string }[];
  streams: boolean;
  meta: boolean;
  configure_url?: string;
}

export async function fetchSettings(): Promise<Settings> {
  const res = await fetch('/api/settings');
  if (!res.ok) throw new Error(`settings request failed: ${res.status}`);
  return res.json();
}

export async function saveSettings(settings: Settings): Promise<void> {
  const res = await fetch('/api/settings', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(settings),
  });
  if (!res.ok) throw new Error(await res.text());
}

export async function fetchLibrary(): Promise<Row[]> {
  const res = await fetch('/api/library');
  if (!res.ok) throw new Error(`library request failed: ${res.status}`);
  return res.json();
}

export async function fetchInstalls(): Promise<InstallJob[]> {
  const res = await fetch('/api/installs');
  if (!res.ok) throw new Error(`installs request failed: ${res.status}`);
  return res.json();
}

async function post(path: string, body: unknown): Promise<void> {
  const res = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(await res.text());
}

// Launch sends the whole item so the daemon can record it for the
// recommender's Continue / Recommended rows.
export const launch = (item: ContentItem) =>
  post('/api/launch', { id: item.id, title: item.title, kind: item.kind, art: item.art });
export const startInstall = (id: string) => post('/api/install', { id });

export async function fetchSteamStatus(): Promise<SteamStatus> {
  const res = await fetch('/api/steam/status');
  if (!res.ok) throw new Error(`steam status failed: ${res.status}`);
  return res.json();
}

export async function fetchAddons(): Promise<Addon[]> {
  const res = await fetch('/api/addons');
  if (!res.ok) throw new Error(`addons request failed: ${res.status}`);
  return res.json();
}

export const addAddon = (url: string) => post('/api/addons', { url });
export const removeAddon = (url: string) => post('/api/addons/remove', { url });
export const openUrl = (url: string) => post('/api/open', { url });

export async function fetchMeta(id: string): Promise<Meta> {
  const res = await fetch(`/api/meta?id=${encodeURIComponent(id)}`);
  if (!res.ok) throw new Error(`meta request failed: ${res.status}`);
  return res.json();
}

export async function fetchStreams(id: string): Promise<Stream[]> {
  const res = await fetch(`/api/streams?id=${encodeURIComponent(id)}`);
  if (!res.ok) throw new Error(`streams request failed: ${res.status}`);
  return res.json();
}

// Plays a chosen stream; sends the item so the daemon records it for the
// recommender (with the title/art shown on the details page).
export const playStream = (stream: Stream, item: ContentItem) =>
  post('/api/play', {
    stream,
    item: { id: item.id, title: item.title, kind: item.kind, art: item.art },
  });
