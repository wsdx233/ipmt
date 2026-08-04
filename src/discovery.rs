use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::Value;
use thiserror::Error;
use url::Url;

const MAX_CATALOG_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: Option<String>,
    pub config: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct DiscoveryRequest {
    provider_id: String,
    config_dir: PathBuf,
    base_url: String,
    api: String,
    provider_api_key: Option<String>,
    auth_header: bool,
    headers: Vec<(String, String)>,
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("provider {0} is not an object")]
    InvalidProvider(String),
    #[error("provider {0} has no baseUrl")]
    MissingBaseUrl(String),
    #[error("provider {0} has no API type")]
    MissingApi(String),
    #[error("invalid baseUrl: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("cannot read auth.json: {0}")]
    AuthRead(std::io::Error),
    #[error("invalid auth.json: {0}")]
    AuthJson(serde_json::Error),
    #[error("credential commands are not executed during model discovery")]
    CommandCredential,
    #[error("environment variable {0} is not set")]
    MissingEnvironment(String),
    #[error("invalid header name {0}")]
    InvalidHeaderName(String),
    #[error("invalid value for header {0}")]
    InvalidHeaderValue(String),
    #[error("request failed: {0}")]
    Request(reqwest::Error),
    #[error("server returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("model catalog is larger than 10 MiB")]
    TooLarge,
    #[error("model endpoint returned invalid JSON: {0}")]
    ResponseJson(serde_json::Error),
    #[error("response does not contain a supported model list")]
    UnsupportedResponse,
    #[error("the model endpoint returned an empty catalog")]
    EmptyCatalog,
}

#[derive(Debug)]
struct CredentialSource {
    value: String,
    scoped_env: HashMap<String, String>,
}

#[derive(Debug)]
struct ResolvedAuth {
    key: Option<String>,
    scoped_env: HashMap<String, String>,
}

