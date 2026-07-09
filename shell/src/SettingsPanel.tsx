import { MutableRefObject, useCallback, useEffect, useRef, useState } from 'react';
import {
  Addon,
  EnhanceMode,
  Settings,
  SourceStatus,
  addAddon,
  fetchAddons,
  fetchSettings,
  fetchSources,
  fetchSteamStatus,
  fetchTrackingStatus,
  fetchVersion,
  fetchYouTubeStatus,
  traktConnect,
  openUrl,
  removeAddon,
  saveSettings,
} from './api';
import { activateFocused, moveFocus, stepSelect } from './focusNav';
import { NavAction } from './input';
import { ACCENT_PRESETS, DEFAULT_ACCENT, Theme, UiMode, applyAccent } from './theme';

interface Props {
  onClose: () => void;
  /** Refetch settings + library after a change (updates the home screen). */
  reload: () => void;
  theme: Theme;
  onToggleTheme: () => void;
  mode: UiMode;
  onToggleMode: () => void;
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

export function SettingsPanel({
  onClose,
  reload,
  theme,
  onToggleTheme,
  mode,
  onToggleMode,
  actionRef,
}: Props) {
  const [form, setForm] = useState<Settings>(BLANK);
  const [addons, setAddons] = useState<Addon[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState(false);
  const [version, setVersion] = useState<string | null>(null);
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
        setForm({ ...BLANK, ...s });
        setLoaded(true);
      })
      .catch(() => setLoadError(true));
    refreshAddons();
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
        e.stopPropagation();
        if (editingSelect.current) stopEditing();
        else onClose();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [onClose, stopEditing]);

  const handleAction = useCallback(
    (action: NavAction) => {
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
    [stopEditing],
  );

  useEffect(() => {
    actionRef.current = handleAction;
    return () => {
      actionRef.current = null;
    };
  }, [actionRef, handleAction]);

  // Land focus on the first control so the focus ring is visible immediately.
  useEffect(() => {
    if (loaded || loadError) moveFocus(panelRef.current!, 'down');
  }, [loaded, loadError]);

  const refreshAddons = () => fetchAddons().then(setAddons).catch(() => {});
  const update = (patch: Partial<Settings>) => setForm((f) => ({ ...f, ...patch }));

  return (
    <div className="settings-scrim" onClick={onClose}>
      <div className="settings" ref={panelRef} onClick={(e) => e.stopPropagation()}>
        <div className="settings-head">
          <h1>
            Settings
            {version && <span className="settings-version">tvosd v{version}</span>}
          </h1>
          <button className="btn" onClick={onClose}>
            Close (B / Esc)
          </button>
        </div>

        {loadError ? (
          <p className="settings-muted">
            Could not load settings — the tvosd daemon isn't answering. Check that it's running,
            then reopen this panel.
          </p>
        ) : !loaded ? (
          <p className="settings-muted">Loading…</p>
        ) : (
          <>
            <SteamSection form={form} update={update} reload={reload} />
            <GameLibrariesSection />
            <TmdbSection form={form} update={update} reload={reload} />
            <YouTubeSection form={form} update={update} reload={reload} />
            <TrackingSection form={form} update={update} reload={reload} />
            <AddonSection addons={addons} refresh={refreshAddons} reload={reload} />
            <AppearanceSection
              form={form}
              update={update}
              reload={reload}
              theme={theme}
              onToggleTheme={onToggleTheme}
              mode={mode}
              onToggleMode={onToggleMode}
            />
          </>
        )}
      </div>
    </div>
  );
}

type SectionProps = {
  form: Settings;
  update: (patch: Partial<Settings>) => void;
  reload: () => void;
};

