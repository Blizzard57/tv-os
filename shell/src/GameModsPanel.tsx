import { useEffect, useMemo, useState } from 'react';
import {
  GameModsOverview,
  ModProfile,
  activateModProfile,
  createModProfile,
  deployModProfile,
  fetchGameMods,
  importGameMod,
  openUrl,
  removeGameMod,
  rollbackModProfile,
  setGameModEnabled,
} from './api';

type Section = 'overview' | 'browse' | 'installed' | 'profiles' | 'activity';

interface Props {
  gameId: string;
  gameTitle: string;
  onChanged?: () => void;
}

const SECTIONS: { id: Section; label: string }[] = [
  { id: 'overview', label: 'Overview' },
  { id: 'browse', label: 'Browse' },
  { id: 'installed', label: 'Installed' },
  { id: 'profiles', label: 'Profiles' },
  { id: 'activity', label: 'Activity' },
];

export function GameModsPanel({ gameId, gameTitle, onChanged }: Props) {
  const [data, setData] = useState<GameModsOverview | null>(null);
  const [section, setSection] = useState<Section>('overview');
  const [selectedProfile, setSelectedProfile] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  const [profileName, setProfileName] = useState('');
  const [modTitle, setModTitle] = useState('');
  const [modSource, setModSource] = useState('');
  const [modTarget, setModTarget] = useState('');
  const [armedRemove, setArmedRemove] = useState('');

  const reload = () => fetchGameMods(gameId).then((next) => {
    setData(next);
    setSelectedProfile((current) => current || next.profiles.find((p) => p.active)?.id || next.profiles[0]?.id || '');
  }).catch((error) => setMessage(error instanceof Error ? error.message : String(error)));

  useEffect(() => {
    void reload();
  }, [gameId]);

  const profile = data?.profiles.find((p) => p.id === selectedProfile)
    ?? data?.profiles.find((p) => p.active);
  const mods = useMemo(
    () => (data?.installed ?? []).filter((mod) => mod.profile_id === profile?.id),
    [data, profile?.id],
  );

  const perform = async (label: string, action: () => Promise<unknown>) => {
    setBusy(true); setMessage(label);
    try { await action(); await reload(); onChanged?.(); setMessage(`${label} — done`); }
    catch (error) { setMessage(error instanceof Error ? error.message : String(error)); }
    finally { setBusy(false); }
  };

  const activate = (target: ModProfile) => perform('Activating profile', () => activateModProfile(gameId, target.id));
  const deploy = (target: ModProfile) => perform('Validating and deploying', () => deployModProfile(gameId, target.id));

  if (!data) return <div className="mods-panel mods-loading">Loading native mod profiles…</div>;

  return (
    <section className="mods-panel" aria-label={`Mods for ${gameTitle}`}>
      <div className="mods-topline">
        <div>
          <h2>Mods</h2>
          <p>{data.support_detail}</p>
        </div>
        {profile && <HealthPill profile={profile} />}
      </div>

      <nav className="mods-tabs" aria-label="Mod sections">
        {SECTIONS.map((tab, index) => (
          <button key={tab.id} type="button" tabIndex={0} data-primary={index === 0 ? true : undefined}
            className={section === tab.id ? 'active' : ''} onClick={() => setSection(tab.id)}>
            {tab.label}
          </button>
        ))}
      </nav>

      <div className="mods-content">
        {section === 'overview' && (
          <div className="mods-overview-grid">
            <article className="mods-summary-card">
              <span className="mods-eyebrow">Active profile</span>
              <strong>{data.profiles.find((p) => p.active)?.name ?? 'Vanilla'}</strong>
              <span>{data.profiles.find((p) => p.active)?.mod_count ?? 0} enabled mods</span>
              <div className="mods-inline-actions">
                {profile && !profile.active && <button tabIndex={0} onClick={() => activate(profile)}>Make active</button>}
                {profile && <button tabIndex={0} onClick={() => deploy(profile)}>Validate &amp; deploy</button>}
              </div>
            </article>
            <article className="mods-summary-card">
              <span className="mods-eyebrow">Compatibility</span>
              <strong>{profile?.health === 'ready' ? 'Ready to play' : profile?.health === 'blocked' ? 'Action required' : 'Review warnings'}</strong>
              <span>{profile?.issues[0]?.message ?? 'No known dependency or file conflicts.'}</span>
              {!!profile?.issues.length && <button tabIndex={0} onClick={() => setSection('installed')}>Review {profile.issues.length} issue{profile.issues.length === 1 ? '' : 's'}</button>}
            </article>
            <article className="mods-summary-card">
              <span className="mods-eyebrow">Sources</span>
              <strong>{data.providers.filter((p) => p.connected).length} connected</strong>
              <span>Nexus, Workshop, Thunderstore and local packages share one profile engine.</span>
              <button tabIndex={0} onClick={() => setSection('browse')}>Browse providers</button>
            </article>
          </div>
        )}

        {section === 'browse' && (
          <div className="mods-browse-layout">
            <div className="mods-provider-list">
              {data.providers.map((provider) => (
                <article className="mods-provider-row" key={provider.id}>
                  <span className={`provider-dot ${provider.connected ? 'connected' : ''}`} />
                  <div><strong>{provider.name}</strong><span>{provider.detail}</span></div>
                  <span className="mods-provider-mode">{provider.mode.replace('_', ' ')}</span>
                  {provider.browse_url && <button tabIndex={0} onClick={() => openUrl(provider.browse_url!)}>Open</button>}
                </article>
              ))}
            </div>
            <form className="mods-import" onSubmit={(event) => {
              event.preventDefault();
              if (!profile || !modTitle.trim() || !modSource.trim()) return;
              perform('Inspecting and importing package', () => importGameMod(gameId, profile.id, modTitle.trim(), modSource.trim(), modTarget.trim()))
                .then(() => { setModTitle(''); setModSource(''); setModTarget(''); });
            }}>
              <span className="mods-eyebrow">Local or direct package</span>
              <h3>Add to {profile?.name ?? 'profile'}</h3>
              <label>Mod name<input tabIndex={0} value={modTitle} onChange={(e) => setModTitle(e.target.value)} placeholder="Community patch" /></label>
              <label>ZIP, folder, or HTTPS URL<input tabIndex={0} value={modSource} onChange={(e) => setModSource(e.target.value)} placeholder="/home/me/Downloads/mod.zip" /></label>
              <label>Game subfolder <span>optional</span><input tabIndex={0} value={modTarget} onChange={(e) => setModTarget(e.target.value)} placeholder="Mods" /></label>
              <button tabIndex={0} type="submit" disabled={busy || !profile || !modTitle.trim() || !modSource.trim()}>Inspect &amp; add</button>
            </form>
          </div>
        )}

        {section === 'installed' && (
          <div className="mods-installed-layout">
            <ProfilePicker profiles={data.profiles} selected={profile?.id ?? ''} onSelect={setSelectedProfile} />
            <div className="mods-list">
              {mods.map((mod) => (
                <article className={`mods-row ${mod.enabled ? '' : 'disabled'}`} key={mod.id}>
                  <button className={`mods-toggle ${mod.enabled ? 'on' : ''}`} tabIndex={0}
                    onClick={() => perform(mod.enabled ? 'Disabling mod' : 'Enabling mod', () => setGameModEnabled(gameId, profile!.id, mod.id, !mod.enabled))}>
                    {mod.enabled ? 'On' : 'Off'}
                  </button>
                  <div className="mods-row-main"><strong>{mod.title}</strong><span>{mod.provider} · {mod.version} · {mod.file_count} files</span></div>
                  <span className={`mods-security ${mod.security === 'data-only' ? 'safe' : ''}`}>{mod.security}</span>
                  <button className={armedRemove === mod.id ? 'danger armed' : 'danger'} tabIndex={0} onClick={() => {
                    if (armedRemove !== mod.id) { setArmedRemove(mod.id); setMessage('Press Remove again to confirm'); return; }
                    setArmedRemove(''); perform('Removing mod', () => removeGameMod(gameId, profile!.id, mod.id));
                  }}>{armedRemove === mod.id ? 'Confirm remove' : 'Remove'}</button>
                </article>
              ))}
              {mods.length === 0 && <div className="mods-empty">No mods in this profile. Browse a provider or import a package.</div>}
              {(profile?.issues ?? []).map((issue, index) => <div className={`mods-issue ${issue.severity}`} key={`${issue.code}-${index}`}><strong>{issue.code.replace('_', ' ')}</strong><span>{issue.message}</span></div>)}
            </div>
          </div>
        )}

        {section === 'profiles' && (
          <div className="mods-profiles-layout">
            <div className="mods-profile-grid">
              {data.profiles.map((candidate) => (
                <article className={`mods-profile-card ${candidate.id === profile?.id ? 'selected' : ''}`} key={candidate.id}>
                  <button className="mods-profile-select" tabIndex={0} onClick={() => setSelectedProfile(candidate.id)}>
                    <HealthPill profile={candidate} /><strong>{candidate.name}</strong><span>{candidate.mod_count} mods · revision {candidate.revision}</span>
                  </button>
                  <div className="mods-inline-actions">
                    {!candidate.active && <button tabIndex={0} onClick={() => activate(candidate)}>Activate</button>}
                    <button tabIndex={0} onClick={() => deploy(candidate)}>Deploy</button>
                    {!candidate.locked && <button tabIndex={0} onClick={() => perform('Rolling back deployment', () => rollbackModProfile(gameId, candidate.id))}>Restore game</button>}
                  </div>
                </article>
              ))}
            </div>
            <form className="mods-new-profile" onSubmit={(event) => {
              event.preventDefault(); if (!profileName.trim()) return;
              perform('Creating profile', () => createModProfile(gameId, profileName.trim(), profile?.id)).then(() => setProfileName(''));
            }}>
              <h3>Clone profile</h3><p>Create a safe branch before changing a working setup.</p>
              <input tabIndex={0} value={profileName} onChange={(e) => setProfileName(e.target.value)} placeholder="Profile name" />
              <button tabIndex={0} type="submit" disabled={!profileName.trim() || busy}>Create from {profile?.name ?? 'Vanilla'}</button>
            </form>
          </div>
        )}

        {section === 'activity' && (
          <div className="mods-activity">
            {data.jobs.map((job) => <article className="mods-job" key={job.id}><div><strong>{job.title}</strong><span>{job.phase} · {job.detail}</span></div><div className="mods-progress"><i style={{ width: `${job.progress}%` }} /></div><b>{job.status}</b></article>)}
            {data.jobs.length === 0 && <div className="mods-empty">No mod transactions yet.</div>}
          </div>
        )}
      </div>
      {message && <div className="mods-message">{message}</div>}
    </section>
  );
}

function HealthPill({ profile }: { profile: ModProfile }) {
  return <span className={`mods-health ${profile.health}`}>{profile.health === 'ready' ? '✓ Ready' : profile.health === 'blocked' ? '! Blocked' : '△ Warnings'}</span>;
}

function ProfilePicker({ profiles, selected, onSelect }: { profiles: ModProfile[]; selected: string; onSelect: (id: string) => void }) {
  return <aside className="mods-profile-picker"><span className="mods-eyebrow">Profile</span>{profiles.map((profile) => <button tabIndex={0} key={profile.id} className={profile.id === selected ? 'active' : ''} onClick={() => onSelect(profile.id)}><span>{profile.name}</span><small>{profile.mod_count} mods</small></button>)}</aside>;
}
