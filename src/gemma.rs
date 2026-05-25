use crate::audio::{encode_wav_bytes, AudioPayload};
use crate::config::ModelConfig;
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::env;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct DictationRequest {
    pub audio: AudioPayload,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationResponse {
    pub text: String,
}

#[async_trait]
pub trait GemmaClient: Send + Sync {
    async fn dictate(&self, request: DictationRequest) -> Result<DictationResponse>;
}

#[derive(Debug, Clone)]
pub struct HttpGemmaClient {
    endpoint: String,
    model: String,
    timeout: Duration,
    api_key: Option<String>,
    api_key_env: Option<String>,
}

impl HttpGemmaClient {
    pub fn new(config: &ModelConfig) -> Self {
        Self {
            endpoint: chat_completions_endpoint(&config.url),
            model: config.model.clone(),
            timeout: Duration::from_secs(config.timeout_seconds.max(1)),
            api_key: config.api_key.clone(),
            api_key_env: config.api_key_env.clone(),
        }
    }

    async fn dictate_async(&self, request: DictationRequest) -> Result<DictationResponse> {
        let wav_bytes = encode_wav_bytes(&request.audio.samples, request.audio.sample_rate);
        let body = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage::system(request.prompt),
                ChatMessage::user(vec![
                    ContentPart::text("Transcribe the audio and return only the insertable text."),
                    ContentPart::input_audio(base64_encode(&wav_bytes), "wav"),
                ]),
            ],
            temperature: Some(0.0),
        };

        let request_body = serde_json::to_vec(&body)?;
        let response = send_http_request(
            &self.endpoint,
            &request_body,
            self.resolve_api_key(),
            self.timeout,
        )
        .await?;
        let completion: ChatCompletionResponse =
            serde_json::from_slice(&response).context("failed to parse chat completion")?;
        extract_text(&completion).map(|text| DictationResponse { text })
    }

    fn resolve_api_key(&self) -> Option<String> {
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

#[async_trait]
impl GemmaClient for HttpGemmaClient {
    async fn dictate(&self, request: DictationRequest) -> Result<DictationResponse> {
        self.dictate_async(request).await
    }
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: ChatContent,
}

impl ChatMessage {
    fn system(content: String) -> Self {
        Self {
            role: "system".to_string(),
            content: ChatContent::Text(content),
        }
    }

    fn user(parts: Vec<ContentPart>) -> Self {
        Self {
            role: "user".to_string(),
            content: ChatContent::Parts(parts),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ChatContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Serialize)]
struct ContentPart {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_audio: Option<InputAudioPart>,
}

impl ContentPart {
    fn text(text: impl Into<String>) -> Self {
        Self {
            kind: "text",
            text: Some(text.into()),
            input_audio: None,
        }
    }

    fn input_audio(data: String, format: &'static str) -> Self {
        Self {
            kind: "input_audio",
            text: None,
            input_audio: Some(InputAudioPart { data, format }),
        }
    }
}

#[derive(Debug, Serialize)]
struct InputAudioPart {
    data: String,
    format: &'static str,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessageResponse,
}

#[derive(Debug, Deserialize)]
struct ChatMessageResponse {
    content: Option<Value>,
}

fn extract_text(response: &ChatCompletionResponse) -> Result<String> {
    let choice = response
        .choices
        .first()
        .context("chat completion returned no choices")?;
    let content = choice
        .message
        .content
        .as_ref()
        .context("chat completion returned empty content")?;

    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        other => anyhow::bail!("unsupported chat completion content: {other}"),
    };

    let text = text.trim().to_string();
    if text.is_empty() {
        anyhow::bail!("Gemma returned empty dictation text");
    }
    Ok(text)
}

async fn send_http_request(
    url: &str,
    body: &[u8],
    api_key: Option<String>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .context("failed to build HTTP client")?;
    let mut request = client
        .post(url)
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_vec());

    if let Some(api_key) = api_key {
        request = request.header(AUTHORIZATION, format!("Bearer {api_key}"));
    }

    let response = request
        .send()
        .await
        .context("failed to send HTTP request")?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .context("failed to read HTTP response body")?;

    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        anyhow::bail!(
            "Gemma HTTP request failed with status {}: {text}",
            status.as_u16()
        );
    }

    Ok(bytes.to_vec())
}

fn chat_completions_endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        let triple = ((a as u32) << 16) | ((b as u32) << 8) | c as u32;

        output.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        output.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioPayload;
    use crate::config::Config;
    use crate::context::AppContext;
    use crate::prompt::build_prompt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ENV_KEY_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn unique_env_key(prefix: &str) -> String {
        let id = ENV_KEY_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("UT_{prefix}_{}_{}", std::process::id(), id)
    }

    #[test]
    fn parses_text_content() {
        let response = ChatCompletionResponse {
            choices: vec![ChatChoice {
                message: ChatMessageResponse {
                    content: Some(Value::String("hello".to_string())),
                },
            }],
        };

        assert_eq!(extract_text(&response).unwrap(), "hello");
    }

    #[test]
    fn base64_encodes_bytes() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
    }

    #[test]
    fn prompt_in_request_uses_configured_text() {
        let config = Config::default();
        let prompt = build_prompt(&AppContext::default(), &config);
        let request = DictationRequest {
            audio: AudioPayload::new(16_000, 1, vec![0.0]),
            prompt,
        };
        assert!(request.prompt.contains("You are a dictation engine."));
    }

    #[test]
    fn chat_completions_endpoint_is_built_from_base_v1_url() {
        assert_eq!(
            chat_completions_endpoint("http://127.0.0.1:11434/v1"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("http://127.0.0.1:11434/v1/"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn explicit_api_key_takes_precedence_over_environment_key() {
        let env_key = unique_env_key("GEMMA_API_KEY");
        let client = HttpGemmaClient {
            endpoint: "http://127.0.0.1:11434/v1/chat/completions".to_string(),
            model: "model".to_string(),
            timeout: Duration::from_secs(1),
            api_key: Some("inline-secret".to_string()),
            api_key_env: Some(env_key.clone()),
        };

        unsafe {
            env::set_var(&env_key, "env-secret");
        }
        assert_eq!(client.resolve_api_key().as_deref(), Some("inline-secret"));
        unsafe {
            env::remove_var(env_key);
        }
    }

    #[test]
    fn api_key_can_be_loaded_from_environment() {
        let env_key = unique_env_key("GEMMA_API_KEY_ENV");
        let client = HttpGemmaClient {
            endpoint: "http://127.0.0.1:11434/v1/chat/completions".to_string(),
            model: "model".to_string(),
            timeout: Duration::from_secs(1),
            api_key: None,
            api_key_env: Some(env_key.clone()),
        };

        unsafe {
            env::set_var(&env_key, "env-secret");
        }
        assert_eq!(client.resolve_api_key().as_deref(), Some("env-secret"));
        unsafe {
            env::remove_var(env_key);
        }
    }
}
