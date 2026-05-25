use crate::context::AppContext;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub recording: RecordingConfig,
    pub model: ModelConfig,
    pub paste: PasteConfig,
    pub prompts: BTreeMap<String, String>,
    pub app_rules: Vec<AppRule>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config at {path:?}"))?;
        Ok(toml::from_str(&text).with_context(|| format!("failed to parse config at {path:?}"))?)
    }

    pub fn prompt_name_for_context(&self, context: &AppContext) -> Option<&str> {
        self.app_rule_for_context(context)
            .and_then(|rule| rule.prompt.as_deref())
    }

    pub fn app_rule_for_context(&self, context: &AppContext) -> Option<&AppRule> {
        self.app_rules.iter().find(|rule| rule.matches(context))
    }

    pub fn validate(&self) -> Result<()> {
        if self.recording.max_seconds == 0 {
            anyhow::bail!("recording.max_seconds must be greater than 0");
        }
        if self.recording.sample_rate == 0 {
            anyhow::bail!("recording.sample_rate must be greater than 0");
        }
        if self.recording.channels == 0 {
            anyhow::bail!("recording.channels must be greater than 0");
        }
        if self.model.model.trim().is_empty() {
            anyhow::bail!("model.model must not be empty");
        }
        if self.model.timeout_seconds == 0 {
            anyhow::bail!("model.timeout_seconds must be greater than 0");
        }

        let url = reqwest::Url::parse(&self.model.url)
            .with_context(|| format!("invalid model.url: {}", self.model.url))?;
        match url.scheme() {
            "http" | "https" => {}
            scheme => anyhow::bail!("model.url must use http or https, got {scheme}"),
        }

        for (index, rule) in self.app_rules.iter().enumerate() {
            if let Some(prompt_name) = rule.prompt.as_deref() {
                if prompt_name != "default" && !self.prompts.contains_key(prompt_name) {
                    anyhow::bail!(
                        "app_rules[{index}].prompt references missing prompt {prompt_name:?}"
                    );
                }
            }
        }

        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("ut").join("config.toml");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("ut")
            .join("config.toml");
    }

    PathBuf::from("ut.toml")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingConfig {
    pub max_seconds: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub trim_silence: bool,
    pub trim_padding_ms: u32,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            max_seconds: 29,
            sample_rate: 16_000,
            channels: 1,
            trim_silence: true,
            trim_padding_ms: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub url: String,
    pub model: String,
    pub timeout_seconds: u64,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:11434/v1".to_string(),
            model: "unsloth/gemma-4-E2B-it-GGUF:Q4_K_XL".to_string(),
            timeout_seconds: 60,
            api_key: None,
            api_key_env: None,
        }
    }
}

