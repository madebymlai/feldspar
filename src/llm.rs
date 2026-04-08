use crate::config::LlmConfig;
use serde_json::Value;
use std::time::Duration;

pub struct LlmClient {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
}

impl LlmClient {
    pub fn new(config: &LlmConfig) -> Option<Self> {
        let api_key = config
            .api_key_env
            .as_deref()
            .and_then(|env_name| std::env::var(env_name).ok());

        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://openrouter.ai/api/v1".into())
            .trim_end_matches('/')
            .to_owned();

        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("failed to build LLM HTTP client: {}", e);
                return None;
            }
        };

        Some(Self {
            client,
            base_url,
            api_key,
            model: config.model.clone(),
        })
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    pub async fn chat_json(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Option<Value> {
        let url = format!("{}/chat/completions", self.base_url);

        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json");

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "max_completion_tokens": max_tokens,
            "response_format": {"type": "json_object"}
        });

        let resp = match req.json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("LLM request failed: {}", e);
                return None;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!("LLM returned HTTP {}", resp.status());
            return None;
        }

        let json: Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("LLM response parse failed: {}", e);
                return None;
            }
        };

        let content = match json["choices"][0]["message"]["content"].as_str() {
            Some(c) => c,
            None => {
                tracing::warn!("LLM response missing content field");
                return None;
            }
        };

        match serde_json::from_str(content) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("LLM content JSON parse failed: {}", e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LlmConfig;

    fn local_config(base_url: Option<&str>) -> LlmConfig {
        LlmConfig {
            base_url: base_url.map(|s| s.into()),
            api_key_env: None,
            model: "test".into(),
        }
    }

    #[test]
    fn test_llm_client_constructs() {
        let config = LlmConfig {
            base_url: Some("http://localhost:11434/v1".into()),
            api_key_env: None,
            model: "test".into(),
        };
        let client = LlmClient::new(&config);
        assert!(client.is_some());
    }

    #[test]
    fn test_base_url_trailing_slash_trimmed() {
        let config = local_config(Some("http://localhost/v1/"));
        let client = LlmClient::new(&config).expect("client should build");
        assert_eq!(client.base_url, "http://localhost/v1");
    }

    #[test]
    fn test_base_url_defaults_to_openrouter() {
        let config = local_config(None);
        let client = LlmClient::new(&config).expect("client should build");
        assert_eq!(client.base_url, "https://openrouter.ai/api/v1");
    }

    #[test]
    fn test_has_api_key_true() {
        unsafe { std::env::set_var("TEST_HAS_KEY", "somevalue"); }
        let config = LlmConfig {
            base_url: None,
            api_key_env: Some("TEST_HAS_KEY".into()),
            model: "test".into(),
        };
        let client = LlmClient::new(&config).unwrap();
        assert!(client.has_api_key());
        unsafe { std::env::remove_var("TEST_HAS_KEY"); }
    }

    #[test]
    fn test_has_api_key_false() {
        let config = LlmConfig {
            base_url: None,
            api_key_env: Some("NONEXISTENT_VAR_12345".into()),
            model: "test".into(),
        };
        let client = LlmClient::new(&config).unwrap();
        assert!(!client.has_api_key());
    }

    #[test]
    fn test_has_api_key_none_env() {
        let config = LlmConfig {
            base_url: None,
            api_key_env: None,
            model: "test".into(),
        };
        let client = LlmClient::new(&config).unwrap();
        assert!(!client.has_api_key());
    }
}
