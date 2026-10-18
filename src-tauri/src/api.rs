//! GPN VPS API client: user auth (login/register/refresh/logout, single-flight refresh) and
//! authenticated requests carrying the `x-gpn-peer` header the server requires for targets.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const TIMEOUT: Duration = Duration::from_secs(10);
/// Refresh 1h before the server's 24h expiry.
const SESSION_TTL_MS: u64 = 23 * 3600 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSession {
    pub username: String,
    pub role: String,
    #[serde(default)]
    pub status: Option<String>,
    pub access_token: String,
    pub refresh_token: String,
    /// unix ms
    pub expires_at: u64,
}

/// What the UI is allowed to see of the session (never the tokens).
#[derive(Debug, Clone, Serialize)]
pub struct AuthInfo {
    pub username: String,
    pub role: String,
}

type SessionSink = Box<dyn Fn(Option<AuthSession>) + Send + Sync>;

pub struct GpnApi {
    http: reqwest::Client,
    base_url: Mutex<String>,
    token: Mutex<String>,
    session: Mutex<Option<AuthSession>>,
    /// WireGuard public key of this client, sent as `x-gpn-peer`
    peer_key: Mutex<Option<String>>,
    /// serialises refreshes: refresh tokens are one-time use
    refresh_lock: tokio::sync::Mutex<()>,
    on_session: SessionSink,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

pub fn normalize_url(u: &str) -> String {
    u.trim().trim_end_matches('/').to_string()
}

/// https, or http to a loopback host (so credentials never cross the network in clear text).
pub fn is_secure_or_local(u: &str) -> bool {
    let lc = u.trim().to_lowercase();
    if lc.starts_with("https://") {
        return true;
    }
    let Some(rest) = lc.strip_prefix("http://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if host.starts_with('[') {
        host.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

fn error_message(status: reqwest::StatusCode, body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect());
    if msg.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {msg}")
    }
}

impl GpnApi {
    pub fn new(base_url: &str, token: &str, session: Option<AuthSession>, on_session: SessionSink) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: Mutex::new(normalize_url(base_url)),
            token: Mutex::new(token.to_string()),
            session: Mutex::new(session),
            peer_key: Mutex::new(None),
            refresh_lock: tokio::sync::Mutex::new(()),
            on_session,
        }
    }

    pub fn server_url(&self) -> String {
        self.base_url.lock().unwrap().clone()
    }
    pub fn set_server(&self, url: &str) {
        *self.base_url.lock().unwrap() = normalize_url(url);
    }
    pub fn set_token(&self, t: &str) {
        *self.token.lock().unwrap() = t.to_string();
    }
    pub fn set_peer_key(&self, k: Option<String>) {
        *self.peer_key.lock().unwrap() = k;
    }
    pub fn peer_key(&self) -> Option<String> {
        self.peer_key.lock().unwrap().clone()
    }
    pub fn has_credentials(&self) -> bool {
        self.session.lock().unwrap().is_some() || !self.token.lock().unwrap().is_empty()
    }
    pub fn auth_info(&self) -> Option<AuthInfo> {
        self.session.lock().unwrap().as_ref().map(|s| AuthInfo { username: s.username.clone(), role: s.role.clone() })
    }

    fn set_session(&self, s: Option<AuthSession>) {
        *self.session.lock().unwrap() = s.clone();
        (self.on_session)(s);
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.server_url(), path)
    }

    /// Authenticated request. Uses the user session when logged in (refreshing it when expired
    /// or on a 401), else the machine token. HTTP errors are returned as a response, only
    /// network failures/timeouts are `Err`.
    pub async fn request(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<reqwest::Response, String> {
        let expired = self.session.lock().unwrap().as_ref().filter(|s| now_ms() >= s.expires_at).map(|s| s.refresh_token.clone());
        if expired.is_some() {
            self.refresh(expired).await;
        }
        let (res, used) = self.send(&method, path, &body).await?;
        if res.status() == reqwest::StatusCode::UNAUTHORIZED && used.is_some() && self.refresh(used).await {
            return Ok(self.send(&method, path, &body).await?.0);
        }
        Ok(res)
    }

    /// Returns the response and the refresh token of the session that signed the request.
    async fn send(&self, method: &reqwest::Method, path: &str, body: &Option<Value>) -> Result<(reqwest::Response, Option<String>), String> {
        let session = self.session.lock().unwrap().clone();
        let mut req = self.http.request(method.clone(), self.url(path)).timeout(TIMEOUT);
        if let Some(s) = &session {
            req = req.bearer_auth(&s.access_token);
        } else {
            let t = self.token.lock().unwrap().clone();
            if !t.is_empty() {
                req = req.bearer_auth(t);
            }
        }
        if let Some(k) = self.peer_key() {
            req = req.header("x-gpn-peer", k);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = req.send().await.map_err(|e| format!("cannot reach server: {e}"))?;
        Ok((res, session.map(|s| s.refresh_token)))
    }

    pub async fn register_user(&self, username: &str, password: &str, invite: Option<&str>) -> Result<AuthInfo, String> {
        let mut body = json!({ "username": username, "password": password });
        if let Some(code) = invite.filter(|c| !c.is_empty()) {
            body["inviteCode"] = json!(code);
        }
        self.auth_flow("/api/auth/register", body).await
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<AuthInfo, String> {
        self.auth_flow("/api/auth/login", json!({ "username": username, "password": password })).await
    }

    async fn auth_flow(&self, path: &str, body: Value) -> Result<AuthInfo, String> {
        let res = self
            .http
            .post(self.url(path))
            .json(&body)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("cannot reach server: {e}"))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(error_message(status, &text));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| format!("bad server response: {e}"))?;
        let s = session_from(&data).ok_or("bad server response (missing tokens)")?;
        let info = AuthInfo { username: s.username.clone(), role: s.role.clone() };
        self.set_session(Some(s));
        Ok(info)
    }

    pub async fn logout(&self) {
        let s = self.session.lock().unwrap().clone();
        self.set_session(None);
        if let Some(s) = s {
            let _ = self
                .http
                .post(self.url("/api/auth/logout"))
                .bearer_auth(&s.access_token)
                .json(&json!({ "refreshToken": s.refresh_token }))
                .timeout(TIMEOUT)
                .send()
                .await;
        }
    }

    /// Single-flight refresh: tokens are one-time, so concurrent callers queue on the lock and
    /// the later ones notice the session was already renewed. `stale` is the refresh token the
    /// caller's failing request was signed with (None = refresh unconditionally).
    pub async fn refresh(&self, stale: Option<String>) -> bool {
        let _guard = self.refresh_lock.lock().await;
        let Some(s) = self.session.lock().unwrap().clone() else { return false };
        if stale.is_some_and(|t| t != s.refresh_token) {
            return true; // someone else already refreshed
        }
        let res = match self
            .http
            .post(self.url("/api/auth/refresh"))
            .json(&json!({ "refreshToken": s.refresh_token }))
            .timeout(TIMEOUT)
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return false,
        };
        let status = res.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            // definitively rejected -> logged out. Transient errors (5xx, 429, network) keep the session.
            let same = self.session.lock().unwrap().as_ref().is_some_and(|c| c.refresh_token == s.refresh_token);
            if same {
                self.set_session(None);
            }
            return false;
        }
        if !status.is_success() {
            return false;
        }
        let Ok(data) = res.json::<Value>().await else { return false };
        let (Some(access), Some(refresh)) = (data["accessToken"].as_str(), data["refreshToken"].as_str()) else { return false };
        let same = self.session.lock().unwrap().as_ref().is_some_and(|c| c.refresh_token == s.refresh_token);
        if !same {
            return false; // logged out / replaced meanwhile
        }
        self.set_session(Some(AuthSession {
            access_token: access.to_string(),
            refresh_token: refresh.to_string(),
            expires_at: now_ms() + SESSION_TTL_MS,
            ..s
        }));
        true
    }

    /// Push the current set of public game IPs for this client's peer (server accepts /32 hosts only).
    pub async fn push_targets(&self, ips: &[String]) -> Result<Value, String> {
        if self.peer_key().is_none() {
            return Err("not registered yet".into());
        }
        let body = json!({ "targets": ips.iter().map(|ip| json!({ "ip": ip })).collect::<Vec<_>>(), "ts": now_ms() });
        let res = self.request(reqwest::Method::POST, "/api/targets", Some(body)).await?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(error_message(status, &text));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// POST /api/register for the given WG public key.
    pub async fn register_peer(&self, public_key: &str, name: &str) -> Result<Value, String> {
        let res = self
            .request(reqwest::Method::POST, "/api/register", Some(json!({ "publicKey": public_key, "name": name })))
            .await?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(error_message(status, &text));
        }
        serde_json::from_str(&text).map_err(|e| format!("bad server response: {e}"))
    }

    /// The server's packet-duplication relay (host:port), if it runs one.
    pub async fn dup_endpoint(&self) -> Result<Option<String>, String> {
        let res = self.http.get(self.url("/api/health")).timeout(TIMEOUT).send().await.map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("HTTP {}", res.status()));
        }
        let v: Value = res.json().await.map_err(|e| e.to_string())?;
        Ok(v["dup"].as_str().map(str::to_string))
    }

    pub async fn ping(&self) -> Result<String, String> {
        let res = self.http.get(self.url("/api/health")).timeout(TIMEOUT).send().await.map_err(|e| e.to_string())?;
        if res.status().is_success() {
            Ok("ok".into())
        } else {
            Err(format!("HTTP {}", res.status()))
        }
    }
}

