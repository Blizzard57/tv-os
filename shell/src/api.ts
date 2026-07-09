// Types and calls for the tvosd JSON API. Mirrors tvosd/src/model.rs and
// tvosd/src/install.rs.

// ---- fetch wrapper: timeout + typed errors (+ retry for the library) ----

/** A network/HTTP failure with a code callers can branch on for friendly UI. */
export class ApiError extends Error {
  code: 'timeout' | 'offline' | 'http' | 'network';
  status?: number;
  constructor(message: string, code: ApiError['code'], status?: number) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
    this.status = status;
  }
}

const REQUEST_TIMEOUT_MS = 9000;

/** fetch with an AbortSignal.timeout() and typed errors. Callers get an
 *  {@link ApiError} on timeout/offline/network trouble instead of a raw
 *  DOMException, so the UI can show couch-friendly copy. */
async function apiFetch(input: string, init?: RequestInit): Promise<Response> {
  try {
    return await fetch(input, {
      ...init,
      signal: init?.signal ?? AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
  } catch (e) {
    if (typeof navigator !== 'undefined' && navigator.onLine === false) {
      throw new ApiError('You appear to be offline.', 'offline');
    }
    // AbortSignal.timeout() aborts with a TimeoutError.
    if (e instanceof DOMException && e.name === 'TimeoutError') {
      throw new ApiError('The request timed out.', 'timeout');
    }
    throw new ApiError(e instanceof Error ? e.message : String(e), 'network');
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

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
  /** Fullscreen output resolution "WxH" (e.g. "1920x1080"); "" = display native. */
  display_resolution: string;
  /** Ask gamescope to enable HDR output on capable displays. */
  display_hdr: boolean;
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
  /**
   * Secrets are write-only: `GET /api/settings` blanks them and instead
   * reports whether each is configured via these siblings, so the UI can show
   * a "configured" state without ever receiving the value. Saving an empty
   * secret leaves the stored value untouched.
   */
  steam_api_key_set?: boolean;
  tmdb_key_set?: boolean;
  trakt_client_secret_set?: boolean;
  trakt_token_set?: boolean;
  anilist_token_set?: boolean;
  mal_token_set?: boolean;
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
  const res = await apiFetch(`/api/game?id=${encodeURIComponent(id)}`);
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
  const res = await apiFetch('/api/tracking/status');
  if (!res.ok) throw new ApiError(`tracking status failed: ${res.status}`, 'http', res.status);
  return res.json();
}

/** Starts the Trakt device flow → show the code, the daemon polls. */
export async function traktConnect(): Promise<{ user_code: string; url: string }> {
  const res = await apiFetch('/api/trakt/connect', { method: 'POST' });
  if (!res.ok) throw new ApiError(await res.text(), 'http', res.status);
  return res.json();
}

export interface YouTubeStatus {
  connected: boolean;
  detail: string;
}

/** Whether the signed-in YouTube feeds are reachable (cookie check). */
export async function fetchYouTubeStatus(): Promise<YouTubeStatus> {
  const res = await apiFetch('/api/youtube/status');
  if (!res.ok) throw new ApiError(`youtube status failed: ${res.status}`, 'http', res.status);
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
  const res = await apiFetch('/api/version');
  if (!res.ok) throw new ApiError(`version request failed: ${res.status}`, 'http', res.status);
  return (await res.json()).version as string;
}

export async function fetchSettings(): Promise<Settings> {
  const res = await apiFetch('/api/settings');
  if (!res.ok) throw new ApiError(`settings request failed: ${res.status}`, 'http', res.status);
  return res.json();
}

// Accepts a partial payload: the panel omits secrets the user hasn't touched
// this session so a blanked-but-stored credential is never overwritten (every
// daemon-side field defaults, and an absent/empty secret means "unchanged").
export async function saveSettings(settings: Partial<Settings>): Promise<void> {
  const res = await apiFetch('/api/settings', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(settings),
  });
  if (!res.ok) throw new ApiError(await res.text(), 'http', res.status);
}

export async function fetchLibrary(): Promise<Row[]> {
  // The library is the home screen's lifeblood — retry a couple of times with
  // a short backoff so a slow-to-wake daemon doesn't hard-fail the boot.
  const attempts = 3;
  let lastErr: unknown;
  for (let i = 0; i < attempts; i++) {
    try {
      const res = await apiFetch('/api/library');
      if (!res.ok) throw new ApiError(`library request failed: ${res.status}`, 'http', res.status);
      return res.json();
    } catch (e) {
      lastErr = e;
      // Don't waste retries when the box is plainly offline.
      if (e instanceof ApiError && e.code === 'offline') break;
      if (i < attempts - 1) await sleep(500 * (i + 1));
    }
  }
  throw lastErr instanceof Error
    ? lastErr
    : new ApiError('library request failed', 'network');
}

export async function fetchInstalls(): Promise<InstallJob[]> {
  const res = await apiFetch('/api/installs');
  if (!res.ok) throw new ApiError(`installs request failed: ${res.status}`, 'http', res.status);
  return res.json();
}

async function post(path: string, body: unknown): Promise<void> {
  const res = await apiFetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new ApiError(await res.text(), 'http', res.status);
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
  const res = await apiFetch('/api/sources');
  if (!res.ok) throw new ApiError(`sources request failed: ${res.status}`, 'http', res.status);
  return res.json();
}

export async function fetchSteamStatus(): Promise<SteamStatus> {
  const res = await apiFetch('/api/steam/status');
  if (!res.ok) throw new ApiError(`steam status failed: ${res.status}`, 'http', res.status);
  return res.json();
}

export async function fetchAddons(): Promise<Addon[]> {
  const res = await apiFetch('/api/addons');
  if (!res.ok) throw new ApiError(`addons request failed: ${res.status}`, 'http', res.status);
  return res.json();
}

export const addAddon = (url: string) => post('/api/addons', { url });
export const removeAddon = (url: string) => post('/api/addons/remove', { url });
export const openUrl = (url: string) => post('/api/open', { url });

// One provider inside a source manifest, with its enable + last-probe state.
export interface ManifestSource {
  name: string;
  enabled: boolean;
  series: boolean;
  reachable?: boolean;
  latency_ms?: number;
}

// A CloudStream-style source manifest, as returned by /api/source-manifests.
export interface SourceManifest {
  id: string;
  name: string;
  source_url?: string;
  sources: ManifestSource[];
}

export async function fetchSourceManifests(): Promise<SourceManifest[]> {
  const res = await apiFetch('/api/source-manifests');
  if (!res.ok)
    throw new ApiError(`source manifests request failed: ${res.status}`, 'http', res.status);
  return res.json();
}

// `text` is a manifest URL or the manifest JSON pasted directly — the daemon
// auto-detects. Returns the installed manifest summary.
export async function addSourceManifest(text: string): Promise<SourceManifest> {
  const res = await apiFetch('/api/source-manifests', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text }),
  });
  if (!res.ok) throw new ApiError(await res.text(), 'http', res.status);
  return res.json();
}

