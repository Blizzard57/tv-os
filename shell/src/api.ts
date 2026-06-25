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

export interface InstallJob {
  id: string;
  title: string;
  status: 'running' | 'done' | 'failed';
  progress: number; // 0–100
  detail: string;
}

// Video upscaling preference — mirrors tvosd/src/settings.rs.
export type EnhanceMode = 'auto' | 'quality' | 'performance' | 'off';

export interface Settings {
  enhance: EnhanceMode;
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