fn session_from(data: &Value) -> Option<AuthSession> {
    Some(AuthSession {
        username: data["user"]["username"].as_str()?.to_string(),
        role: data["user"]["role"].as_str()?.to_string(),
        status: data["user"]["status"].as_str().map(str::to_string),
        access_token: data["accessToken"].as_str()?.to_string(),
        refresh_token: data["refreshToken"].as_str()?.to_string(),
        expires_at: now_ms() + SESSION_TTL_MS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_urls() {
        assert!(is_secure_or_local("https://gpn.example.com"));
        assert!(is_secure_or_local("http://localhost:9090"));
        assert!(is_secure_or_local("http://127.0.0.1:9090/x"));
        assert!(is_secure_or_local("http://[::1]:9090"));
        assert!(!is_secure_or_local("http://gpn.example.com"));
        assert!(!is_secure_or_local("http://localhost.evil.com"));
        assert!(!is_secure_or_local("ftp://x"));
    }

    #[test]
    fn error_messages() {
        let st = reqwest::StatusCode::BAD_REQUEST;
        assert_eq!(error_message(st, r#"{"error":"nope"}"#), "HTTP 400 Bad Request: nope");
        assert_eq!(error_message(st, ""), "HTTP 400 Bad Request");
    }

    #[test]
    fn parses_session() {
        let v = json!({"user":{"username":"a","role":"user","status":"active"},"accessToken":"x","refreshToken":"y"});
        let s = session_from(&v).unwrap();
        assert_eq!((s.username.as_str(), s.refresh_token.as_str()), ("a", "y"));
        assert!(session_from(&json!({"user":{}})).is_none());
    }
}
