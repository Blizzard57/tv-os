import { MutableRefObject, useCallback, useEffect, useRef, useState } from 'react';
import {
  Addon,
  EnhanceMode,
  Settings,
  SourceManifest,
  SourceStatus,
  addAddon,
  addSourceManifest,
  fetchAddons,
  fetchSettings,
  fetchSourceManifests,
  fetchSources,
  fetchSteamStatus,
  fetchTrackingStatus,
  fetchVersion,
  fetchYouTubeStatus,
  testSourceManifests,
  toggleSource,
  traktConnect,
  openUrl,
  removeAddon,
  removeSourceManifest,
  saveSettings,
} from './api';
import { activateFocused, moveFocus, stepSelect } from './focusNav';
import { NavAction } from './input';
import { OnScreenKeyboard } from './SearchOverlay';
import { ACCENT_PRESETS, DEFAULT_ACCENT, Theme, applyAccent } from './theme';

interface Props {
  onClose: () => void;
  /** Refetch settings + library after a change (updates the home screen). */
  reload: () => void;
  theme: Theme;
  onToggleTheme: () => void;
  /** App writes its forwarded nav handler here so a controller can drive
   *  the panel (d-pad moves focus, A activates). */
  actionRef: MutableRefObject<((a: NavAction) => void) | null>;
}

const ENHANCE_OPTIONS: EnhanceMode[] = ['auto', 'quality', 'performance', 'off'];

const BLANK: Settings = {
  enhance: 'auto',
  steam_api_key: '',
  steam_id: '',
  tmdb_key: '',
  accent: '',
  display_resolution: '',
  display_hdr: false,
  youtube_channels: '',
  youtube_account: false,
  game_region: '',
  trakt_client_id: '',
  trakt_client_secret: '',
  trakt_token: '',
  anilist_token: '',
  mal_client_id: '',
  mal_token: '',
};

/** Common store regions for game pricing (any ISO code works via Other). */
const GAME_REGIONS = [
  'US', 'CA', 'GB', 'DE', 'FR', 'ES', 'IT', 'NL', 'SE', 'PL',
  'BR', 'MX', 'AR', 'IN', 'JP', 'KR', 'AU', 'NZ', 'TR', 'ZA',
];

/** Secret fields the daemon returns BLANKED — never echoed back. We only send
 *  one on PUT when the user actually types a new value, so leaving it untouched
 *  keeps the stored secret. Each maps to a sibling `<field>_set` boolean the
 *  daemon may include; we fall back to "value is non-empty" when it's absent. */
const SECRET_FIELDS = [
  'steam_api_key',
  'tmdb_key',
  'trakt_client_secret',
  'trakt_token',
  'anilist_token',
  'mal_token',
] as const;
type SecretField = (typeof SECRET_FIELDS)[number];

/** Raw settings response may carry `<secret>_set` booleans alongside blanked
 *  secrets. Kept loose since the exact flag names are owned by the backend. */
type SettingsResponse = Settings & Record<string, unknown>;

const isSecret = (key: string): key is SecretField =>
  (SECRET_FIELDS as readonly string[]).includes(key);

type SettingsCategory = 'accounts' | 'games' | 'sources' | 'display' | 'appearance';

const SETTINGS_CATEGORIES: {
  id: SettingsCategory;
  label: string;
  detail: string;
  icon: string;
}[] = [
  { id: 'accounts', label: 'Accounts & services', detail: 'Steam, TMDB, YouTube, tracking', icon: 'person' },
  { id: 'games', label: 'Games & libraries', detail: 'Stores and price region', icon: 'apps' },
  { id: 'sources', label: 'Streaming sources', detail: 'Torrentio, WatchHub, CloudStream', icon: 'wifi' },
  { id: 'display', label: 'Display & sound', detail: 'Resolution and HDR', icon: 'tv' },
  { id: 'appearance', label: 'System', detail: 'Theme, accent, enhancement', icon: 'gear' },
];

