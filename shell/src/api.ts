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
  developer?: string;
  publisher?: string;
  genres: string[];
  tags?: string[];
  screenshots?: string[];
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
  accent: string; // hex; "" means the default accent
  /** YouTube channels to follow (@handles / URLs, comma or space separated). */
  youtube_channels: string;
  /** Use the account signed in inside TV OS for For-you/Subscriptions rows. */
  youtube_account: boolean;
  /** Two-letter country code for game store pricing ("" = US). */
  game_region: string;
  /** Trakt API app credentials + saved OAuth token (device flow). */
  trakt_client_id: string;
  trakt_client_secret: string;
  trakt_token: string;
  /** AniList access token (implicit grant from your own API client). */
  anilist_token: string;
  /** MyAnimeList client id + token saved by the PKCE callback. */
  mal_client_id: string;
  mal_token: string;
}

// ---- Game page extras (playtime, HLTB, achievements) ----

export interface GameAchievement {
  name: string;
  description: string;
  icon: string;
  unlocked_at?: number;
}

export interface GameExtras {
  playtime_minutes?: number | null;
  hltb?: { main: number; main_extra: number; completionist: number } | null;
  achievements?: { unlocked: GameAchievement[]; locked: GameAchievement[] } | null;
}

export async function fetchGameExtras(id: string): Promise<GameExtras> {
  const res = await fetch(`/api/game?id=${encodeURIComponent(id)}`);
  if (!res.ok) return {};
  return res.json();
}

// ---- Watch tracking (Trakt / AniList / MAL) ----

export interface TrackingStatus {
  trakt: boolean;
  trakt_pending?: string | null;
  anilist: boolean;
  mal: boolean;
}

export async function fetchTrackingStatus(): Promise<TrackingStatus> {
  const res = await fetch('/api/tracking/status');
  if (!res.ok) throw new Error(`tracking status failed: ${res.status}`);
  return res.json();
}

/** Starts the Trakt device flow → show the code, the daemon polls. */
export async function traktConnect(): Promise<{ user_code: string; url: string }> {
  const res = await fetch('/api/trakt/connect', { method: 'POST' });
  if (!res.ok) throw new Error(await res.text());
  return res.json();
}

export interface YouTubeStatus {
  connected: boolean;
  detail: string;
}

/** Whether the signed-in YouTube feeds are reachable (cookie check). */
export async function fetchYouTubeStatus(): Promise<YouTubeStatus> {
  const res = await fetch('/api/youtube/status');
  if (!res.ok) throw new Error(`youtube status failed: ${res.status}`);
  return res.json();
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

/** Daemon version — the quick "am I on the latest build?" check. */
export async function fetchVersion(): Promise<string> {
  const res = await fetch('/api/version');
  if (!res.ok) throw new Error(`version request failed: ${res.status}`);
  return (await res.json()).version as string;
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

export interface SourceStatus {
  id: string;
  available: boolean;
}

export async function fetchSources(): Promise<SourceStatus[]> {
  const res = await fetch('/api/sources');
  if (!res.ok) throw new Error(`sources request failed: ${res.status}`);
  return res.json();
}

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

export async function searchCatalog(query: string): Promise<ContentItem[]> {
  const res = await fetch(`/api/search?q=${encodeURIComponent(query)}`);
  if (!res.ok) throw new Error(`search failed: ${res.status}`);
  return res.json();
}

/** Deep search over the entire space — titles, actors, plot keywords, genre/
 *  region idioms ("k drama"), library, addons — as titled sections. */
export async function searchDeep(query: string): Promise<Row[]> {
  const res = await fetch(`/api/search/deep?q=${encodeURIComponent(query)}`);
  if (!res.ok) throw new Error(`deep search failed: ${res.status}`);
  return res.json();
}

/** "More like this" for a details page item (empty when nothing is known). */
export async function fetchSimilar(id: string): Promise<ContentItem[]> {
  const res = await fetch(`/api/similar?id=${encodeURIComponent(id)}`);
  if (!res.ok) return [];
  return res.json();
}

export interface ResumeInfo {
  stream: Stream;
  position: number; // seconds
}

/** The source + position to continue an item from, or null if none saved. */
export async function fetchResume(id: string): Promise<ResumeInfo | null> {
  const res = await fetch(`/api/resume?id=${encodeURIComponent(id)}`);
  if (!res.ok) return null;
  return res.json();
}

// Plays a chosen stream; sends the item so the daemon records it for the
// recommender (with the title/art shown on the details page). `trackId` is the
// precise watched id — for an episode it carries season:episode so Trakt/
// AniList scrobble the exact episode, while `item` (the show) drives Continue.
export const playStream = (stream: Stream, item: ContentItem, trackId?: string) =>
  post('/api/play', {
    stream,
    item: { id: item.id, title: item.title, kind: item.kind, art: item.art },
    track_id: trackId ?? item.id,
  });
