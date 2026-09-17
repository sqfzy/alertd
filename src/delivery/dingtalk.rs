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
        let url = endpoint(&self.token, self.secret.as_deref(), SystemTime::now())?;
        let response = self.client.post(url).json(&json!({"msgtype":"markdown","markdown":{"title":"alertd","text":text},"at":{"isAtAll": critical && self.at_all_on_critical}})).send()?.error_for_status()?;
        let value: serde_json::Value = response.json()?;
        if value.get("errcode").and_then(|v| v.as_i64()) != Some(0) {
            return Err(DingTalkError::Rejected(value.to_string()));
        }
        Ok(())
    }
}

fn endpoint(token: &str, secret: Option<&str>, now: SystemTime) -> Result<String, DingTalkError> {
    let base = format!(
        "https://oapi.dingtalk.com/robot/send?access_token={}",
        urlencoding::encode(token)
    );
    let Some(secret) = secret else {
        return Ok(base);
    };
    let timestamp = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| DingTalkError::Clock)?
        .as_millis();
    let signature = sign(timestamp, secret);
    Ok(format!(
        "{base}&timestamp={timestamp}&sign={}",
        urlencoding::encode(&signature)
    ))
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
    fn unsigned_endpoint_contains_only_the_token() {
        let url = endpoint("a+b", None, UNIX_EPOCH).unwrap();
        assert_eq!(
            url,
            "https://oapi.dingtalk.com/robot/send?access_token=a%2Bb"
        );
        assert!(!url.contains("timestamp="));
        assert!(!url.contains("sign="));
    }

    #[test]
    fn signed_endpoint_keeps_the_existing_signature_contract() {
        let url = endpoint("token", Some("secret"), UNIX_EPOCH).unwrap();
        assert!(url.contains("access_token=token&timestamp=0&sign="));
    }
}
