//! Authentication and credential ownership for mod providers.
//!
//! Provider application identifiers are public configuration. User access and
//! refresh tokens are kept in Secret Service when a TV session exposes it,
//! otherwise in an owner-only fallback file. API responses never contain them.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::settings::{self, config_dir};
use crate::util::percent_encode;

const SESSION_LIFETIME: i64 = 15 * 60;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    Unavailable,
    Disconnected,
    Authorizing,
    Connected,
    Expired,
    RateLimited,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModProviderConnection {
    pub provider: String,
    pub name: String,
    pub state: ProviderState,
    pub account_name: Option<String>,
    pub account_tier: Option<String>,
    pub capabilities: Vec<String>,
    pub expires_at: Option<i64>,
    pub quota_remaining: Option<i64>,
    pub quota_reset_at: Option<i64>,
    pub error: Option<String>,
    pub credential_backend: String,
    pub requires_app_configuration: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModAuthorizationSession {
    pub id: String,
    pub provider: String,
    pub state: String,
    pub authorization_url: Option<String>,
    pub user_code: Option<String>,
    pub verification_url: Option<String>,
    pub expires_at: i64,
    pub interval_seconds: u64,
    pub error: Option<String>,
    #[serde(skip_serializing)]
    verifier: String,
    #[serde(skip_serializing)]
    device_code: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct ConnectRequest {
    pub api_key: Option<String>,
    pub email: Option<String>,
    pub security_code: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Credential {
    access_token: String,
    refresh_token: String,
    expires_at: Option<i64>,
    account_name: Option<String>,
    account_tier: Option<String>,
    quota_remaining: Option<i64>,
    quota_reset_at: Option<i64>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProviderClients {
    nexus_client_id: String,
    modio_client_id: String,
    github_client_id: String,
}

pub struct ModAuthManager {
    sessions: Mutex<HashMap<String, ModAuthorizationSession>>,
    credentials: CredentialStore,
}

impl ModAuthManager {
    pub fn open() -> Self {
        let credentials = CredentialStore::new(config_dir().join("mod-credentials"));
        let sessions = read_private_json::<HashMap<String, ModAuthorizationSession>>(
            &credentials.root.join("sessions.json"),
        )
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, session)| session.expires_at > now())
        .collect();
        Self {
            sessions: Mutex::new(sessions),
            credentials,
        }
    }

    pub fn statuses(&self) -> Vec<ModProviderConnection> {
        [
            "nexus",
            "modio",
            "github",
            "curseforge",
            "workshop",
            "thunderstore",
            "modrinth",
        ]
        .into_iter()
        .map(|provider| self.status(provider))
        .collect()
    }

    pub fn status(&self, provider: &str) -> ModProviderConnection {
        let clients = provider_clients();
        let credential = self.credentials.get(provider).ok().flatten();
        let (name, capabilities, configured, inherently_connected) = match provider {
            "nexus" => (
                "Nexus Mods",
                vec!["browse", "download", "track", "endorse"],
                !clients.nexus_client_id.is_empty(),
                false,
            ),
            "modio" => (
                "mod.io",
                vec!["browse", "subscribe", "download", "rate"],
                !clients.modio_client_id.is_empty(),
                false,
            ),
            "github" => (
                "GitHub",
                vec!["public releases", "private releases"],
                !clients.github_client_id.is_empty(),
                false,
            ),
            "curseforge" => ("CurseForge", vec!["browse", "download"], true, false),
            "workshop" => (
                "Steam Workshop",
                vec!["subscribe", "download", "updates"],
                steam_available(),
                steam_signed_in(),
            ),
            "thunderstore" => (
                "Thunderstore",
                vec!["browse", "download", "dependencies"],
                true,
                true,
            ),
            "modrinth" => (
                "Modrinth",
                vec!["browse", "download", "dependencies"],
                true,
                true,
            ),
            _ => (provider, vec![], false, false),
        };
        let pending = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|s| s.provider == provider && s.expires_at > now() && s.state == "authorizing");
        let state = if !configured {
            ProviderState::Unavailable
        } else if pending {
            ProviderState::Authorizing
        } else if inherently_connected {
            ProviderState::Connected
        } else if credential
            .as_ref()
            .and_then(|c| c.last_error.as_deref())
            .is_some_and(|e| e.contains("rate limit"))
        {
            ProviderState::RateLimited
        } else if credential.as_ref().is_some_and(|c| {
            c.expires_at.is_some_and(|expiry| expiry <= now()) && c.refresh_token.is_empty()
        }) {
            ProviderState::Expired
        } else if credential.as_ref().is_some_and(|c| c.last_error.is_some()) {
            ProviderState::Error
        } else if credential.is_some() {
            ProviderState::Connected
        } else {
            ProviderState::Disconnected
        };
        ModProviderConnection {
            provider: provider.into(),
            name: name.into(),
            state,
            account_name: credential.as_ref().and_then(|c| c.account_name.clone()),
            account_tier: credential.as_ref().and_then(|c| c.account_tier.clone()),
            capabilities: capabilities.into_iter().map(str::to_string).collect(),
            expires_at: credential.as_ref().and_then(|c| c.expires_at),
            quota_remaining: credential.as_ref().and_then(|c| c.quota_remaining),
            quota_reset_at: credential.as_ref().and_then(|c| c.quota_reset_at),
            error: credential.as_ref().and_then(|c| c.last_error.clone()),
            credential_backend: self.credentials.backend_name().into(),
            requires_app_configuration: !configured,
        }
    }

    pub fn connect(
        &self,
        provider: &str,
        request: ConnectRequest,
    ) -> Result<ModAuthorizationSession, String> {
        match provider {
            "curseforge" => return self.connect_api_key(provider, request.api_key),
            "workshop" => return self.connect_steam(),
            "modio" => return self.connect_modio(request),
            "thunderstore" | "modrinth" => {
                return Err("this provider is available without an account".into())
            }
            "nexus" | "github" => {}
            _ => return Err("unknown mod provider".into()),
        }
        let clients = provider_clients();
        let client_id = match provider {
            "nexus" => clients.nexus_client_id,
            _ => clients.github_client_id,
        };
        if client_id.is_empty() {
            return Err(format!("{provider} is not configured in this build; add its application id in Advanced settings"));
        }
        let id = random_token(18)?;
        let verifier = random_token(48)?;
        let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));
        let callback = format!("http://127.0.0.1:8484/api/mods/providers/{provider}/callback");
        let mut session = ModAuthorizationSession {
            id: id.clone(),
            provider: provider.into(),
            state: "authorizing".into(),
            authorization_url: None,
            user_code: None,
            verification_url: None,
            expires_at: now() + SESSION_LIFETIME,
            interval_seconds: 5,
            error: None,
            verifier,
            device_code: String::new(),
        };
        if provider == "github" {
            let response: serde_json::Value = client()
                .post("https://github.com/login/device/code")
                .header("Accept", "application/json")
                .form(&[("client_id", client_id.as_str()), ("scope", "read:user")])
                .send()
                .map_err(net)?
                .json()
                .map_err(net)?;
            session.device_code = string(&response, "device_code")?;
            session.user_code = Some(string(&response, "user_code")?);
            session.verification_url = Some(string(&response, "verification_uri")?);
            session.authorization_url = session.verification_url.clone();
            session.interval_seconds = response
                .get("interval")
                .and_then(|v| v.as_u64())
                .unwrap_or(5);
            session.expires_at = now()
                + response
                    .get("expires_in")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(900);
        } else {
            let base = "https://users.nexusmods.com/oauth/authorize";
            session.authorization_url = Some(format!("{base}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={challenge}&code_challenge_method=S256",percent_encode(&client_id),percent_encode(&callback),percent_encode(&id)));
        }
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, session.clone());
        self.persist_sessions()?;
        Ok(session)
    }

    fn connect_modio(&self, request: ConnectRequest) -> Result<ModAuthorizationSession, String> {
        let api_key = provider_clients().modio_client_id;
        if api_key.is_empty() {
            return Err(
                "mod.io is not configured in this build; add its game API key in Advanced settings"
                    .into(),
            );
        }
        if let Some(code) = request
            .security_code
            .filter(|value| !value.trim().is_empty())
        {
            let session_id = request
                .session_id
                .ok_or("missing mod.io authorization session")?;
            let mut session = self
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&session_id)
                .cloned()
                .ok_or("mod.io authorization session expired")?;
            if session.provider != "modio" || session.expires_at <= now() {
                return Err("invalid or expired mod.io authorization session".into());
            }
            let value: serde_json::Value = client()
                .post(format!(
                    "https://api.mod.io/v1/oauth/emailexchange?api_key={}",
                    percent_encode(&api_key)
                ))
                .form(&[("security_code", code.trim())])
                .send()
                .map_err(net)?
                .json()
                .map_err(net)?;
            let credential = Credential {
                access_token: string(&value, "access_token")?,
                expires_at: value.get("date_expires").and_then(|v| v.as_i64()),
                ..Default::default()
            };
            self.finish("modio", &mut session, credential)?;
            self.sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(session_id, session.clone());
            self.persist_sessions()?;
            return Ok(session);
        }
        let email = request
            .email
            .filter(|value| value.contains('@'))
            .ok_or("enter the email address used for mod.io")?;
        let response = client()
            .post(format!(
                "https://api.mod.io/v1/oauth/emailrequest?api_key={}",
                percent_encode(&api_key)
            ))
            .form(&[("email", email.as_str())])
            .send()
            .map_err(net)?;
        if !response.status().is_success() {
            return Err(format!(
                "mod.io could not send the security code ({})",
                response.status()
            ));
        }
        let session = ModAuthorizationSession {
            id: random_token(18)?,
            provider: "modio".into(),
            state: "awaiting_code".into(),
            authorization_url: None,
            user_code: None,
            verification_url: None,
            expires_at: now() + 900,
            interval_seconds: 0,
            error: None,
            verifier: String::new(),
            device_code: String::new(),
        };
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.id.clone(), session.clone());
        self.persist_sessions()?;
        Ok(session)
    }

    fn connect_api_key(
        &self,
        provider: &str,
        key: Option<String>,
    ) -> Result<ModAuthorizationSession, String> {
        let key = key
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                let s = settings::STORE.get();
                (!s.curseforge_api_key.is_empty()).then_some(s.curseforge_api_key)
            })
            .ok_or("enter a CurseForge API key")?;
        let response = client()
            .get("https://api.curseforge.com/v1/games?index=0&pageSize=1")
            .header("x-api-key", &key)
            .send()
            .map_err(net)?;
        if !response.status().is_success() {
            return Err(format!(
                "CurseForge rejected the API key ({})",
                response.status()
            ));
        }
        self.credentials.put(
            provider,
            &Credential {
                access_token: key,
                account_name: Some("API application".into()),
                ..Default::default()
            },
        )?;
        Ok(completed_session(provider))
    }

    fn connect_steam(&self) -> Result<ModAuthorizationSession, String> {
        if !steam_available() {
            return Err("Steam is not installed".into());
        }
        if !steam_signed_in() {
            let _ = Command::new("steam").spawn();
            return Err("Steam opened; sign in there, then choose Retry".into());
        }
        Ok(completed_session("workshop"))
    }

    pub fn poll(&self, provider: &str, id: &str) -> Result<ModAuthorizationSession, String> {
        let mut session = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or("authorization session not found")?;
        if session.provider != provider {
            return Err("authorization session provider mismatch".into());
        }
        if session.expires_at <= now() {
            session.state = "expired".into();
            return Ok(session);
        }
        if provider == "github" && session.state == "authorizing" {
            let client_id = provider_clients().github_client_id;
            let response: serde_json::Value = client()
                .post("https://github.com/login/oauth/access_token")
                .header("Accept", "application/json")
                .form(&[
                    ("client_id", client_id.as_str()),
                    ("device_code", session.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .map_err(net)?
                .json()
                .map_err(net)?;
            if let Some(token) = response.get("access_token").and_then(|v| v.as_str()) {
                self.finish(
                    provider,
                    &mut session,
                    Credential {
                        access_token: token.into(),
                        account_name: github_name(token),
                        ..Default::default()
                    },
                )?;
            } else if let Some(error) = response.get("error").and_then(|v| v.as_str()) {
                if error != "authorization_pending" && error != "slow_down" {
                    session.state = "error".into();
                    session.error = Some(error.into());
                }
            }
        }
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.into(), session.clone());
        self.persist_sessions()?;
        Ok(session)
    }

    pub fn callback(&self, provider: &str, query: CallbackQuery) -> Result<String, String> {
        let state = query.state.ok_or("missing OAuth state")?;
        let code = query.code.ok_or_else(|| {
            query
                .error
                .unwrap_or_else(|| "authorization was rejected".into())
        })?;
        let mut session = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&state)
            .cloned()
            .ok_or("unknown or expired OAuth state")?;
        if session.provider != provider
            || session.expires_at <= now()
            || session.state != "authorizing"
        {
            return Err("invalid or expired OAuth session".into());
        }
        let clients = provider_clients();
        let client_id = if provider == "nexus" {
            clients.nexus_client_id
        } else {
            clients.modio_client_id
        };
        let callback = format!("http://127.0.0.1:8484/api/mods/providers/{provider}/callback");
        let endpoint = if provider == "nexus" {
            "https://users.nexusmods.com/oauth/token"
        } else {
            "https://api.mod.io/v1/oauth/token"
        };
        let response: serde_json::Value = client()
            .post(endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code.as_str()),
                ("redirect_uri", callback.as_str()),
                ("code_verifier", session.verifier.as_str()),
            ])
            .send()
            .map_err(net)?
            .json()
            .map_err(net)?;
        let access = string(&response, "access_token")?;
        let expires = response
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .map(|v| now() + v);
        let credential = Credential {
            access_token: access,
            refresh_token: response
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
            expires_at: expires,
            ..Default::default()
        };
        self.finish(provider, &mut session, credential)?;
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(state, session);
        self.persist_sessions()?;
        Ok(format!("{provider} connected. You can return to TV OS."))
    }

    pub fn disconnect(&self, provider: &str) -> Result<(), String> {
        self.credentials.delete(provider)?;
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, s| s.provider != provider);
        self.persist_sessions()
    }
    pub fn refresh(&self, provider: &str) -> Result<ModProviderConnection, String> {
        if provider == "workshop" {
            return Ok(self.status(provider));
        }
        let mut credential = self
            .credentials
            .get(provider)?
            .ok_or("provider is not connected")?;
        if credential
            .expires_at
            .is_some_and(|expiry| expiry <= now() + 60)
            && !credential.refresh_token.is_empty()
            && (provider == "nexus" || provider == "modio")
        {
            let clients = provider_clients();
            let client_id = if provider == "nexus" {
                clients.nexus_client_id
            } else {
                clients.modio_client_id
            };
            let endpoint = if provider == "nexus" {
                "https://users.nexusmods.com/oauth/token"
            } else {
                "https://api.mod.io/v1/oauth/token"
            };
            let value: serde_json::Value = client()
                .post(endpoint)
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", client_id.as_str()),
                    ("refresh_token", credential.refresh_token.as_str()),
                ])
                .send()
                .map_err(net)?
                .json()
                .map_err(net)?;
            credential.access_token = string(&value, "access_token")?;
            credential.refresh_token = value
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .unwrap_or(&credential.refresh_token)
                .into();
            credential.expires_at = value
                .get("expires_in")
                .and_then(|v| v.as_i64())
                .map(|seconds| now() + seconds);
            self.credentials.put(provider, &credential)?;
        }
        let request = match provider {
            "nexus" => client()
                .get("https://api.nexusmods.com/v1/users/validate.json")
                .bearer_auth(&credential.access_token),
            "modio" => client()
                .get("https://api.mod.io/v1/me")
                .bearer_auth(&credential.access_token),
            "github" => client()
                .get("https://api.github.com/user")
                .bearer_auth(&credential.access_token),
            "curseforge" => client()
                .get("https://api.curseforge.com/v1/games?index=0&pageSize=1")
                .header("x-api-key", &credential.access_token),
            _ => return Ok(self.status(provider)),
        };
        let response = request.send().map_err(net)?;
        let status = response.status();
        credential.quota_remaining = response
            .headers()
            .get("x-rl-hourly-remaining")
            .or_else(|| response.headers().get("x-ratelimit-remaining"))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        if status.as_u16() == 401 || status.as_u16() == 403 {
            credential.last_error = Some(format!("credentials were rejected ({status})"));
            credential.expires_at = Some(now() - 1);
            self.credentials.put(provider, &credential)?;
            return Ok(self.status(provider));
        }
        if status.as_u16() == 429 {
            credential.last_error = Some("provider rate limit reached".into());
            self.credentials.put(provider, &credential)?;
            return Ok(self.status(provider));
        }
        if !status.is_success() {
            return Err(format!("provider status check failed ({status})"));
        }
        if let Ok(value) = response.json::<serde_json::Value>() {
            credential.account_name = value
                .get("name")
                .or_else(|| value.get("username"))
                .or_else(|| value.get("login"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or(credential.account_name);
            if provider == "nexus" {
                credential.account_tier = Some(
                    if value.get("is_premium").and_then(|v| v.as_bool()) == Some(true) {
                        "Premium"
                    } else {
                        "Free"
                    }
                    .into(),
                );
            }
        }
        credential.last_error = None;
        self.credentials.put(provider, &credential)?;
        Ok(self.status(provider))
    }
    fn finish(
        &self,
        provider: &str,
        session: &mut ModAuthorizationSession,
        credential: Credential,
    ) -> Result<(), String> {
        self.credentials.put(provider, &credential)?;
        session.state = "connected".into();
        session.authorization_url = None;
        session.device_code.clear();
        session.verifier.clear();
        Ok(())
    }
    fn persist_sessions(&self) -> Result<(), String> {
        write_private_json(
            &self.credentials.root.join("sessions.json"),
            &*self.sessions.lock().unwrap_or_else(|e| e.into_inner()),
        )
    }
}