impl DiscoveryRequest {
    pub fn from_provider(
        provider_id: impl Into<String>,
        provider: &Value,
        config_dir: impl Into<PathBuf>,
    ) -> Result<Self, DiscoveryError> {
        let provider_id = provider_id.into();
        let object = provider
            .as_object()
            .ok_or_else(|| DiscoveryError::InvalidProvider(provider_id.clone()))?;
        let base_url = object
            .get("baseUrl")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| DiscoveryError::MissingBaseUrl(provider_id.clone()))?
            .to_owned();
        let api = object
            .get("api")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| DiscoveryError::MissingApi(provider_id.clone()))?
            .to_owned();
        let headers = object
            .get("headers")
            .and_then(Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(name, value)| {
                        value.as_str().map(|value| (name.clone(), value.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            provider_id,
            config_dir: config_dir.into(),
            base_url,
            api,
            provider_api_key: object
                .get("apiKey")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            auth_header: object
                .get("authHeader")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            headers,
        })
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn endpoint(&self) -> Result<Url, DiscoveryError> {
        model_endpoint(&self.base_url, &self.api)
    }
}

pub fn discover_models(request: &DiscoveryRequest) -> Result<Vec<DiscoveredModel>, DiscoveryError> {
    let endpoint = request.endpoint()?;
    let auth = resolve_auth(request)?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("ipmt/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(DiscoveryError::Request)?;

    let mut builder = client.get(endpoint);
    let mut explicit_headers = HashSet::new();
    let mut secrets = auth.key.iter().cloned().collect::<Vec<_>>();
    for (name, raw_value) in &request.headers {
        let normalized = name.to_ascii_lowercase();
        let value = resolve_config_value(raw_value, &auth.scoped_env)?;
        if value.len() >= 6 {
            secrets.push(value.clone());
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| DiscoveryError::InvalidHeaderName(name.clone()))?;
        let header_value = HeaderValue::from_str(&value)
            .map_err(|_| DiscoveryError::InvalidHeaderValue(name.clone()))?;
        explicit_headers.insert(normalized);
        builder = builder.header(header_name, header_value);
    }
    builder = add_auth_headers(builder, request, auth.key, &explicit_headers)?;

    let response = builder
        .send()
        .map_err(|error| DiscoveryError::Request(error.without_url()))?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
    {
        return Err(DiscoveryError::TooLarge);
    }
    let bytes = response
        .bytes()
        .map_err(|error| DiscoveryError::Request(error.without_url()))?;
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(DiscoveryError::TooLarge);
    }
    if !status.is_success() {
        return Err(DiscoveryError::Http {
            status: status.as_u16(),
            message: safe_server_message(&bytes, &secrets),
        });
    }
    let payload: Value = serde_json::from_slice(&bytes).map_err(DiscoveryError::ResponseJson)?;
    parse_catalog(&payload)
}

fn add_auth_headers(
    mut builder: RequestBuilder,
    request: &DiscoveryRequest,
    credential: Option<String>,
    explicit: &HashSet<String>,
) -> Result<RequestBuilder, DiscoveryError> {
    let Some(key) = credential else {
        return Ok(builder);
    };
    let value = HeaderValue::from_str(&key)
        .map_err(|_| DiscoveryError::InvalidHeaderValue("credential".into()))?;
    if request.auth_header {
        if !explicit.contains("authorization") {
            builder = builder.bearer_auth(key);
        }
    } else {
        match request.api.as_str() {
            "anthropic-messages" => {
                if !explicit.contains("x-api-key") {
                    builder = builder.header("x-api-key", value);
                }
                if !explicit.contains("anthropic-version") {
                    builder = builder.header("anthropic-version", "2023-06-01");
                }
            }
            "google-generative-ai" => {
                if !explicit.contains("x-goog-api-key") {
                    builder = builder.header("x-goog-api-key", value);
                }
            }
            _ => {
                if !explicit.contains("authorization") {
                    builder = builder.bearer_auth(key);
                }
            }
        }
    }
    Ok(builder)
}

fn resolve_auth(request: &DiscoveryRequest) -> Result<ResolvedAuth, DiscoveryError> {
    if let Some(source) = read_auth_credential(&request.config_dir, &request.provider_id)? {
        let key = resolve_config_value(&source.value, &source.scoped_env)?;
        return Ok(ResolvedAuth {
            key: Some(key),
            scoped_env: source.scoped_env,
        });
    }
    if let Some(variable) = provider_environment_variable(&request.provider_id)
        && let Ok(value) = std::env::var(variable)
        && !value.is_empty()
    {
        return Ok(ResolvedAuth {
            key: Some(value),
            scoped_env: HashMap::new(),
        });
    }
    let key = request
        .provider_api_key
        .as_deref()
        .map(|value| resolve_config_value(value, &HashMap::new()))
        .transpose()?;
    Ok(ResolvedAuth {
        key,
        scoped_env: HashMap::new(),
    })
}

fn read_auth_credential(
    config_dir: &Path,
    provider_id: &str,
) -> Result<Option<CredentialSource>, DiscoveryError> {
    let path = config_dir.join("auth.json");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DiscoveryError::AuthRead(error)),
    };
    let root: Value = serde_json::from_slice(&bytes).map_err(DiscoveryError::AuthJson)?;
    let Some(entry) = root.get(provider_id).and_then(Value::as_object) else {
        return Ok(None);
    };
    if entry.get("type").and_then(Value::as_str) != Some("api_key") {
        return Ok(None);
    }
    let Some(value) = entry.get("key").and_then(Value::as_str) else {
        return Ok(None);
    };
    let scoped_env = entry
        .get("env")
        .and_then(Value::as_object)
        .map(|env| {
            env.iter()
                .filter_map(|(name, value)| {
                    value.as_str().map(|value| (name.clone(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(CredentialSource {
        value: value.to_owned(),
        scoped_env,
    }))
}

fn resolve_config_value(
    raw: &str,
    scoped_env: &HashMap<String, String>,
) -> Result<String, DiscoveryError> {
    if raw.starts_with('!') {
        return Err(DiscoveryError::CommandCredential);
    }
    let bytes = raw.as_bytes();
    let mut output = String::with_capacity(raw.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            let character = raw[index..].chars().next().expect("valid UTF-8 slice");
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        if bytes.get(index + 1) == Some(&b'$') {
            output.push('$');
            index += 2;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'!') {
            output.push('!');
            index += 2;
            continue;
        }
        let (name, next) = if bytes.get(index + 1) == Some(&b'{') {
            let Some(relative_end) = raw[index + 2..].find('}') else {
                output.push('$');
                index += 1;
                continue;
            };
            let end = index + 2 + relative_end;
            (&raw[index + 2..end], end + 1)
        } else {
            let start = index + 1;
            let mut end = start;
            while let Some(byte) = bytes.get(end)
                && (*byte == b'_' || byte.is_ascii_alphanumeric())
            {
                end += 1;
            }
            if end == start {
                output.push('$');
                index += 1;
                continue;
            }
            (&raw[start..end], end)
        };
        let value = scoped_env
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
            .ok_or_else(|| DiscoveryError::MissingEnvironment(name.to_owned()))?;
        output.push_str(&value);
        index = next;
    }
    Ok(output)
}

fn model_endpoint(base_url: &str, api: &str) -> Result<Url, DiscoveryError> {
    let mut url = Url::parse(base_url)?;
    let base_path = url.path().trim_end_matches('/');
    let path = match api {
        "anthropic-messages" if !base_path.ends_with("/v1") => {
            format!("{base_path}/v1/models")
        }
        _ => format!("{base_path}/models"),
    };
    url.set_path(&path);
    Ok(url)
}

fn parse_catalog(payload: &Value) -> Result<Vec<DiscoveredModel>, DiscoveryError> {
    let items = if let Some(data) = payload.get("data").and_then(Value::as_array) {
        data
    } else if let Some(models) = payload.get("models").and_then(Value::as_array) {
        models
    } else if let Some(items) = payload.as_array() {
        items
    } else {
        return Err(DiscoveryError::UnsupportedResponse);
    };

    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for item in items {
        let (id, name) = if let Some(id) = item.as_str() {
            (id.to_owned(), None)
        } else if let Some(object) = item.as_object() {
            let Some(raw_id) = object
                .get("id")
                .or_else(|| object.get("name"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let id = raw_id.strip_prefix("models/").unwrap_or(raw_id).to_owned();
            let name = object
                .get("display_name")
                .or_else(|| object.get("displayName"))
                .or_else(|| object.get("name"))
                .and_then(Value::as_str)
                .map(|name| name.strip_prefix("models/").unwrap_or(name).to_owned())
                .filter(|name| name != &id);
            (id, name)
        } else {
            continue;
        };
        if !id.trim().is_empty() && seen.insert(id.clone()) {
            models.push(DiscoveredModel {
                id,
                name,
                config: None,
            });
        }
    }
    if models.is_empty() {
        Err(DiscoveryError::EmptyCatalog)
    } else {
        Ok(models)
    }
}

fn provider_environment_variable(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "ant-ling" => "ANT_LING_API_KEY",
        "azure-openai-responses" => "AZURE_OPENAI_API_KEY",
        "cerebras" => "CEREBRAS_API_KEY",
        "cloudflare-ai-gateway" | "cloudflare-workers-ai" => "CLOUDFLARE_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "fireworks" => "FIREWORKS_API_KEY",
        "google" => "GEMINI_API_KEY",
        "groq" => "GROQ_API_KEY",
        "huggingface" => "HF_TOKEN",
        "kimi-coding" => "KIMI_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "minimax-cn" => "MINIMAX_CN_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "nvidia" => "NVIDIA_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "opencode" | "opencode-go" => "OPENCODE_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "qwen-token-plan" => "QWEN_TOKEN_PLAN_API_KEY",
        "qwen-token-plan-cn" => "QWEN_TOKEN_PLAN_CN_API_KEY",
        "together" => "TOGETHER_API_KEY",
        "vercel-ai-gateway" => "AI_GATEWAY_API_KEY",
        "xai" => "XAI_API_KEY",
        "xiaomi" => "XIAOMI_API_KEY",
        "xiaomi-token-plan-cn" => "XIAOMI_TOKEN_PLAN_CN_API_KEY",
        "xiaomi-token-plan-ams" => "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
        "xiaomi-token-plan-sgp" => "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
        "zai" => "ZAI_API_KEY",
        "zai-coding-cn" => "ZAI_CODING_CN_API_KEY",
        _ => return None,
    })
}

fn safe_server_message(bytes: &[u8], secrets: &[String]) -> String {
    let raw = String::from_utf8_lossy(bytes);
    let value = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| raw.into_owned());
    let redacted = secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(value, |message, secret| {
            message.replace(secret, "<redacted>")
        });
    let cleaned: String = redacted
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(400)
        .collect();
    if cleaned.trim().is_empty() {
        "no error message".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn endpoint_matches_api_conventions() {
        assert_eq!(
            model_endpoint("https://api.example.com/v1/", "openai-completions")
                .unwrap()
                .as_str(),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            model_endpoint("https://api.anthropic.com", "anthropic-messages")
                .unwrap()
                .as_str(),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(
            model_endpoint(
                "https://generativelanguage.googleapis.com/v1beta",
                "google-generative-ai"
            )
            .unwrap()
            .as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
        assert_eq!(
            model_endpoint("https://api.example.com/v1?tenant=demo", "openai-responses")
                .unwrap()
                .as_str(),
            "https://api.example.com/v1/models?tenant=demo"
        );
    }

    #[test]
    fn parses_openai_anthropic_and_google_catalogs() {
        let openai = parse_catalog(&json!({
            "data": [{"id":"gpt-a"}, {"id":"gpt-b", "display_name":"GPT B"}]
        }))
        .unwrap();
        assert_eq!(openai[1].name.as_deref(), Some("GPT B"));

        let google = parse_catalog(&json!({
            "models": [{"name":"models/gemini-x", "displayName":"Gemini X"}]
        }))
        .unwrap();
        assert_eq!(google[0].id, "gemini-x");
        assert_eq!(google[0].name.as_deref(), Some("Gemini X"));
    }

    #[test]
    fn discovery_requests_catalog_with_bearer_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]).to_ascii_lowercase();
            assert!(request.starts_with("get /v1/models "));
            assert!(request.contains("authorization: bearer test-token"));
            let body = r#"{"data":[{"id":"remote-model"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let dir = tempdir().unwrap();
        let provider = json!({
            "baseUrl": format!("http://{address}/v1"),
            "api": "openai-completions",
            "apiKey": "test-token"
        });
        let request = DiscoveryRequest::from_provider("test", &provider, dir.path()).unwrap();
        let models = discover_models(&request).unwrap();
        server.join().unwrap();
        assert_eq!(
            models,
            vec![DiscoveredModel {
                id: "remote-model".into(),
                name: None,
                config: None,
            }]
        );
    }

    #[test]
    fn server_errors_are_redacted() {
        let message = safe_server_message(
            br#"{"error":{"message":"upstream rejected secret-token"}}"#,
            &["secret-token".into()],
        );
        assert_eq!(message, "upstream rejected <redacted>");
    }

    #[test]
    fn resolves_interpolation_without_executing_commands() {
        let mut env = HashMap::new();
        env.insert("TOKEN".into(), "secret".into());
        assert_eq!(
            resolve_config_value("Bearer ${TOKEN}:$$:$!", &env).unwrap(),
            "Bearer secret:$:!"
        );
        assert!(matches!(
            resolve_config_value("!password-tool", &env),
            Err(DiscoveryError::CommandCredential)
        ));
    }
}