export const removeSourceManifest = (id: string) =>
  post('/api/source-manifests/remove', { id });

export const toggleSource = (id: string, name: string, enabled: boolean) =>
  post('/api/source-manifests/toggle', { id, name, enabled });

// Probes each source for reachability, auto-disabling the unreachable ones.
// Returns the refreshed summaries (all manifests when `id` is omitted).
export async function testSourceManifests(id?: string): Promise<SourceManifest[]> {
  const res = await apiFetch('/api/source-manifests/test', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(id ? { id } : {}),
  });
  if (!res.ok) throw new ApiError(await res.text(), 'http', res.status);
  return res.json();
}

export async function fetchMeta(id: string, signal?: AbortSignal): Promise<Meta> {
  const res = await apiFetch(`/api/meta?id=${encodeURIComponent(id)}`, { signal });
  if (!res.ok) throw new ApiError(`meta request failed: ${res.status}`, 'http', res.status);
  return res.json();
}

export async function fetchStreams(id: string): Promise<Stream[]> {
  const res = await apiFetch(`/api/streams?id=${encodeURIComponent(id)}`);
  if (!res.ok) throw new ApiError(`streams request failed: ${res.status}`, 'http', res.status);
  return res.json();
}

export async function searchCatalog(query: string): Promise<ContentItem[]> {
  const res = await apiFetch(`/api/search?q=${encodeURIComponent(query)}`);
  if (!res.ok) throw new ApiError(`search failed: ${res.status}`, 'http', res.status);
  return res.json();
}

/** Deep search over the entire space — titles, actors, plot keywords, genre/
 *  region idioms ("k drama"), library, addons — as titled sections. */
export async function searchDeep(query: string): Promise<Row[]> {
  const res = await apiFetch(`/api/search/deep?q=${encodeURIComponent(query)}`);
  if (!res.ok) throw new ApiError(`deep search failed: ${res.status}`, 'http', res.status);
  return res.json();
}

/** "More like this" for a details page item (empty when nothing is known). */
export async function fetchSimilar(id: string): Promise<ContentItem[]> {
  const res = await apiFetch(`/api/similar?id=${encodeURIComponent(id)}`);
  if (!res.ok) return [];
  return res.json();
}

export interface ResumeInfo {
  stream: Stream;
  position: number; // seconds
}

/** The source + position to continue an item from, or null if none saved. */
export async function fetchResume(id: string): Promise<ResumeInfo | null> {
  const res = await apiFetch(`/api/resume?id=${encodeURIComponent(id)}`);
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