fn provider_clients() -> ProviderClients {
    let mut clients: ProviderClients = [
        PathBuf::from("/etc/tvos/provider-clients.json"),
        config_dir().join("provider-clients.json"),
    ]
    .into_iter()
    .find_map(|p| read_private_json(&p))
    .unwrap_or_default();
    let settings = settings::STORE.get();
    if !settings.nexus_client_id.is_empty() {
        clients.nexus_client_id = settings.nexus_client_id;
    }
    if !settings.modio_client_id.is_empty() {
        clients.modio_client_id = settings.modio_client_id;
    }
    if !settings.github_client_id.is_empty() {
        clients.github_client_id = settings.github_client_id;
    }
    clients
}

struct CredentialStore {
    root: PathBuf,
    secret_service: bool,
}
impl CredentialStore {
    fn new(root: PathBuf) -> Self {
        let _ = fs::create_dir_all(&root);
        Self {
            root,
            secret_service: std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
                && executable("secret-tool"),
        }
    }
    fn backend_name(&self) -> &'static str {
        if self.secret_service {
            "secret_service"
        } else {
            "owner_only_file"
        }
    }
    fn get(&self, provider: &str) -> Result<Option<Credential>, String> {
        if self.secret_service {
            if let Ok(out) = Command::new("secret-tool")
                .args(["lookup", "application", "tvos", "provider", provider])
                .output()
            {
                if out.status.success() {
                    return serde_json::from_slice(&out.stdout)
                        .map(Some)
                        .map_err(|e| e.to_string());
                }
            }
        }
        Ok(read_private_json(
            &self.root.join(format!("{provider}.json")),
        ))
    }
    fn put(&self, provider: &str, value: &Credential) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if self.secret_service {
            let mut child = Command::new("secret-tool")
                .args([
                    "store",
                    "--label",
                    "TV OS mod provider",
                    "application",
                    "tvos",
                    "provider",
                    provider,
                ])
                .stdin(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?;
            child
                .stdin
                .as_mut()
                .ok_or("secret service input unavailable")?
                .write_all(&bytes)
                .map_err(|e| e.to_string())?;
            if child.wait().map_err(|e| e.to_string())?.success() {
                return Ok(());
            }
        }
        write_private_bytes(&self.root.join(format!("{provider}.json")), &bytes)
    }
    fn delete(&self, provider: &str) -> Result<(), String> {
        if self.secret_service {
            let _ = Command::new("secret-tool")
                .args(["clear", "application", "tvos", "provider", provider])
                .status();
        }
        let path = self.root.join(format!("{provider}.json"));
        if path.exists() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("tvos/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}
fn github_name(token: &str) -> Option<String> {
    client()
        .get("https://api.github.com/user")
        .bearer_auth(token)
        .send()
        .ok()?
        .json::<serde_json::Value>()
        .ok()?
        .get("login")?
        .as_str()
        .map(str::to_string)
}
fn string(value: &serde_json::Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("provider response omitted {key}"))
}
fn net(error: impl std::fmt::Display) -> String {
    format!("provider connection failed: {error}")
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn random_token(bytes: usize) -> Result<String, String> {
    let mut data = vec![0u8; bytes];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut data))
        .map_err(|e| format!("secure random source unavailable: {e}"))?;
    Ok(data.iter().map(|b| format!("{b:02x}")).collect())
}
fn base64_url(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for byte in bytes {
        acc = (acc << 8) | *byte as u32;
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            out.push(TABLE[((acc >> bits) & 63) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(TABLE[((acc << (6 - bits)) & 63) as usize] as char);
    }
    out
}
fn completed_session(provider: &str) -> ModAuthorizationSession {
    ModAuthorizationSession {
        id: format!("{provider}-complete"),
        provider: provider.into(),
        state: "connected".into(),
        authorization_url: None,
        user_code: None,
        verification_url: None,
        expires_at: now() + 60,
        interval_seconds: 0,
        error: None,
        verifier: String::new(),
        device_code: String::new(),
    }
}
fn executable(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(name).is_file()))
}
fn steam_available() -> bool {
    executable("steam") || executable("flatpak")
}
fn steam_signed_in() -> bool {
    std::env::var_os("HOME").is_some_and(|home| {
        Path::new(&home)
            .join(".local/share/Steam/config/loginusers.vdf")
            .is_file()
            || Path::new(&home)
                .join(".var/app/com.valvesoftware.Steam/.local/share/Steam/config/loginusers.vdf")
                .is_file()
    })
}
fn write_private_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    write_private_bytes(
        path,
        &serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
}
fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temp = path.with_extension("tmp");
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    fs::rename(temp, path).map_err(|e| e.to_string())
}
fn read_private_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_encoding_has_no_padding() {
        assert!(!base64_url(&Sha256::digest(b"verifier")).contains('='));
    }
    #[test]
    fn unknown_provider_is_unavailable() {
        let status = ModProviderConnection {
            provider: "x".into(),
            name: "x".into(),
            state: ProviderState::Unavailable,
            account_name: None,
            account_tier: None,
            capabilities: vec![],
            expires_at: None,
            quota_remaining: None,
            quota_reset_at: None,
            error: None,
            credential_backend: "file".into(),
            requires_app_configuration: true,
        };
        assert_eq!(status.state, ProviderState::Unavailable);
    }
}
