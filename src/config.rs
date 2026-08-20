use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use thiserror::Error;
use url::Url;

const BACKUP_DIRECTORY: &str = ".backup";
const BACKUP_RETENTION: usize = 20;

pub const SUPPORTED_APIS: &[&str] = &[
    "openai-completions",
    "openai-responses",
    "openai-codex-responses",
    "azure-openai-responses",
    "anthropic-messages",
    "bedrock-converse-stream",
    "google-generative-ai",
    "google-gemini-cli",
    "google-vertex",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Yaml,
}

impl ConfigFormat {
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some(extension) if extension.eq_ignore_ascii_case("yaml") => Self::Yaml,
            Some(extension) if extension.eq_ignore_ascii_case("yml") => Self::Yaml,
            _ => Self::Json,
        }
    }
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    non_empty_path(env::var_os(name))
}

fn non_empty_path(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

pub fn configured_agent_directory() -> Option<PathBuf> {
    non_empty_env_path("PI_CODING_AGENT_DIR")
}

pub fn configured_omp_agent_directory() -> Option<PathBuf> {
    non_empty_env_path("PI_CONFIG_DIR").map(|directory| directory.join("agent"))
}

pub fn existing_model_path(directory: &Path) -> Option<PathBuf> {
    ["models.yml", "models.yaml", "models.json"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())
}

pub fn existing_yaml_path(directory: &Path) -> Option<PathBuf> {
    ["models.yml", "models.yaml"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())
}

const BUILT_IN_PROVIDERS: &[&str] = &[
    "amazon-bedrock",
    "ant-ling",
    "anthropic",
    "azure-openai-responses",
    "cerebras",
    "cloudflare-ai-gateway",
    "cloudflare-workers-ai",
    "deepseek",
    "fireworks",
    "github-copilot",
    "google",
    "google-vertex",
    "groq",
    "huggingface",
    "kimi-coding",
    "minimax",
    "minimax-cn",
    "mistral",
    "moonshotai",
    "moonshotai-cn",
    "nvidia",
    "openai",
    "openai-codex",
    "opencode",
    "opencode-go",
    "openrouter",
    "qwen-token-plan",
    "qwen-token-plan-cn",
    "radius",
    "together",
    "vercel-ai-gateway",
    "xai",
    "xiaomi",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-sgp",
    "zai",
    "zai-coding-cn",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub path: String,
    pub message: String,
}

impl Diagnostic {
    fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        }
    }

    fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialHint {
    Missing,
    Environment { name: String, available: bool },
    Command,
    Literal,
}

#[derive(Debug, Clone)]
pub struct ProviderSummary {
    pub id: String,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub model_count: usize,
    pub credential: CredentialHint,
    pub has_overrides: bool,
}

#[derive(Debug, Clone)]
pub struct ModelSummary {
    pub id: String,
    pub name: Option<String>,
    pub api: Option<String>,
    pub reasoning: bool,
    pub vision: bool,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid YAML in {path}: {source}")]
    Yaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("the configuration root must be an object")]
    InvalidRoot,
    #[error("the file changed on disk after it was loaded")]
    ExternalChange,
    #[error("cannot create configuration directory {path}: {source}")]
    CreateDir { path: PathBuf, source: io::Error },
    #[error("cannot save {path}: {source}")]
    Save { path: PathBuf, source: io::Error },
    #[error("cannot serialize configuration: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("cannot serialize YAML configuration: {0}")]
    SerializeYaml(serde_yaml::Error),
}

#[derive(Debug, Clone)]
pub struct SaveOutcome {
    pub backup: Option<PathBuf>,
    pub bytes: usize,
}

#[derive(Debug, Clone)]
pub struct ConfigDocument {
    path: PathBuf,
    format: ConfigFormat,
    root: Value,
    loaded_bytes: Option<Vec<u8>>,
}

