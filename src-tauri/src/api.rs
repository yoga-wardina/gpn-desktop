//! GPN VPS API client.

use serde::Serialize;

use crate::watcher::Target;

#[derive(Clone)]
pub struct GpnApi {
    pub server_url: String,
    pub token: String,
}

#[derive(Serialize)]
struct TargetPush<'a> {
    targets: &'a [Target],
    ts: u64,
}

impl GpnApi {
    pub fn new(server_url: String, token: String) -> Self {
        Self { server_url, token }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        reqwest::Client::new()
            .request(method, format!("{}{}", self.server_url.trim_end_matches('/'), path))
            .header("authorization", format!("Bearer {}", self.token))
            .timeout(std::time::Duration::from_secs(8))
    }

    pub async fn push_targets(&self, targets: &[Target]) -> Result<(), String> {
        let body = TargetPush {
            targets,
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };
        let res = self
            .req(reqwest::Method::POST, "/api/targets")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {}", res.status()))
        }
    }

    pub async fn ping(&self) -> Result<String, String> {
        let res = self
            .req(reqwest::Method::GET, "/api/health")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if res.status().is_success() {
            Ok("ok".into())
        } else {
            Err(format!("HTTP {}", res.status()))
        }
    }
}
