use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use reqwest::blocking::Client;
use serde_json::json;
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct DingTalkClient {
    client: Client,
    token: String,
    secret: String,
    at_all_on_critical: bool,
}

#[derive(Debug, Error)]
pub enum DingTalkError {
    #[error("HTTP client error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("DingTalk rejected message: {0}")]
    Rejected(String),
    #[error("system clock is before Unix epoch")]
    Clock,
}

impl DingTalkClient {
    pub fn new(
        token: String,
        secret: String,
        timeout: Duration,
        at_all_on_critical: bool,
    ) -> Result<Self, DingTalkError> {
        Ok(Self {
            client: Client::builder().timeout(timeout).build()?,
            token,
            secret,
            at_all_on_critical,
        })
    }

    pub fn send(&self, text: &str, critical: bool) -> Result<(), DingTalkError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| DingTalkError::Clock)?
            .as_millis();
        let sign = sign(timestamp, &self.secret);
        let url = format!(
            "https://oapi.dingtalk.com/robot/send?access_token={}&timestamp={timestamp}&sign={}",
            urlencoding::encode(&self.token),
            urlencoding::encode(&sign)
        );
        let response = self.client.post(url).json(&json!({"msgtype":"markdown","markdown":{"title":"alertd","text":text},"at":{"isAtAll": critical && self.at_all_on_critical}})).send()?.error_for_status()?;
        let value: serde_json::Value = response.json()?;
        if value.get("errcode").and_then(|v| v.as_i64()) != Some(0) {
            return Err(DingTalkError::Rejected(value.to_string()));
        }
        Ok(())
    }
}

pub fn sign(timestamp_ms: u128, secret: &str) -> String {
    let content = format!("{timestamp_ms}\n{secret}");
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(content.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}