export function SettingsPanel({
  onClose,
  reload,
  theme,
  onToggleTheme,
  actionRef,
}: Props) {
  const [form, setForm] = useState<Settings>(BLANK);
  const [addons, setAddons] = useState<Addon[]>([]);
  const [manifests, setManifests] = useState<SourceManifest[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState(false);
  const [version, setVersion] = useState<string | null>(null);
  const [activeCategory, setActiveCategory] = useState<SettingsCategory>('sources');
  // Which secret fields already have a stored value on the daemon (their value
  // arrives blanked). Drives the "configured/•••" display.
  const [configured, setConfigured] = useState<Record<string, boolean>>({});
  // Secret fields the user has actually typed into this session — only these
  // are sent on PUT, so an untouched secret is never overwritten with "".
  const touched = useRef<Set<string>>(new Set());
  // A field being edited via the on-screen keyboard (controller/remote).
  const [osk, setOsk] = useState<{
    label: string;
    value: string;
    masked: boolean;
    commit: (v: string) => void;
  } | null>(null);
  const oskAction = useRef<((a: NavAction) => void) | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  // The <select> currently in "edit mode": Enter opens it, Arrows then change
  // its value, Enter again commits. Held in a ref (not state) because the DOM
  // select is owned by a child section — we drive it directly.
  const editingSelect = useRef<HTMLSelectElement | null>(null);

  useEffect(() => {
    fetchVersion().then(setVersion).catch(() => {});
  }, []);

  useEffect(() => {
    // Only show the form once real settings arrive — editing (and saving) a
    // blank form when the daemon is unreachable would wipe saved keys.
    fetchSettings()
      .then((s) => {
        const resp = s as SettingsResponse;
        // A secret counts as configured if the daemon flags `<field>_set`, or
        // (fallback) if it echoed a non-empty value.
        const conf: Record<string, boolean> = {};
        for (const key of SECRET_FIELDS) {
          const flag = resp[`${key}_set`];
          conf[key] =
            typeof flag === 'boolean' ? flag : (s[key] ?? '').trim() !== '';
        }
        setConfigured(conf);
        setForm({ ...BLANK, ...s });
        setLoaded(true);
      })
      .catch(() => setLoadError(true));
    refreshAddons();
    refreshManifests();
  }, []);

  // Controller / arrow-key navigation. By default the d-pad moves focus
  // between fields and A activates — crucially, scrolling past a <select>
  // never changes its value. Pressing A on a select enters "edit mode", where
  // the d-pad changes the value and A commits; that's the only time a
  // selection changes.
  const stopEditing = useCallback(() => {
    const el = editingSelect.current;
    if (el) {
      el.classList.remove('select-editing');
      // Keep focus on the select so the next arrow moves to the neighbouring
      // field rather than jumping back to the top of the panel.
      editingSelect.current = null;
    }
  }, []);

  // Close on Escape regardless of which field has focus — unless a select is
  // being edited, in which case Escape just leaves edit mode.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (osk) return; // the OSK owns Escape while it's open
        e.stopPropagation();
        if (editingSelect.current) stopEditing();
        else onClose();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [onClose, stopEditing, osk]);

  const handleAction = useCallback(
    (action: NavAction) => {
      // The on-screen keyboard takes over all input while open.
      if (osk) {
        oskAction.current?.(action);
        return;
      }
      const panel = panelRef.current;
      if (!panel) return;
      const active = document.activeElement as HTMLElement | null;
      const editing = editingSelect.current;

      // A select being edited: arrows change the value, confirm commits.
      if (editing && active === editing) {
        if (action === 'confirm') stopEditing();
        else if (action === 'up' || action === 'left') stepSelect(editing, -1);
        else if (action === 'down' || action === 'right') stepSelect(editing, 1);
        return;
      }
      if (editing) stopEditing(); // focus moved off somehow — leave edit mode

      if (action === 'confirm') {
        if (active?.tagName === 'SELECT') {
          const select = active as HTMLSelectElement;
          select.classList.add('select-editing');
          editingSelect.current = select;
        } else {
          activateFocused();
        }
      } else if (action === 'up' || action === 'down' || action === 'left' || action === 'right') {
        moveFocus(panel, action);
      }
    },
    [stopEditing, osk],
  );

  useEffect(() => {
    actionRef.current = handleAction;
    return () => {
      actionRef.current = null;
    };
  }, [actionRef, handleAction]);

  // Land focus on the first control so the focus ring is visible immediately.
  // In the error branch there are no fields, so make sure the (always-present)
  // Close button gets focus — otherwise a controller user is stranded.
  useEffect(() => {
    if (loaded || loadError) moveFocus(panelRef.current!, 'down');
  }, [loaded, loadError]);

  const refreshAddons = () => fetchAddons().then(setAddons).catch(() => {});
  const refreshManifests = () => fetchSourceManifests().then(setManifests).catch(() => {});
  const update = (patch: Partial<Settings>) => {
    // Any secret in the patch was typed by the user → mark it for sending.
    for (const key of Object.keys(patch)) if (isSecret(key)) touched.current.add(key);
    setForm((f) => ({ ...f, ...patch }));
  };

  /** Save the form to the daemon, omitting any secret the user hasn't touched
   *  this session so blanked-but-stored secrets aren't wiped. `override` lets
   *  instant fields (accent/enhance) save the freshest value without a
   *  setState round-trip. */
  const submit = useCallback(
    async (override?: Partial<Settings>) => {
      const merged = { ...form, ...override };
      const payload: Partial<Settings> = { ...merged };
      for (const key of SECRET_FIELDS) {
        const typedHere = touched.current.has(key) || (override && key in override);
        // Don't send an untouched secret — leaving it out keeps the stored one.
        if (!typedHere) delete payload[key];
      }
      await saveSettings(payload);
    },
    [form],
  );

  /** Open the on-screen keyboard to edit a field with a controller/remote. */
  const openOsk = useCallback(
    (label: string, value: string, masked: boolean, commit: (v: string) => void) => {
      setOsk({ label, value, masked, commit });
    },
    [],
  );

  const activeInfo =
    SETTINGS_CATEGORIES.find((cat) => cat.id === activeCategory) ?? SETTINGS_CATEGORIES[0];
  const playableCloud = manifests.reduce(
    (sum, manifest) => sum + manifest.sources.filter((source) => source.playable).length,
    0,
  );
  const packageCloud = manifests.reduce(
    (sum, manifest) =>
      sum + manifest.sources.filter((source) => !source.playable && source.kind === 'cs3').length,
    0,
  );
  const streamAddons = addons.filter((addon) => addon.streams).length;

  return (
    <div className="settings-scrim" onClick={onClose}>
      <div className="settings" ref={panelRef} onClick={(e) => e.stopPropagation()}>
        {loadError ? (
          <div className="settings-empty-state">
            <h1>Settings</h1>
            <p>Could not load settings. tvosd is not answering.</p>
            <button className="btn btn-primary" onClick={onClose}>
              Close
            </button>
          </div>
        ) : !loaded ? (
          <div className="settings-empty-state">
            <h1>Settings</h1>
            <p>Loading…</p>
          </div>
        ) : (
          <div className="settings-tv-page">
            <aside className="settings-nav-pane">
              <div className="settings-brand">
                <div>
                  <span className="settings-kicker">Google TV</span>
                  <h1>Settings</h1>
                </div>
                <button className="settings-gear-button" onClick={onClose} aria-label="Close settings">
                  ×
                </button>
              </div>
              <nav className="settings-nav-list" aria-label="Settings categories">
                {SETTINGS_CATEGORIES.map((cat) => (
                  <button
                    key={cat.id}
                    className={`settings-nav-item ${activeCategory === cat.id ? 'settings-nav-active' : ''}`}
                    onClick={() => setActiveCategory(cat.id)}
                  >
                    <span className={`settings-nav-icon settings-nav-icon-${cat.icon}`} aria-hidden />
                    <span>
                      <span className="settings-nav-label">{cat.label}</span>
                      <span className="settings-nav-detail">{cat.detail}</span>
                    </span>
                  </button>
                ))}
              </nav>
              <div className="settings-version-bar">
                <span>TV OS</span>
                {version && <span>tvosd v{version}</span>}
              </div>
            </aside>

            <main className="settings-detail-pane">
              <div className="settings-detail-head">
                <span className="settings-kicker">All settings</span>
                <h2>{activeInfo.label}</h2>
              </div>
              <div className="settings-detail-scroll">
                {activeCategory === 'accounts' && (
                  <>
                    <SteamSection form={form} update={update} reload={reload} submit={submit} configured={configured} openOsk={openOsk} />
                    <TmdbSection form={form} update={update} reload={reload} submit={submit} configured={configured} openOsk={openOsk} />
                    <YouTubeSection form={form} update={update} reload={reload} submit={submit} configured={configured} openOsk={openOsk} />
                    <TrackingSection form={form} update={update} reload={reload} submit={submit} configured={configured} openOsk={openOsk} />
                  </>
                )}
                {activeCategory === 'games' && (
                  <GameLibrariesSection form={form} update={update} submit={submit} openOsk={openOsk} />
                )}
                {activeCategory === 'sources' && (
                  <>
                    <SourceHealthPanel
                      addons={addons}
                      manifests={manifests}
                      playableCloud={playableCloud}
                      packageCloud={packageCloud}
                      streamAddons={streamAddons}
                    />
                    <AddonSection addons={addons} refresh={refreshAddons} reload={reload} openOsk={openOsk} />
                    <SourceManifestSection manifests={manifests} refresh={refreshManifests} reload={reload} openOsk={openOsk} />
                  </>
                )}
                {activeCategory === 'display' && (
                  <DisplaySection form={form} update={update} submit={submit} />
                )}
                {activeCategory === 'appearance' && (
                  <AppearanceSection
                    form={form}
                    update={update}
                    reload={reload}
                    submit={submit}
                    configured={configured}
                    openOsk={openOsk}
                    theme={theme}
                    onToggleTheme={onToggleTheme}
                  />
                )}
              </div>
            </main>
          </div>
        )}
      </div>

      {osk && (
        <OnScreenKeyboard
          label={osk.label}
          initialValue={osk.value}
          masked={osk.masked}
          actionRef={oskAction}
          onCommit={(v) => {
            osk.commit(v);
            setOsk(null);
          }}
          onCancel={() => setOsk(null)}
        />
      )}
    </div>
  );
}