impl ConfigDocument {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let requested = path.into();
        let format = ConfigFormat::from_path(&requested);
        let path = resolve_symlink(&requested);
        match fs::read(&path) {
            Ok(bytes) => {
                let root: Value = match format {
                    ConfigFormat::Json => {
                        serde_json::from_slice(&bytes).map_err(|source| ConfigError::Json {
                            path: path.clone(),
                            source,
                        })?
                    }
                    ConfigFormat::Yaml => {
                        serde_yaml::from_slice(&bytes).map_err(|source| ConfigError::Yaml {
                            path: path.clone(),
                            source,
                        })?
                    }
                };
                if !root.is_object() {
                    return Err(ConfigError::InvalidRoot);
                }
                Ok(Self {
                    path,
                    format,
                    root,
                    loaded_bytes: Some(bytes),
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self {
                path,
                format,
                root: json!({ "providers": {} }),
                loaded_bytes: None,
            }),
            Err(source) => Err(ConfigError::Read { path, source }),
        }
    }

    pub fn format(&self) -> ConfigFormat {
        self.format
    }

    #[cfg(test)]
    pub fn from_value(path: impl Into<PathBuf>, root: Value) -> Self {
        let path = path.into();
        Self {
            format: ConfigFormat::from_path(&path),
            path,
            root,
            loaded_bytes: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn root(&self) -> &Value {
        &self.root
    }

    pub fn replace_root(&mut self, root: Value) {
        self.root = root;
    }

    pub fn file_exists(&self) -> bool {
        self.loaded_bytes.is_some()
    }

    pub fn providers(&self) -> Vec<ProviderSummary> {
        let Some(providers) = self.providers_object() else {
            return Vec::new();
        };

        providers
            .iter()
            .map(|(id, value)| {
                let object = value.as_object();
                let api = string_field(object, "api");
                let base_url = string_field(object, "baseUrl");
                let model_count = object
                    .and_then(|item| item.get("models"))
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                let credential = object
                    .and_then(|item| item.get("apiKey"))
                    .and_then(Value::as_str)
                    .map(credential_hint)
                    .unwrap_or(CredentialHint::Missing);
                let has_overrides = object
                    .and_then(|item| item.get("modelOverrides"))
                    .and_then(Value::as_object)
                    .is_some_and(|item| !item.is_empty());
                ProviderSummary {
                    id: id.clone(),
                    api,
                    base_url,
                    model_count,
                    credential,
                    has_overrides,
                }
            })
            .collect()
    }

    pub fn provider_value(&self, provider_id: &str) -> Option<&Value> {
        self.providers_object()?.get(provider_id)
    }

    pub fn provider_value_mut(&mut self, provider_id: &str) -> Option<&mut Value> {
        self.providers_object_mut()?.get_mut(provider_id)
    }

    pub fn models(&self, provider_id: &str) -> Vec<ModelSummary> {
        self.provider_value(provider_id)
            .and_then(Value::as_object)
            .and_then(|provider| provider.get("models"))
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .enumerate()
                    .map(|(index, value)| model_summary(value, index))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn model_value(&self, provider_id: &str, index: usize) -> Option<&Value> {
        self.provider_value(provider_id)?
            .as_object()?
            .get("models")?
            .as_array()?
            .get(index)
    }

    pub fn upsert_provider(
        &mut self,
        original_id: Option<&str>,
        new_id: String,
        value: Value,
    ) -> bool {
        let Some(providers) = self.providers_object_mut() else {
            return false;
        };
        if let Some(original_id) = original_id {
            if !providers.contains_key(original_id) {
                return false;
            }
            let old = std::mem::take(providers);
            let mut renamed = Map::with_capacity(old.len());
            for (id, existing) in old {
                if id == original_id {
                    renamed.insert(new_id.clone(), value.clone());
                } else {
                    renamed.insert(id, existing);
                }
            }
            *providers = renamed;
        } else {
            providers.insert(new_id, value);
        }
        true
    }

    pub fn remove_provider(&mut self, provider_id: &str) -> Option<Value> {
        self.providers_object_mut()?.shift_remove(provider_id)
    }

    pub fn push_model(&mut self, provider_id: &str, value: Value) -> Option<usize> {
        let provider = self.provider_value_mut(provider_id)?.as_object_mut()?;
        let models = provider
            .entry("models")
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()?;
        models.push(value);
        Some(models.len() - 1)
    }

    pub fn replace_model(&mut self, provider_id: &str, index: usize, value: Value) -> bool {
        let Some(model) = self
            .provider_value_mut(provider_id)
            .and_then(Value::as_object_mut)
            .and_then(|provider| provider.get_mut("models"))
            .and_then(Value::as_array_mut)
            .and_then(|models| models.get_mut(index))
        else {
            return false;
        };
        *model = value;
        true
    }

    pub fn remove_model(&mut self, provider_id: &str, index: usize) -> Option<Value> {
        let models = self
            .provider_value_mut(provider_id)?
            .as_object_mut()?
            .get_mut("models")?
            .as_array_mut()?;
        (index < models.len()).then(|| models.remove(index))
    }

    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let Some(root) = self.root.as_object() else {
            diagnostics.push(Diagnostic::error("$", "root must be an object"));
            return diagnostics;
        };
        let Some(providers_value) = root.get("providers") else {
            diagnostics.push(Diagnostic::warning(
                "$.providers",
                "missing providers object; it will be created when an item is added",
            ));
            return diagnostics;
        };
        let Some(providers) = providers_value.as_object() else {
            diagnostics.push(Diagnostic::error(
                "$.providers",
                "providers must be an object",
            ));
            return diagnostics;
        };

        for (provider_id, value) in providers {
            let path = format!("$.providers.{provider_id}");
            if provider_id.trim().is_empty() {
                diagnostics.push(Diagnostic::error(&path, "provider ID cannot be empty"));
            }
            let Some(provider) = value.as_object() else {
                diagnostics.push(Diagnostic::error(&path, "provider must be an object"));
                continue;
            };
            if ![
                "baseUrl",
                "headers",
                "compat",
                "modelOverrides",
                "models",
                "apiKey",
                "api",
                "auth",
                "authHeader",
                "disableStrictTools",
                "discovery",
                "remoteCompaction",
                "transport",
            ]
            .iter()
            .any(|field| provider.contains_key(*field))
            {
                diagnostics.push(Diagnostic::error(
                    &path,
                    "provider must configure a supported provider field",
                ));
            }

            validate_optional_string(provider, "baseUrl", &path, &mut diagnostics);
            validate_optional_string(provider, "apiKey", &path, &mut diagnostics);
            validate_optional_bool(provider, "authHeader", &path, &mut diagnostics);
            validate_api(
                provider.get("api"),
                &format!("{path}.api"),
                &mut diagnostics,
            );
            validate_url(
                provider.get("baseUrl"),
                &format!("{path}.baseUrl"),
                &mut diagnostics,
            );
            validate_string_map(
                provider.get("headers"),
                &format!("{path}.headers"),
                &mut diagnostics,
            );
            validate_optional_object(
                provider.get("compat"),
                &format!("{path}.compat"),
                &mut diagnostics,
            );
            validate_optional_object(
                provider.get("modelOverrides"),
                &format!("{path}.modelOverrides"),
                &mut diagnostics,
            );

            if let Some(oauth) = provider.get("oauth")
                && oauth.as_str() != Some("radius")
            {
                diagnostics.push(Diagnostic::error(
                    format!("{path}.oauth"),
                    "oauth must be \"radius\"",
                ));
            }

            let Some(models_value) = provider.get("models") else {
                continue;
            };
            let Some(models) = models_value.as_array() else {
                diagnostics.push(Diagnostic::error(
                    format!("{path}.models"),
                    "models must be an array",
                ));
                continue;
            };

            if !models.is_empty() && !BUILT_IN_PROVIDERS.contains(&provider_id.as_str()) {
                if provider.get("baseUrl").and_then(Value::as_str).is_none() {
                    diagnostics.push(Diagnostic::error(
                        format!("{path}.baseUrl"),
                        "custom providers with models require baseUrl",
                    ));
                }
                let provider_has_api = provider.get("api").and_then(Value::as_str).is_some();
                let every_model_has_api = models.iter().all(|model| {
                    model
                        .as_object()
                        .and_then(|model| model.get("api"))
                        .and_then(Value::as_str)
                        .is_some()
                });
                if !provider_has_api && !every_model_has_api {
                    diagnostics.push(Diagnostic::error(
                        format!("{path}.api"),
                        "set api on the provider or every model",
                    ));
                }
            }

            let mut ids = HashSet::new();
            for (index, model_value) in models.iter().enumerate() {
                validate_model(
                    model_value,
                    &format!("{path}.models[{index}]"),
                    &mut ids,
                    &mut diagnostics,
                );
            }
        }

        #[cfg(unix)]
        if let Ok(metadata) = fs::metadata(&self.path) {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                diagnostics.push(Diagnostic::warning(
                    "$file.permissions",
                    format!("file mode is {mode:03o}; save with ipmt to change it to 600"),
                ));
            }
        }

        diagnostics
    }

    pub fn save(&mut self, create_backup: bool, force: bool) -> Result<SaveOutcome, ConfigError> {
        let current = match fs::read(&self.path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(ConfigError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        if !force && current != self.loaded_bytes {
            return Err(ConfigError::ExternalChange);
        }

        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
            set_directory_permissions(parent).map_err(|source| ConfigError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let backup = if create_backup {
            current
                .as_deref()
                .map(|bytes| create_backup_file(&self.path, bytes))
                .transpose()
                .map_err(|source| ConfigError::Save {
                    path: self.path.clone(),
                    source,
                })?
        } else {
            None
        };

        if !force {
            let latest = match fs::read(&self.path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(ConfigError::Read {
                        path: self.path.clone(),
                        source,
                    });
                }
            };
            if latest != current {
                return Err(ConfigError::ExternalChange);
            }
        }

        let mut bytes = match self.format {
            ConfigFormat::Json => serde_json::to_vec_pretty(&self.root)?,
            ConfigFormat::Yaml => serde_yaml::to_string(&self.root)
                .map(String::into_bytes)
                .map_err(ConfigError::SerializeYaml)?,
        };
        while bytes
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            bytes.pop();
        }
        bytes.push(b'\n');
        let temp_path = temporary_path(&self.path);
        let result = write_atomic(&temp_path, &self.path, &bytes);
        if let Err(source) = result {
            let _ = fs::remove_file(&temp_path);
            return Err(ConfigError::Save {
                path: self.path.clone(),
                source,
            });
        }

        self.loaded_bytes = Some(bytes.clone());
        Ok(SaveOutcome {
            backup,
            bytes: bytes.len(),
        })
    }

    fn providers_object(&self) -> Option<&Map<String, Value>> {
        self.root.as_object()?.get("providers")?.as_object()
    }

    fn providers_object_mut(&mut self) -> Option<&mut Map<String, Value>> {
        let root = self.root.as_object_mut()?;
        root.entry("providers")
            .or_insert_with(|| Value::Object(Map::new()));
        root.get_mut("providers")?.as_object_mut()
    }
}

fn model_summary(value: &Value, index: usize) -> ModelSummary {
    let object = value.as_object();
    let id = string_field(object, "id").unwrap_or_else(|| format!("<invalid #{index}>"));
    let input = object
        .and_then(|item| item.get("input"))
        .and_then(Value::as_array);
    ModelSummary {
        id,
        name: string_field(object, "name"),
        api: string_field(object, "api"),
        reasoning: object
            .and_then(|item| item.get("reasoning"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        vision: input.is_some_and(|items| items.iter().any(|item| item.as_str() == Some("image"))),
        context_window: object
            .and_then(|item| item.get("contextWindow"))
            .and_then(Value::as_u64),
        max_tokens: object
            .and_then(|item| item.get("maxTokens"))
            .and_then(Value::as_u64),
    }
}

fn string_field(object: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    object?.get(key)?.as_str().map(ToOwned::to_owned)
}

fn credential_hint(value: &str) -> CredentialHint {
    if value.starts_with('!') {
        return CredentialHint::Command;
    }
    if let Some(name) = exact_environment_name(value) {
        return CredentialHint::Environment {
            available: std::env::var_os(&name).is_some(),
            name,
        };
    }
    CredentialHint::Literal
}

fn exact_environment_name(value: &str) -> Option<String> {
    if let Some(name) = value.strip_prefix("${").and_then(|v| v.strip_suffix('}'))
        && !name.is_empty()
        && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    {
        return Some(name.to_owned());
    }
    let name = value.strip_prefix('$')?;
    if !name.is_empty() && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        Some(name.to_owned())
    } else {
        None
    }
}

fn validate_model(
    value: &Value,
    path: &str,
    ids: &mut HashSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(model) = value.as_object() else {
        diagnostics.push(Diagnostic::error(path, "model must be an object"));
        return;
    };
    let id_path = format!("{path}.id");
    match model.get("id").and_then(Value::as_str) {
        Some(id) if id.trim().is_empty() => {
            diagnostics.push(Diagnostic::error(id_path, "model ID cannot be empty"));
        }
        Some(id) if !ids.insert(id.to_owned()) => {
            diagnostics.push(Diagnostic::error(id_path, "duplicate model ID"));
        }
        Some(_) => {}
        None => diagnostics.push(Diagnostic::error(id_path, "model ID is required")),
    }
    validate_optional_string(model, "name", path, diagnostics);
    validate_optional_bool(model, "reasoning", path, diagnostics);
    validate_api(model.get("api"), &format!("{path}.api"), diagnostics);
    validate_positive_integer(
        model.get("contextWindow"),
        &format!("{path}.contextWindow"),
        diagnostics,
    );
    validate_positive_integer(
        model.get("maxTokens"),
        &format!("{path}.maxTokens"),
        diagnostics,
    );
    validate_string_map(
        model.get("headers"),
        &format!("{path}.headers"),
        diagnostics,
    );
    validate_optional_object(model.get("compat"), &format!("{path}.compat"), diagnostics);

    if let (Some(context), Some(max)) = (
        model.get("contextWindow").and_then(Value::as_u64),
        model.get("maxTokens").and_then(Value::as_u64),
    ) && max > context
    {
        diagnostics.push(Diagnostic::warning(
            format!("{path}.maxTokens"),
            "maxTokens is greater than contextWindow",
        ));
    }

    if let Some(input) = model.get("input") {
        let Some(items) = input.as_array() else {
            diagnostics.push(Diagnostic::error(
                format!("{path}.input"),
                "input must be an array",
            ));
            return;
        };
        if items.is_empty() || items.first().and_then(Value::as_str) != Some("text") {
            diagnostics.push(Diagnostic::error(
                format!("{path}.input"),
                "input must start with \"text\"",
            ));
        }
        for item in items {
            if !matches!(item.as_str(), Some("text" | "image")) {
                diagnostics.push(Diagnostic::error(
                    format!("{path}.input"),
                    "input supports only \"text\" and \"image\"",
                ));
            }
        }
    }

    if let Some(cost) = model.get("cost") {
        let Some(cost) = cost.as_object() else {
            diagnostics.push(Diagnostic::error(
                format!("{path}.cost"),
                "cost must be an object",
            ));
            return;
        };
        for field in ["input", "output", "cacheRead", "cacheWrite"] {
            match cost.get(field) {
                None => diagnostics.push(Diagnostic::error(
                    format!("{path}.cost.{field}"),
                    "cost field is required by pi",
                )),
                Some(value) if value.as_f64().is_none_or(|number| number < 0.0) => {
                    diagnostics.push(Diagnostic::error(
                        format!("{path}.cost.{field}"),
                        "cost must be a non-negative number",
                    ));
                }
                Some(_) => {}
            }
        }
    }

    if let Some(map) = model.get("thinkingLevelMap") {
        let Some(map) = map.as_object() else {
            diagnostics.push(Diagnostic::error(
                format!("{path}.thinkingLevelMap"),
                "thinkingLevelMap must be an object",
            ));
            return;
        };
        let levels = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
        for (level, value) in map {
            if !levels.contains(&level.as_str()) {
                diagnostics.push(Diagnostic::warning(
                    format!("{path}.thinkingLevelMap.{level}"),
                    "unknown pi thinking level",
                ));
            }
            if !value.is_null() && !value.is_string() {
                diagnostics.push(Diagnostic::error(
                    format!("{path}.thinkingLevelMap.{level}"),
                    "thinking level value must be a string or null",
                ));
            }
        }
    }
}

fn validate_optional_string(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(value) = object.get(field)
        && !value.is_string()
    {
        diagnostics.push(Diagnostic::error(
            format!("{path}.{field}"),
            format!("{field} must be a string"),
        ));
    }
}

fn validate_optional_bool(
    object: &Map<String, Value>,
    field: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(value) = object.get(field)
        && !value.is_boolean()
    {
        diagnostics.push(Diagnostic::error(
            format!("{path}.{field}"),
            format!("{field} must be true or false"),
        ));
    }
}

fn validate_api(value: Option<&Value>, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(value) = value else {
        return;
    };
    let Some(api) = value.as_str() else {
        diagnostics.push(Diagnostic::error(path, "api must be a string"));
        return;
    };
    if !SUPPORTED_APIS.contains(&api) {
        diagnostics.push(Diagnostic::error(
            path,
            format!("unsupported model API type: {api}"),
        ));
    }
}

fn validate_url(value: Option<&Value>, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(url) = value.and_then(Value::as_str) else {
        return;
    };
    match Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {}
        Ok(_) => diagnostics.push(Diagnostic::error(path, "baseUrl must use http or https")),
        Err(error) => diagnostics.push(Diagnostic::error(path, format!("invalid URL: {error}"))),
    }
}

fn validate_positive_integer(value: Option<&Value>, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    if let Some(value) = value
        && value.as_u64().is_none_or(|number| number == 0)
    {
        diagnostics.push(Diagnostic::error(path, "value must be a positive integer"));
    }
}

fn validate_optional_object(value: Option<&Value>, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    if let Some(value) = value
        && !value.is_object()
    {
        diagnostics.push(Diagnostic::error(path, "value must be an object"));
    }
}

fn validate_string_map(value: Option<&Value>, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(value) = value else {
        return;
    };
    let Some(object) = value.as_object() else {
        diagnostics.push(Diagnostic::error(path, "value must be an object"));
        return;
    };
    for (key, value) in object {
        if !value.is_string() {
            diagnostics.push(Diagnostic::error(
                format!("{path}.{key}"),
                "header value must be a string",
            ));
        }
    }
}

fn resolve_symlink(path: &Path) -> PathBuf {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("models.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_file_name(format!(".{name}.ipmt.{}.{nonce}.tmp", std::process::id()))
}

fn create_backup_file(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let backup_directory = parent.join(BACKUP_DIRECTORY);
    fs::create_dir_all(&backup_directory)?;
    set_directory_permissions(&backup_directory)?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("models.json");
    let prefix = format!("{name}.bak.");
    let first_suffix = backup_entries(&backup_directory, &prefix)?
        .into_iter()
        .filter(|entry| entry.timestamp == timestamp)
        .map(|entry| entry.suffix)
        .max()
        .map_or(0, |suffix| suffix.saturating_add(1));

    for suffix in first_suffix..first_suffix.saturating_add(1000) {
        let ending = if suffix == 0 {
            String::new()
        } else {
            format!(".{suffix}")
        };
        let candidate = backup_directory.join(format!("{prefix}{timestamp}{ending}"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(mut output) => {
                output.write_all(bytes)?;
                output.sync_all()?;
                set_file_permissions(&candidate)?;
                prune_backups(&backup_directory, &prefix)?;
                sync_directory(&backup_directory)?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique backup name",
    ))
}

#[derive(Debug)]
struct BackupEntry {
    path: PathBuf,
    timestamp: u64,
    suffix: u64,
}

fn backup_entries(directory: &Path, prefix: &str) -> io::Result<Vec<BackupEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(remainder) = file_name
            .to_str()
            .and_then(|name| name.strip_prefix(prefix))
        else {
            continue;
        };
        let mut parts = remainder.split('.');
        let Some(timestamp) = parts.next().and_then(|part| part.parse::<u64>().ok()) else {
            continue;
        };
        let suffix = match (parts.next(), parts.next()) {
            (None, None) => 0,
            (Some(part), None) => match part.parse::<u64>() {
                Ok(suffix) => suffix,
                Err(_) => continue,
            },
            _ => continue,
        };
        entries.push(BackupEntry {
            path: entry.path(),
            timestamp,
            suffix,
        });
    }
    Ok(entries)
}

fn prune_backups(directory: &Path, prefix: &str) -> io::Result<()> {
    let mut entries = backup_entries(directory, prefix)?;
    entries.sort_unstable_by_key(|entry| (entry.timestamp, entry.suffix));
    let remove_count = entries.len().saturating_sub(BACKUP_RETENTION);
    for entry in entries.into_iter().take(remove_count) {
        fs::remove_file(entry.path)?;
    }
    Ok(())
}

fn write_atomic(temp_path: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temp_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    set_file_permissions(temp_path)?;
    fs::rename(temp_path, path)?;
    set_file_permissions(path)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn empty_environment_paths_are_ignored() {
        assert_eq!(non_empty_path(Some(OsString::new())), None);
        assert_eq!(non_empty_path(None), None);
        assert_eq!(
            non_empty_path(Some(OsString::from("/tmp/agent"))),
            Some(PathBuf::from("/tmp/agent"))
        );
    }

    #[test]
    fn model_path_helpers_keep_yaml_precedence() {
        let directory = tempdir().unwrap();
        assert_eq!(existing_model_path(directory.path()), None);
        assert_eq!(existing_yaml_path(directory.path()), None);

        fs::write(directory.path().join("models.json"), b"{}").unwrap();
        assert_eq!(
            existing_model_path(directory.path()),
            Some(directory.path().join("models.json"))
        );
        assert_eq!(existing_yaml_path(directory.path()), None);

        fs::write(directory.path().join("models.yaml"), b"{}").unwrap();
        assert_eq!(
            existing_model_path(directory.path()),
            Some(directory.path().join("models.yaml"))
        );
        assert_eq!(
            existing_yaml_path(directory.path()),
            Some(directory.path().join("models.yaml"))
        );
    }

    #[test]
    fn summaries_and_validation_cover_common_errors() {
        let doc = ConfigDocument::from_value(
            "/tmp/models.json",
            json!({
                "providers": {
                    "local": {
                        "baseUrl": "http://localhost:11434/v1",
                        "api": "openai-completions",
                        "apiKey": "$IPMT_MISSING_TEST_KEY",
                        "models": [
                            {"id": "qwen", "reasoning": true, "input": ["text", "image"], "contextWindow": 100, "maxTokens": 200},
                            {"id": "qwen"}
                        ]
                    }
                }
            }),
        );
        let provider = &doc.providers()[0];
        assert_eq!(provider.id, "local");
        assert_eq!(provider.model_count, 2);
        assert!(matches!(
            provider.credential,
            CredentialHint::Environment {
                available: false,
                ..
            }
        ));
        assert!(doc.models("local")[0].vision);
        let diagnostics = doc.validate();
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message == "duplicate model ID")
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message == "maxTokens is greater than contextWindow")
        );
    }

    #[test]
    fn provider_rename_preserves_position_and_unknown_values() {
        let mut doc = ConfigDocument::from_value(
            "/tmp/models.json",
            json!({
                "futureRoot": 1,
                "providers": {
                    "first": {"futureProvider": true},
                    "second": {"models": [{"id": "m", "futureModel": 2}]},
                    "third": {}
                }
            }),
        );
        let mut edited = doc.provider_value("second").unwrap().clone();
        edited["baseUrl"] = json!("https://example.com/v1");
        assert!(doc.upsert_provider(Some("second"), "renamed".into(), edited));
        let ids: Vec<_> = doc.providers().into_iter().map(|item| item.id).collect();
        assert_eq!(ids, ["first", "renamed", "third"]);
        assert_eq!(doc.root()["futureRoot"], 1);
        assert_eq!(
            doc.root()["providers"]["renamed"]["models"][0]["futureModel"],
            2
        );
    }

    #[test]
    fn save_is_atomic_private_and_backed_up() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.json");
        fs::write(&path, b"{\"providers\":{}}\n").unwrap();
        let mut doc = ConfigDocument::load(&path).unwrap();
        assert!(doc.upsert_provider(
            None,
            "local".into(),
            json!({"baseUrl":"http://localhost:1234/v1","api":"openai-completions","models":[]}),
        ));
        let outcome = doc.save(true, false).unwrap();
        let backup = outcome.backup.unwrap();
        assert!(backup.exists());
        assert_eq!(backup.parent(), Some(dir.path().join(".backup").as_path()));
        assert_eq!(fs::read(&backup).unwrap(), b"{\"providers\":{}}\n");
        let loaded = ConfigDocument::load(&path).unwrap();
        assert!(loaded.provider_value("local").is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(dir.path().join(".backup"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn backups_keep_only_the_latest_twenty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("custom.json");
        fs::write(&path, b"{\"providers\":{}}\n").unwrap();
        let mut doc = ConfigDocument::load(&path).unwrap();

        for index in 0..25 {
            doc.root["revision"] = json!(index);
            doc.save(true, false).unwrap();
        }

        let backups = backup_entries(&dir.path().join(".backup"), "custom.json.bak.").unwrap();
        assert_eq!(backups.len(), BACKUP_RETENTION);
        assert!(backups.iter().all(|entry| entry.path.is_file()));
    }

    #[test]
    fn blank_provider_and_invalid_compat_are_rejected() {
        let doc = ConfigDocument::from_value(
            "/tmp/models.json",
            json!({
                "providers": {
                    "blank": {},
                    "bad-compat": {"compat": true}
                }
            }),
        );
        let diagnostics = doc.validate();
        assert!(
            diagnostics.iter().any(|item| {
                item.path == "$.providers.blank" && item.severity == Severity::Error
            })
        );
        assert!(diagnostics.iter().any(|item| {
            item.path == "$.providers.bad-compat.compat" && item.severity == Severity::Error
        }));
    }

    #[test]
    fn incomplete_cost_is_rejected_before_save() {
        let doc = ConfigDocument::from_value(
            "/tmp/models.json",
            json!({
                "providers": {
                    "test": {
                        "baseUrl": "https://example.com/v1",
                        "api": "openai-responses",
                        "models": [{
                            "id": "model",
                            "cost": {"input": 1, "output": 2, "cacheRead": 0.1}
                        }]
                    }
                }
            }),
        );
        let diagnostics = doc.validate();
        assert!(diagnostics.iter().any(|item| {
            item.path == "$.providers.test.models[0].cost.cacheWrite"
                && item.severity == Severity::Error
        }));
    }

    #[test]
    fn mutation_does_not_replace_malformed_providers_value() {
        let mut doc =
            ConfigDocument::from_value("/tmp/models.json", json!({"providers": ["preserve-me"]}));
        assert!(!doc.upsert_provider(None, "new".into(), json!({"models": []})));
        assert_eq!(doc.root()["providers"], json!(["preserve-me"]));
    }

    #[test]
    fn save_detects_external_edits() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.json");
        fs::write(&path, b"{\"providers\":{}}\n").unwrap();
        let mut doc = ConfigDocument::load(&path).unwrap();
        fs::write(&path, b"{\"providers\":{},\"outside\":true}\n").unwrap();
        assert!(matches!(
            doc.save(false, false),
            Err(ConfigError::ExternalChange)
        ));
    }
    #[test]
    fn yaml_model_list_round_trips_and_preserves_omp_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.yml");
        fs::write(
            &path,
            r#"futureRoot: keep
providers:
  gateway:
    baseUrl: https://gateway.example/v1
    api: openai-codex-responses
    auth: none
    disableStrictTools: true
    models:
      - id: model-a
        name: Model A
        input: [text, image]
        contextWindow: 1000
        maxTokens: 500
        futureModel:
          route: fast
"#,
        )
        .unwrap();

        let mut doc = ConfigDocument::load(&path).unwrap();
        assert_eq!(doc.format(), ConfigFormat::Yaml);
        assert_eq!(doc.providers()[0].model_count, 1);
        assert!(doc.models("gateway")[0].vision);
        assert!(
            doc.validate()
                .iter()
                .all(|item| item.severity != Severity::Error)
        );

        let mut edited = doc.model_value("gateway", 0).unwrap().clone();
        edited["name"] = json!("Edited Model");
        assert!(doc.replace_model("gateway", 0, edited));
        doc.save(false, false).unwrap();

        let bytes = fs::read_to_string(&path).unwrap();
        assert!(bytes.contains("providers:"));
        assert!(bytes.contains("openai-codex-responses"));
        assert!(bytes.contains("futureModel:"));
        assert!(!bytes.trim_start().starts_with('{'));
        let loaded = ConfigDocument::load(&path).unwrap();
        assert_eq!(loaded.root()["futureRoot"], "keep");
        assert_eq!(
            loaded.root()["providers"]["gateway"]["models"][0]["name"],
            "Edited Model"
        );
        assert_eq!(
            loaded.root()["providers"]["gateway"]["models"][0]["futureModel"]["route"],
            "fast"
        );
    }

    #[test]
    fn yaml_parse_errors_report_yaml_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        fs::write(&path, "providers: [\n").unwrap();
        assert!(matches!(
            ConfigDocument::load(&path),
            Err(ConfigError::Yaml { path: error_path, .. }) if error_path == path
        ));
    }
}
