import { useEffect, useState } from 'react';
import {
  Addon,
  EnhanceMode,
  Settings,
  addAddon,
  fetchAddons,
  fetchSettings,
  fetchSteamStatus,
  removeAddon,
  saveSettings,
} from './api';
import { Theme } from './theme';

interface Props {
  onClose: () => void;
  /** Refetch settings + library after a change (updates the home screen). */
  reload: () => void;
  theme: Theme;
  onToggleTheme: () => void;
}

const ENHANCE_OPTIONS: EnhanceMode[] = ['auto', 'quality', 'performance', 'off'];

const BLANK: Settings = { enhance: 'auto', steam_api_key: '', steam_id: '', tmdb_key: '' };

export function SettingsPanel({ onClose, reload, theme, onToggleTheme }: Props) {
  const [form, setForm] = useState<Settings>(BLANK);
  const [addons, setAddons] = useState<Addon[]>([]);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    fetchSettings()
      .then((s) => setForm({ ...BLANK, ...s }))
      .finally(() => setLoaded(true));
    refreshAddons();
  }, []);

  // Close on Escape regardless of which field has focus.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [onClose]);

  const refreshAddons = () => fetchAddons().then(setAddons).catch(() => {});
  const update = (patch: Partial<Settings>) => setForm((f) => ({ ...f, ...patch }));

  return (
    <div className="settings-scrim" onClick={onClose}>
      <div className="settings" onClick={(e) => e.stopPropagation()}>
        <div className="settings-head">
          <h1>Settings</h1>
          <button className="btn" onClick={onClose}>
            Close (B / Esc)
          </button>
        </div>

        {!loaded ? (
          <p className="settings-muted">Loading…</p>
        ) : (
          <>
            <SteamSection form={form} update={update} reload={reload} />
            <TmdbSection form={form} update={update} reload={reload} />
            <AddonSection addons={addons} refresh={refreshAddons} reload={reload} />
            <AppearanceSection
              form={form}
              update={update}
              reload={reload}
              theme={theme}
              onToggleTheme={onToggleTheme}
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
      <Field label="Web API key">
        <input
          type="password"
          value={form.steam_api_key}
          onChange={(e) => update({ steam_api_key: e.target.value })}
          placeholder="0123456789ABCDEF…"
        />
      </Field>
      <Field label="SteamID or profile name">
        <input
          type="text"
          value={form.steam_id}
          onChange={(e) => update({ steam_id: e.target.value })}
          placeholder="76561197960287930 or gabelogannewell"
        />
      </Field>
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
      <Field label="TMDB API key">
        <input
          type="password"
          value={form.tmdb_key}
          onChange={(e) => update({ tmdb_key: e.target.value })}
          placeholder="TMDB API key (v3 auth)"
        />
      </Field>
      <div className="settings-actions">
        <button className="btn btn-primary" onClick={save}>
          Save
        </button>
        {status && <span className="settings-status">{status}</span>}
      </div>
    </section>
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
        https://v3-cinemeta.strem.io/manifest.json
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
                </div>
              </div>
              <button className="btn" onClick={() => remove(a)}>
                Remove
              </button>
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
}: SectionProps & { theme: Theme; onToggleTheme: () => void }) {
  const setEnhance = async (enhance: EnhanceMode) => {
    update({ enhance });
    await saveSettings({ ...form, enhance }).catch(() => {});
    reload();
  };

  return (
    <section className="settings-section">
      <h2>Appearance &amp; playback</h2>
      <Field label="Theme">
        <button className="btn" onClick={onToggleTheme}>
          {theme === 'dark' ? 'Dark' : 'Light'} — switch
        </button>
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