type SectionProps = {
  form: Settings;
  update: (patch: Partial<Settings>) => void;
  reload: () => void;
  /** Save the form, omitting untouched secrets (#6). */
  submit: (override?: Partial<Settings>) => Promise<void>;
  /** Which secret fields are already stored on the daemon (shown as •••). */
  configured: Record<string, boolean>;
  /** Open the on-screen keyboard to edit a field with a controller/remote. */
  openOsk: (label: string, value: string, masked: boolean, commit: (v: string) => void) => void;
};

function SteamSection({ form, update, reload, submit, configured, openOsk }: SectionProps) {
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const connect = async () => {
    setBusy(true);
    setStatus('Connecting…');
    try {
      await submit();
      const s = await fetchSteamStatus();
      setStatus(s.connected ? `Connected — ${s.count} games in your library` : (s.error ?? 'Failed'));
      reload();
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-section">
      <h2>Steam account</h2>
      <p className="settings-muted">
        Get a key at steamcommunity.com/dev/apikey, then enter your SteamID64 or
        profile name. Your profile's game details must be public.
      </p>
      <LockedField
        label="Web API key"
        masked
        value={form.steam_api_key}
        configured={configured.steam_api_key}
        openOsk={openOsk}
        onChange={(v) => update({ steam_api_key: v })}
        placeholder="0123456789ABCDEF…"
      />
      <LockedField
        label="SteamID or profile name"
        value={form.steam_id}
        openOsk={openOsk}
        onChange={(v) => update({ steam_id: v })}
        placeholder="76561197960287930 or gabelogannewell"
      />
      <div className="settings-actions">
        <button className="btn btn-primary" onClick={connect} disabled={busy}>
          Connect &amp; sync games
        </button>
        {status && <span className="settings-status">{status}</span>}
      </div>
    </section>
  );
}

function TmdbSection({ form, update, reload, submit, configured, openOsk }: SectionProps) {
  const [status, setStatus] = useState<string | null>(null);

  const save = async () => {
    try {
      await submit();
      setStatus(
        form.tmdb_key || configured.tmdb_key ? 'Saved — Movies & Shows rows enabled' : 'Saved',
      );
      reload();
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    }
  };

  return (
    <section className="settings-section">
      <h2>Movies &amp; TV (TMDB)</h2>
      <p className="settings-muted">
        A free TMDB API key fills the Trending Movies and Shows rows. Playback
        uses your installed stream addons.
      </p>
      <LockedField
        label="TMDB API key"
        masked
        value={form.tmdb_key}
        configured={configured.tmdb_key}
        openOsk={openOsk}
        onChange={(v) => update({ tmdb_key: v })}
        placeholder="TMDB API key (v3 auth)"
      />
      <div className="settings-actions">
        <button className="btn btn-primary" onClick={save}>
          Save
        </button>
        {status && <span className="settings-status">{status}</span>}
      </div>
    </section>
  );
}

function YouTubeSection({ form, update, reload, submit, openOsk }: SectionProps) {
  const [status, setStatus] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  const save = async () => {
    try {
      await submit();
      setStatus(
        form.youtube_account || form.youtube_channels.trim()
          ? 'Saved — YouTube rows will appear on Home'
          : 'Saved',
      );
      reload();
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    }
  };

  // Sign-in happens inside TV OS's own browser window — that profile's
  // cookies are what the daemon reads for the personal feeds. Route through the
  // daemon's open endpoint so it works in kiosk/gamescope (no _blank there).
  const signIn = () => {
    openUrl('https://www.youtube.com').catch(() => {});
    setStatus('Sign in in the window that opened, close it, then check the connection.');
  };

  const check = async () => {
    setChecking(true);
    setStatus('Checking…');
    try {
      const s = await fetchYouTubeStatus();
      setStatus(s.detail);
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    } finally {
      setChecking(false);
    }
  };

  return (
    <section className="settings-section">
      <h2>YouTube</h2>
      <p className="settings-muted">
        Follow channels by @handle or URL (comma or space separated) — each becomes a home row,
        and search includes YouTube results. Connecting your account adds your personal “For
        you” and “Subscriptions” rows: sign in once inside TV OS, no API key needed. Playback
        runs in the same player with upscaling.
      </p>
      <LockedField
        label="Channels"
        value={form.youtube_channels}
        openOsk={openOsk}
        onChange={(v) => update({ youtube_channels: v })}
        placeholder="@veritasium, @kurzgesagt, youtube.com/@mkbhd"
      />
      <Field label="Account">
        <div className="settings-actions">
          <button className="btn" onClick={signIn}>
            Sign in to YouTube
          </button>
          <label className="settings-check">
            <input
              type="checkbox"
              checked={form.youtube_account}
              onChange={(e) => update({ youtube_account: e.target.checked })}
            />
            Use my account for “For you” &amp; “Subscriptions” rows
          </label>
        </div>
      </Field>
      <div className="settings-actions">
        <button className="btn btn-primary" onClick={save}>
          Save
        </button>
        <button className="btn" onClick={check} disabled={checking}>
          Check connection
        </button>
        {status && <span className="settings-status">{status}</span>}
      </div>
    </section>
  );
}

/// Watch tracking: Trakt (movies & shows), AniList and MAL (anime). Finished
/// titles are pushed automatically once a service is connected.
function TrackingSection({ form, update, submit, configured, openOsk }: SectionProps) {
  const [status, setStatus] = useState<string | null>(null);
  const [connected, setConnected] = useState({ trakt: false, anilist: false, mal: false });
  // Trakt device-flow poll handles, cleared on unmount so we don't keep hitting
  // /api/tracking/status for 10 minutes after the panel closes (#7).
  const pollTimers = useRef<{ interval: number; timeout: number } | null>(null);

  const refreshStatus = () =>
    fetchTrackingStatus()
      .then((s) => setConnected({ trakt: s.trakt, anilist: s.anilist, mal: s.mal }))
      .catch(() => {});
  useEffect(() => {
    refreshStatus();
    return () => {
      if (pollTimers.current) {
        window.clearInterval(pollTimers.current.interval);
        window.clearTimeout(pollTimers.current.timeout);
      }
    };
  }, []);

  const save = async () => {
    try {
      await submit();
      setStatus('Saved');
      refreshStatus();
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    }
  };

  const connectTrakt = async () => {
    try {
      await submit();
      const { user_code, url } = await traktConnect();
      setStatus(`Go to ${url} and enter code ${user_code} — this panel updates when approved.`);
      if (pollTimers.current) {
        window.clearInterval(pollTimers.current.interval);
        window.clearTimeout(pollTimers.current.timeout);
      }
      const interval = window.setInterval(async () => {
        const s = await fetchTrackingStatus().catch(() => null);
        if (s?.trakt) {
          if (pollTimers.current) {
            window.clearInterval(pollTimers.current.interval);
            window.clearTimeout(pollTimers.current.timeout);
            pollTimers.current = null;
          }
          setStatus('Trakt connected ✓');
          refreshStatus();
        }
      }, 5000);
      const timeout = window.setTimeout(() => {
        window.clearInterval(interval);
        pollTimers.current = null;
      }, 600_000);
      pollTimers.current = { interval, timeout };
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    }
  };

  const connectMal = async () => {
    try {
      await submit();
      // Route through the daemon's open endpoint — kiosk/gamescope has no
      // window manager for _blank (#8). /api/open only accepts absolute
      // http(s) URLs, so resolve the daemon's own login-redirect endpoint
      // against our origin (the shell is served by tvosd) rather than passing
      // a relative path it would reject.
      await openUrl(new URL('/api/mal/login', window.location.origin).href).catch(() => {});
      setStatus('Approve in the window that opened, then reopen Settings.');
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    }
  };

  const state = (on: boolean) => (on ? '● Connected' : '○ Not connected');

  return (
    <section className="settings-section">
      <h2>Watch tracking</h2>
      <p className="settings-muted">
        Finished movies and episodes sync automatically (a title counts as watched when it plays
        to the end or you stop in the last 10%). Trakt tracks everything; AniList and MAL track
        anime episodes. Your Trakt history also personalizes the “For You” row, so recommendations
        follow what you watch anywhere — not just on this box.
      </p>
      <p className="settings-muted">
        Trakt {state(connected.trakt)} · AniList {state(connected.anilist)} · MAL{' '}
        {state(connected.mal)}
      </p>
      <LockedField
        label="Trakt client id (trakt.tv/oauth/applications)"
        value={form.trakt_client_id}
        openOsk={openOsk}
        onChange={(v) => update({ trakt_client_id: v })}
        placeholder="create an app on trakt.tv, paste its client id"
      />
      <LockedField
        label="Trakt client secret"
        masked
        value={form.trakt_client_secret}
        configured={configured.trakt_client_secret}
        openOsk={openOsk}
        onChange={(v) => update({ trakt_client_secret: v })}
      />
      <LockedField
        label="AniList access token"
        masked
        value={form.anilist_token}
        configured={configured.anilist_token}
        openOsk={openOsk}
        onChange={(v) => update({ anilist_token: v })}
        placeholder="from your AniList API client (implicit grant)"
      />
      <LockedField
        label="MyAnimeList client id (redirect URI: http://localhost:8484/api/mal/callback)"
        value={form.mal_client_id}
        openOsk={openOsk}
        onChange={(v) => update({ mal_client_id: v })}
      />
      <div className="settings-actions">
        <button className="btn btn-primary" onClick={save}>
          Save
        </button>
        <button className="btn" onClick={connectTrakt}>
          Connect Trakt
        </button>
        <button className="btn" onClick={connectMal}>
          Sign in to MAL
        </button>
        {status && <span className="settings-status">{status}</span>}
      </div>
    </section>
  );
}

function GameLibrariesSection({
  form,
  update,
  submit,
}: {
  form: Settings;
  update: (patch: Partial<Settings>) => void;
  submit: SectionProps['submit'];
  openOsk: SectionProps['openOsk'];
}) {
  const [sources, setSources] = useState<SourceStatus[]>([]);
  useEffect(() => {
    fetchSources().then(setSources).catch(() => {});
  }, []);
  const available = (id: string) => sources.some((s) => s.id === id && s.available);

  return (
    <section className="settings-section">
      <h2>Game libraries</h2>
      <p className="settings-muted">
        Connected stores show up in the “Games” and “Ready to Install” rows — view,
        install and launch right from the couch.
      </p>
      <GameRegionField form={form} update={update} submit={submit} />
      <div className="lib-list">
        <LibRow
          name="Steam"
          connected={available('steam')}
          hint="Add your Steam Web API key and ID above."
        />
        <LibRow
          name="Epic Games"
          connected={available('epic')}
          hint="Install legendary, then run “legendary auth” once to sign in."
          help="https://github.com/derrod/legendary#installation"
        />
        <LibRow
          name="GOG"
          connected={available('gog')}
          hint="Install Heroic Games Launcher and sign in to GOG; its library is picked up automatically."
          help="https://heroicgameslauncher.com"
        />
      </div>
    </section>
  );
}

/// Store region for game pricing ("Games for you" + Where-to-buy pages).
/// Driven by the shared form (#5) so it never clobbers unsaved edits; saves
/// immediately via the guarded submit so a change reprices on the next look.
function GameRegionField({
  form,
  update,
  submit,
}: {
  form: Settings;
  update: (patch: Partial<Settings>) => void;
  submit: SectionProps['submit'];
}) {
  const region = form.game_region?.trim().toUpperCase() || 'US';
  const options = GAME_REGIONS.includes(region) ? GAME_REGIONS : [region, ...GAME_REGIONS];

  const change = (next: string) => {
    update({ game_region: next });
    // Persist just this change through the guarded save (omits untouched
    // secrets, keeps the rest of the user's in-progress form intact).
    submit({ game_region: next }).catch(() => {
      /* daemon unreachable — the select still shows the choice */
    });
  };

  return (
    <Field label="Store region (game prices — Games for you & Where to buy)">
      <select value={region} onChange={(e) => change(e.target.value)}>
        {options.map((r) => (
          <option key={r} value={r}>
            {r}
          </option>
        ))}
      </select>
    </Field>
  );
}

function LibRow({
  name,
  connected,
  hint,
  help,
}: {
  name: string;
  connected: boolean;
  hint: string;
  help?: string;
}) {
  return (
    <div className="lib-row">
      <div>
        <span className="lib-name">{name}</span>
        <span className={`lib-status ${connected ? 'lib-on' : ''}`}>
          {connected ? '● Connected' : '○ Not connected'}
        </span>
        {!connected && <div className="settings-muted lib-hint">{hint}</div>}
      </div>
      {!connected && help && (
        <button className="btn" onClick={() => openUrl(help).catch(() => {})}>
          How to connect
        </button>
      )}
    </div>
  );
}

function SourceHealthPanel({
  addons,
  manifests,
  playableCloud,
  packageCloud,
  streamAddons,
}: {
  addons: Addon[];
  manifests: SourceManifest[];
  playableCloud: number;
  packageCloud: number;
  streamAddons: number;
}) {
  const totalCloud = manifests.reduce((sum, manifest) => sum + manifest.sources.length, 0);
  const tested = manifests.reduce(
    (sum, manifest) => sum + manifest.sources.filter((source) => source.reachable != null).length,
    0,
  );
  const reachable = manifests.reduce(
    (sum, manifest) => sum + manifest.sources.filter((source) => source.reachable).length,
    0,
  );
  const hasTorrentio = addons.some((addon) => addon.name.toLowerCase().includes('torrentio'));
  const hasWatchHub = addons.some((addon) => addon.name.toLowerCase().includes('watchhub'));

  return (
    <section className="settings-source-overview">
      <div className="source-overview-head">
        <span className="settings-kicker">Source engine</span>
        <h3>Ranked best-first on every details page</h3>
      </div>
      <div className="source-overview-grid">
        <SourceMetric label="Stream addons" value={String(streamAddons)} detail="Stremio protocol" active={streamAddons > 0} />
        <SourceMetric label="Torrentio" value={hasTorrentio ? 'On' : 'Off'} detail="Seed strength ranked" active={hasTorrentio} />
        <SourceMetric label="WatchHub" value={hasWatchHub ? 'On' : 'Off'} detail="External services" active={hasWatchHub} />
        <SourceMetric label="CloudStream" value={String(playableCloud)} detail="Playable templates" active={playableCloud > 0} />
        <SourceMetric label=".cs3 packages" value={String(packageCloud)} detail="Reachable metadata" active={packageCloud > 0} />
        <SourceMetric label="Tested" value={`${reachable}/${tested || totalCloud || 0}`} detail="CloudStream entries" active={tested > 0} />
      </div>
      <p className="settings-muted source-overview-note">
        Direct and debrid streams rank first, then YouTube, WatchHub, and torrents by seeders,
        resolution, and size. CloudStream URL-template sources join that same list. Imported
        .cs3 packages are probed as packages; they need the CloudStream Android runtime to scrape.
      </p>
    </section>
  );
}

function SourceMetric({
  label,
  value,
  detail,
  active,
}: {
  label: string;
  value: string;
  detail: string;
  active: boolean;
}) {
  return (
    <div className={`source-metric ${active ? 'source-metric-on' : ''}`}>
      <span className="source-metric-label">{label}</span>
      <strong>{value}</strong>
      <span>{detail}</span>
    </div>
  );
}

function AddonSection({
  addons,
  refresh,
  reload,
  openOsk,
}: {
  addons: Addon[];
  refresh: () => void;
  reload: () => void;
  openOsk: SectionProps['openOsk'];
}) {
  const [url, setUrl] = useState('');
  const [status, setStatus] = useState<string | null>(null);

  const add = async () => {
    if (!url.trim()) return;
    setStatus('Installing…');
    try {
      await addAddon(url.trim());
      setUrl('');
      setStatus('Installed');
      await refresh();
      reload();
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    }
  };

  const remove = async (addon: Addon) => {
    await removeAddon(addon.url).catch(() => {});
    await refresh();
    reload();
  };

  return (
    <section className="settings-section">
      <h2>Stremio addons</h2>
      <p className="settings-muted">
        Paste any Stremio addon's manifest URL — e.g.
        https://v3-cinemeta.strem.io/manifest.json,
        https://torrentio.strem.fun/manifest.json, or
        https://watchhub.strem.io/manifest.json. For Torrentio with a debrid
        service (RealDebrid etc.), open Configure, set it up, and paste the
        configured manifest URL it gives you.
      </p>
      <Field label="Manifest URL">
        <input
          type="text"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          onClick={() => openOsk('Manifest URL', url, false, setUrl)}
          placeholder="https://…/manifest.json"
        />
      </Field>
      <div className="settings-actions">
        <button className="btn btn-primary" onClick={add}>
          Add addon
        </button>
        {status && <span className="settings-status">{status}</span>}
      </div>

      {addons.length > 0 && (
        <ul className="addon-list">
          {addons.map((a) => (
            <li key={a.url} className="addon-item">
              <div>
                <div className="addon-name">{a.name}</div>
                <div className="settings-muted addon-meta">
                  {a.catalogs.length} catalog{a.catalogs.length === 1 ? '' : 's'}
                  {a.streams ? ' · streams' : ''}
                  {a.meta ? ' · meta' : ''}
                </div>
              </div>
              <div className="addon-buttons">
                {a.configure_url && (
                  <button className="btn" onClick={() => openUrl(a.configure_url!).catch(() => {})}>
                    Configure
                  </button>
                )}
                <button className="btn" onClick={() => remove(a)}>
                  Remove
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function SourceManifestSection({
  manifests,
  refresh,
  reload,
  openOsk,
}: {
  manifests: SourceManifest[];
  refresh: () => void;
  reload: () => void;
  openOsk: SectionProps['openOsk'];
}) {
  const [text, setText] = useState('');
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const add = async () => {
    const input = text.trim();
    if (!input) {
      setStatus('Paste a manifest URL or its JSON first');
      return;
    }
    setBusy(true);
    setStatus('Adding…');
    try {
      const m = await addSourceManifest(input);
      setText('');
      const n = m.sources.length;
      const playable = m.sources.filter((s) => s.playable).length;
      setStatus(
        playable > 0
          ? `Added “${m.name}” — ${playable} playable source${playable === 1 ? '' : 's'}.`
          : `Added “${m.name}” — ${n} CloudStream plugin descriptor${n === 1 ? '' : 's'}.`
      );
      await refresh();
      reload();
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  };

  // Read the system clipboard into the field (controller / kiosk have no
  // Ctrl+V); on desktop the textarea also accepts a normal paste.
  const paste = async () => {
    try {
      const clip = await navigator.clipboard.readText();
      if (clip) {
        setText(clip);
        setStatus(null);
      } else {
        setStatus('Clipboard is empty');
      }
    } catch {
      setStatus('Clipboard unavailable — type or paste into the box');
    }
  };

  const remove = async (m: SourceManifest) => {
    await removeSourceManifest(m.id).catch(() => {});
    await refresh();
    reload();
  };

  const toggle = async (m: SourceManifest, name: string, enabled: boolean) => {
    await toggleSource(m.id, name, enabled).catch(() => {});
    await refresh();
    reload();
  };

  const test = async (m?: SourceManifest) => {
    setBusy(true);
    setStatus(m ? `Testing “${m.name}”…` : 'Testing all sources…');
    try {
      const updated = await testSourceManifests(m?.id);
      const flat = updated.flatMap((x) => x.sources);
      const up = flat.filter((s) => s.reachable).length;
      const down = flat.filter((s) => s.reachable === false).length;
      const playable = flat.filter((s) => s.playable).length;
      const packages = flat.filter((s) => !s.playable && s.kind === 'cs3').length;
      setStatus(
        playable > 0
          ? `Reachable: ${up} · unreachable (auto-disabled): ${down}`
          : packages > 0
            ? `Packages reachable: ${up} · unreachable: ${down}`
            : 'No testable CloudStream entries yet.'
      );
      await refresh();
      reload();
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-section">
      <h2>CloudStream sources</h2>
      <p className="settings-muted">
        URL-template sources are playable and merge into the same ranked picker as
        Torrentio and WatchHub. CloudStream .cs3 repositories are imported and
        package-tested here; their scraper bytecode still needs a CloudStream runtime.
      </p>
      <Field label="Manifest URL or JSON">
        <textarea
          className="settings-textarea"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onClick={() =>
            openOsk('Manifest URL or JSON', text, false, setText)
          }
          placeholder="https://…/sources.json  — or paste the manifest JSON"
          rows={3}
        />
      </Field>
      <div className="settings-actions">
        <button className="btn btn-primary" onClick={add} disabled={busy}>
          Add sources
        </button>
        <button className="btn" onClick={paste} disabled={busy}>
          Paste
        </button>
        {manifests.length > 0 && (
          <button className="btn" onClick={() => test()} disabled={busy}>
            Test all
          </button>
        )}
        {status && <span className="settings-status">{status}</span>}
      </div>

      {manifests.map((m) => (
        <div key={m.id} className="manifest-block">
          <div className="manifest-head">
            <div>
              <div className="addon-name">{m.name}</div>
              <div className="settings-muted addon-meta">
                {m.source_url ?? 'pasted JSON'}
              </div>
            </div>
            <div className="addon-buttons">
              <button className="btn" onClick={() => test(m)} disabled={busy}>
                Test
              </button>
              <button className="btn" onClick={() => remove(m)} disabled={busy}>
                Remove
              </button>
            </div>
          </div>
          <ul className="manifest-sources">
            {m.sources.map((s) => (
              <li key={s.name} className="manifest-source">
                <label className="manifest-source-toggle">
                  <input
                    type="checkbox"
                    checked={s.enabled}
                    onChange={(e) => toggle(m, s.name, e.target.checked)}
                  />
                  <span className="manifest-source-name">{s.name}</span>
                </label>
                <span className="manifest-source-meta settings-muted">
                  {s.playable
                    ? s.series
                      ? 'movies + series'
                      : 'movies only'
                    : '.cs3 package'}
                  {s.reachable === true && (
                    <span className="reach reach-ok">
                      {' · '}{s.playable ? 'reachable' : 'package reachable'}
                      {s.latency_ms != null ? ` (${s.latency_ms} ms)` : ''}
                    </span>
                  )}
                  {s.reachable === false && (
                    <span className="reach reach-bad">
                      {' · '}{s.playable ? 'unreachable' : 'package unreachable'}
                    </span>
                  )}
                  {s.reachable == null && <span>{' · '}{s.testable ? 'not tested' : 'metadata only'}</span>}
                </span>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </section>
  );
}

/** Common fullscreen output modes; "" follows the display's native resolution. */
const DISPLAY_RESOLUTIONS: { value: string; label: string }[] = [
  { value: '', label: 'Auto (display native)' },
  { value: '3840x2160', label: '3840 × 2160 · 4K UHD' },
  { value: '2560x1440', label: '2560 × 1440 · QHD' },
  { value: '1920x1080', label: '1920 × 1080 · Full HD' },
  { value: '1280x720', label: '1280 × 720 · HD' },
];

/// Display output settings for fullscreen (gamescope) mode. Saved immediately;
/// the launch scripts read them the next time TV OS opens in fullscreen.
function DisplaySection({
  form,
  update,
  submit,
}: {
  form: Settings;
  update: (patch: Partial<Settings>) => void;
  submit: SectionProps['submit'];
}) {
  const [status, setStatus] = useState<string | null>(null);
  const resolution = form.display_resolution?.trim() ?? '';
  const options = DISPLAY_RESOLUTIONS.some((o) => o.value === resolution)
    ? DISPLAY_RESOLUTIONS
    : [...DISPLAY_RESOLUTIONS, { value: resolution, label: resolution }];

  const saved = () => setStatus('Saved — applies next time TV OS opens in fullscreen');
  const setResolution = (value: string) => {
    update({ display_resolution: value });
    submit({ display_resolution: value }).then(saved).catch(() => {});
  };
  const setHdr = (display_hdr: boolean) => {
    update({ display_hdr });
    submit({ display_hdr }).then(saved).catch(() => {});
  };

  return (
    <section className="settings-section">
      <h2>Display</h2>
      <p className="settings-muted">
        Fullscreen (10-foot) mode renders at your display's native resolution by default.
        Override it here only if you want a specific mode. Changes take effect the next time
        TV OS opens in fullscreen.
      </p>
      <Field label="Resolution">
        <select value={resolution} onChange={(e) => setResolution(e.target.value)}>
          {options.map((o) => (
            <option key={o.value || 'auto'} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      </Field>
      <Field label="HDR">
        <label className="settings-check">
          <input
            type="checkbox"
            checked={form.display_hdr}
            onChange={(e) => setHdr(e.target.checked)}
          />
          Enable HDR output on capable displays
        </label>
      </Field>
      {status && <p className="settings-status">{status}</p>}
    </section>
  );
}

function AppearanceSection({
  form,
  update,
  reload,
  submit,
  theme,
  onToggleTheme,
}: SectionProps & {
  theme: Theme;
  onToggleTheme: () => void;
}) {
  const setEnhance = async (enhance: EnhanceMode) => {
    update({ enhance });
    await submit({ enhance }).catch(() => {});
    reload();
  };

  const setAccent = async (accent: string) => {
    update({ accent });
    applyAccent(accent); // instant preview
    await submit({ accent }).catch(() => {});
    reload();
  };

  const currentAccent = form.accent || DEFAULT_ACCENT;

  return (
    <section className="settings-section">
      <h2>Appearance &amp; playback</h2>
      <Field label="Theme">
        <button className="btn" onClick={onToggleTheme}>
          {theme === 'dark' ? 'Dark' : 'Light'} — switch
        </button>
      </Field>
      <Field label="Accent color">
        <div className="accent-swatches">
          {ACCENT_PRESETS.map((c) => (
            <button
              key={c}
              className={`accent-swatch ${currentAccent.toLowerCase() === c.toLowerCase() ? 'accent-swatch-active' : ''}`}
              style={{ background: c }}
              onClick={() => setAccent(c)}
              title={c}
              aria-label={`Accent ${c}`}
            />
          ))}
          <input
            type="color"
            className="accent-picker"
            value={currentAccent}
            onChange={(e) => setAccent(e.target.value)}
            title="Custom color"
          />
        </div>
      </Field>
      <Field label="Enhance (upscaling)">
        <select value={form.enhance} onChange={(e) => setEnhance(e.target.value as EnhanceMode)}>
          {ENHANCE_OPTIONS.map((o) => (
            <option key={o} value={o}>
              {o[0].toUpperCase() + o.slice(1)}
            </option>
          ))}
        </select>
      </Field>
    </section>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="settings-field">
      <span className="settings-field-label">{label}</span>
      {children}
    </label>
  );
}

/** A field that's already configured shows a quiet locked row (masked for
 *  secrets) with an explicit Edit step; empty fields are directly editable.
 *  `configured` (#6): a secret can be set on the daemon yet arrive blanked —
 *  show the locked "•••" state on that flag even when `value` is empty, and
 *  leaving it untouched keeps the stored secret. Focusing the input and
 *  pressing A/Enter (or clicking it) opens the on-screen keyboard so a
 *  controller/remote can type; keyboard users can also type in place. */
function LockedField({
  label,
  value,
  onChange,
  openOsk,
  configured,
  masked = false,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (next: string) => void;
  openOsk: SectionProps['openOsk'];
  configured?: boolean;
  masked?: boolean;
  placeholder?: string;
}) {
  const [editing, setEditing] = useState(false);
  // Locked when the user has typed something, OR the daemon flags it stored.
  const isSet = value.trim() !== '' || !!configured;

  if (isSet && !editing) {
    const shown =
      masked || (configured && value.trim() === '')
        ? '••••••••••••••••••••  configured'
        : value.length > 46
          ? `${value.slice(0, 46)}…`
          : value;
    return (
      <div className="settings-field">
        <span className="settings-field-label">{label}</span>
        <div className="locked-row">
          <span className="locked-value">{shown}</span>
          <button className="btn" onClick={() => setEditing(true)}>
            Edit
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="settings-field">
      <span className="settings-field-label">{label}</span>
      <div className="locked-row">
        <input
          type={masked ? 'password' : 'text'}
          value={value}
          autoFocus={editing}
          onChange={(e) => onChange(e.target.value)}
          onClick={() => openOsk(label, value, masked, onChange)}
          placeholder={placeholder}
        />
        {editing && (
          <button className="btn" onClick={() => setEditing(false)}>
            Done
          </button>
        )}
      </div>
    </div>
  );
}
