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
    secret: Option<String>,
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
        secret: Option<String>,
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
        let url = self.webhook_url()?;
        let response = self.client.post(url).json(&json!({"msgtype":"markdown","markdown":{"title":"alertd","text":text},"at":{"isAtAll": critical && self.at_all_on_critical}})).send()?.error_for_status()?;
        let value: serde_json::Value = response.json()?;
        if value.get("errcode").and_then(|v| v.as_i64()) != Some(0) {
            return Err(DingTalkError::Rejected(value.to_string()));
        }
        Ok(())
    }

    fn webhook_url(&self) -> Result<String, DingTalkError> {
        let mut url = format!(
            "https://oapi.dingtalk.com/robot/send?access_token={}",
            urlencoding::encode(&self.token)
        );
        let Some(secret) = &self.secret else {
            return Ok(url);
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| DingTalkError::Clock)?
            .as_millis();
        let signature = sign(timestamp, secret);
        url.push_str(&format!(
            "&timestamp={timestamp}&sign={}",
            urlencoding::encode(&signature)
        ));
        Ok(url)
    }
}

pub fn sign(timestamp_ms: u128, secret: &str) -> String {
    let content = format!("{timestamp_ms}\n{secret}");
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(content.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_whitelist_mode_omits_timestamp_and_signature() {
        let client =
            DingTalkClient::new("token value".into(), None, Duration::from_secs(1), false).unwrap();

        let url = client.webhook_url().unwrap();

        assert_eq!(
            url,
            "https://oapi.dingtalk.com/robot/send?access_token=token%20value"
        );
    }

    #[test]
    fn signing_mode_includes_timestamp_and_signature() {
        let client = DingTalkClient::new(
            "token".into(),
            Some("secret".into()),
            Duration::from_secs(1),
            false,
        )
        .unwrap();

        let url = client.webhook_url().unwrap();

        assert!(url.contains("&timestamp="));
        assert!(url.contains("&sign="));
    }
}