function SteamSection({ form, update, reload }: SectionProps) {
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const connect = async () => {
    setBusy(true);
    setStatus('Connecting…');
    try {
      await saveSettings(form);
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
        onChange={(v) => update({ steam_api_key: v })}
        placeholder="0123456789ABCDEF…"
      />
      <LockedField
        label="SteamID or profile name"
        value={form.steam_id}
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

function TmdbSection({ form, update, reload }: SectionProps) {
  const [status, setStatus] = useState<string | null>(null);

  const save = async () => {
    try {
      await saveSettings(form);
      setStatus(form.tmdb_key ? 'Saved — Movies & Shows rows enabled' : 'Saved');
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

function YouTubeSection({ form, update, reload }: SectionProps) {
  const [status, setStatus] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  const save = async () => {
    try {
      await saveSettings(form);
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
  // cookies are what the daemon reads for the personal feeds.
  const signIn = () => {
    window.open('https://www.youtube.com', '_blank');
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
function TrackingSection({ form, update }: SectionProps) {
  const [status, setStatus] = useState<string | null>(null);
  const [connected, setConnected] = useState({ trakt: false, anilist: false, mal: false });

  const refreshStatus = () =>
    fetchTrackingStatus()
      .then((s) => setConnected({ trakt: s.trakt, anilist: s.anilist, mal: s.mal }))
      .catch(() => {});
  useEffect(() => {
    refreshStatus();
  }, []);

  const save = async () => {
    try {
      await saveSettings(form);
      setStatus('Saved');
      refreshStatus();
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    }
  };

  const connectTrakt = async () => {
    try {
      await saveSettings(form);
      const { user_code, url } = await traktConnect();
      setStatus(`Go to ${url} and enter code ${user_code} — this panel updates when approved.`);
      const poll = window.setInterval(async () => {
        const s = await fetchTrackingStatus().catch(() => null);
        if (s?.trakt) {
          window.clearInterval(poll);
          setStatus('Trakt connected ✓');
          refreshStatus();
        }
      }, 5000);
      window.setTimeout(() => window.clearInterval(poll), 600_000);
    } catch (e) {
      setStatus(`Error: ${(e as Error).message}`);
    }
  };

  const connectMal = async () => {
    try {
      await saveSettings(form);
      window.open('/api/mal/login', '_blank');
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
        onChange={(v) => update({ trakt_client_id: v })}
        placeholder="create an app on trakt.tv, paste its client id"
      />
      <LockedField
        label="Trakt client secret"
        masked
        value={form.trakt_client_secret}
        onChange={(v) => update({ trakt_client_secret: v })}
      />
      <LockedField
        label="AniList access token"
        masked
        value={form.anilist_token}
        onChange={(v) => update({ anilist_token: v })}
        placeholder="from your AniList API client (implicit grant)"
      />
      <LockedField
        label="MyAnimeList client id (redirect URI: http://localhost:8484/api/mal/callback)"
        value={form.mal_client_id}
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

function GameLibrariesSection() {
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
      <GameRegionField />
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
/// Saves immediately — a region change should reprice on the next look.
function GameRegionField() {
  const [region, setRegion] = useState('US');
  useEffect(() => {
    fetchSettings()
      .then((s) => setRegion(s.game_region?.trim().toUpperCase() || 'US'))
      .catch(() => {});
  }, []);

  const change = async (next: string) => {
    setRegion(next);
    try {
      const current = await fetchSettings();
      await saveSettings({ ...current, game_region: next });
    } catch {
      /* daemon unreachable — the select still shows the choice */
    }
  };

  return (
    <Field label="Store region (game prices — Games for you & Where to buy)">
      <select value={region} onChange={(e) => change(e.target.value)}>
        {GAME_REGIONS.map((r) => (
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

function AddonSection({
  addons,
  refresh,
  reload,
}: {
  addons: Addon[];
  refresh: () => void;
  reload: () => void;
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

function AppearanceSection({
  form,
  update,
  reload,
  theme,
  onToggleTheme,
  mode,
  onToggleMode,
}: SectionProps & {
  theme: Theme;
  onToggleTheme: () => void;
  mode: UiMode;
  onToggleMode: () => void;
}) {
  const setEnhance = async (enhance: EnhanceMode) => {
    update({ enhance });
    await saveSettings({ ...form, enhance }).catch(() => {});
    reload();
  };

  const setAccent = async (accent: string) => {
    update({ accent });
    applyAccent(accent); // instant preview
    await saveSettings({ ...form, accent }).catch(() => {});
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
      <Field label="Layout">
        <button className="btn" onClick={onToggleMode}>
          {mode === 'tv' ? 'TV (10-foot)' : 'Desktop'} — switch
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
 *  secrets) with an explicit Edit step; empty fields are directly editable. */
function LockedField({
  label,
  value,
  onChange,
  masked = false,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (next: string) => void;
  masked?: boolean;
  placeholder?: string;
}) {
  const [editing, setEditing] = useState(false);
  const configured = value.trim() !== '';

  if (configured && !editing) {
    const shown = masked
      ? '·······························'
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