impl ModelConfig {
    pub fn resolved_api_key(&self) -> Option<String> {
        self.api_key
            .as_ref()
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| {
                self.api_key_env
                    .as_deref()
                    .and_then(|name| env::var(name).ok())
                    .filter(|value| !value.is_empty())
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PasteConfig {
    pub method: PasteMethod,
    pub restore_clipboard: bool,
    pub restore_delay_ms: u64,
    pub on_focus_changed: FocusMismatchAction,
}

impl Default for PasteConfig {
    fn default() -> Self {
        Self {
            method: PasteMethod::Clipboard,
            restore_clipboard: true,
            restore_delay_ms: 100,
            on_focus_changed: FocusMismatchAction::Copy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PasteMethod {
    #[default]
    Clipboard,
    Wtype,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FocusMismatchAction {
    #[default]
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppRule {
    pub app_id: Option<String>,
    pub class: Option<String>,
    pub title_contains: Option<String>,
    pub prompt: Option<String>,
    pub paste_keys: Option<String>,
}

impl AppRule {
    pub fn matches(&self, context: &AppContext) -> bool {
        let mut matched_any_field = false;

        if let Some(app_id) = self.app_id.as_deref() {
            matched_any_field = true;
            if context.app_id.as_deref() != Some(app_id) {
                return false;
            }
        }

        if let Some(class) = self.class.as_deref() {
            matched_any_field = true;
            if context.class.as_deref() != Some(class) {
                return false;
            }
        }

        if let Some(title_contains) = self.title_contains.as_deref() {
            matched_any_field = true;
            let Some(title) = context.title.as_deref() else {
                return false;
            };
            if !title.contains(title_contains) {
                return false;
            }
        }

        matched_any_field
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ENV_KEY_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn unique_env_key(prefix: &str) -> String {
        let id = ENV_KEY_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("UT_{prefix}_{}_{}", std::process::id(), id)
    }

    #[test]
    fn defaults_match_mvp_shape() {
        let config = Config::default();
        assert_eq!(config.recording.sample_rate, 16_000);
        assert!(config.paste.restore_clipboard);
        assert!(config.prompts.is_empty());
        assert_eq!(config.model.url, "http://127.0.0.1:11434/v1");
        assert_eq!(config.model.api_key, None);
        assert_eq!(config.model.api_key_env, None);
    }

    #[test]
    fn validate_rejects_invalid_model_url() {
        let mut config = Config::default();
        config.model.url = "notaurl".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_timeout() {
        let mut config = Config::default();
        config.model.timeout_seconds = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_missing_app_rule_prompt_reference() {
        let mut config = Config::default();
        config.app_rules = vec![AppRule {
            app_id: Some("kitty".to_string()),
            class: None,
            title_contains: None,
            prompt: Some("terminal".to_string()),
            paste_keys: None,
        }];

        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_allows_existing_app_rule_prompt_reference() {
        let mut config = Config::default();
        config
            .prompts
            .insert("terminal".to_string(), "terminal prompt".to_string());
        config.app_rules = vec![AppRule {
            app_id: Some("kitty".to_string()),
            class: None,
            title_contains: None,
            prompt: Some("terminal".to_string()),
            paste_keys: None,
        }];

        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_allows_builtin_default_prompt_reference() {
        let mut config = Config::default();
        config.app_rules = vec![AppRule {
            app_id: Some("kitty".to_string()),
            class: None,
            title_contains: None,
            prompt: Some("default".to_string()),
            paste_keys: None,
        }];

        assert!(config.validate().is_ok());
    }

    #[test]
    fn model_config_prefers_static_api_key_over_env() {
        let env_key = unique_env_key("MODEL_API_KEY");
        unsafe {
            env::set_var(&env_key, "env-token");
        }

        let config = ModelConfig {
            api_key: Some("static-token".to_string()),
            api_key_env: Some(env_key.clone()),
            ..ModelConfig::default()
        };

        assert_eq!(config.resolved_api_key().as_deref(), Some("static-token"));

        unsafe {
            env::remove_var(env_key);
        }
    }

    #[test]
    fn model_config_reads_api_key_env_at_call_time() {
        let env_key = unique_env_key("MODEL_API_KEY_ENV");
        let config = ModelConfig {
            api_key: None,
            api_key_env: Some(env_key.clone()),
            ..ModelConfig::default()
        };

        assert_eq!(config.resolved_api_key(), None);

        unsafe {
            env::set_var(&env_key, "first-token");
        }
        assert_eq!(config.resolved_api_key().as_deref(), Some("first-token"));

        unsafe {
            env::set_var(&env_key, "second-token");
        }
        assert_eq!(config.resolved_api_key().as_deref(), Some("second-token"));

        unsafe {
            env::remove_var(env_key);
        }
    }

    #[test]
    fn app_rules_pick_the_first_matching_prompt_name() {
        let mut config = Config::default();
        config.app_rules = vec![
            AppRule {
                app_id: Some("kitty".to_string()),
                class: None,
                title_contains: None,
                prompt: Some("terminal".to_string()),
                paste_keys: Some("ctrl+shift+v".to_string()),
            },
            AppRule {
                app_id: None,
                class: Some("code".to_string()),
                title_contains: None,
                prompt: Some("code".to_string()),
                paste_keys: None,
            },
        ];

        let mut context = AppContext::default();
        context.app_id = Some("kitty".to_string());
        context.class = Some("code".to_string());

        assert_eq!(config.prompt_name_for_context(&context), Some("terminal"));
        assert_eq!(
            config
                .app_rule_for_context(&context)
                .and_then(|rule| rule.paste_keys.as_deref()),
            Some("ctrl+shift+v")
        );
    }

    #[test]
    fn app_rule_requires_at_least_one_matchable_field() {
        let rule = AppRule::default();
        assert!(!rule.matches(&AppContext::default()));
    }

    #[test]
    fn app_rule_matches_title_substrings() {
        let rule = AppRule {
            app_id: None,
            class: None,
            title_contains: Some("ChatGPT".to_string()),
            prompt: Some("chat".to_string()),
            paste_keys: None,
        };
        let mut context = AppContext::default();
        context.title = Some("Open ChatGPT - Firefox".to_string());

        assert!(rule.matches(&context));
    }
}
