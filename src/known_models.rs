use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{Map, Number, Value};
use thiserror::Error;

pub const KNOWN_MODELS_URL: &str = "https://raw.githubusercontent.com/Wei-Shaw/sub2api/main/backend/resources/model-pricing/model_prices_and_context_window.json";
pub const FALLBACK_MODELS_URL: &str =
    "https://raw.githubusercontent.com/router-for-me/models/main/models.json";
const MAX_CATALOG_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct KnownModel {
    pub id: String,
    pub name: Option<String>,
    pub family: String,
    pub value: Value,
}

#[derive(Debug, Error)]
pub enum KnownCatalogError {
    #[error("获取已知模型目录失败: {0}")]
    Request(reqwest::Error),
    #[error("已知模型目录返回 HTTP {0}")]
    Http(u16),
    #[error("已知模型目录大于 10 MiB")]
    TooLarge,
    #[error("已知模型目录 JSON 无效: {0}")]
    Json(serde_json::Error),
    #[error("已知模型目录格式无效")]
    InvalidFormat,
    #[error("已知模型目录为空")]
    Empty,
}

pub fn fetch_known_models() -> Result<Vec<KnownModel>, KnownCatalogError> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("ipmt/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(KnownCatalogError::Request)?;
    let primary =
        fetch_catalog(&client, KNOWN_MODELS_URL).and_then(|root| parse_known_models(&root));
    let fallback =
        fetch_catalog(&client, FALLBACK_MODELS_URL).and_then(|root| parse_fallback_models(&root));
    match (primary, fallback) {
        (Ok(primary), Ok(fallback)) => Ok(merge_known_models(primary, fallback)),
        (Ok(primary), Err(_)) => Ok(primary),
        (Err(_), Ok(fallback)) => Ok(fallback),
        (Err(error), Err(_)) => Err(error),
    }
}

fn fetch_catalog(client: &Client, url: &str) -> Result<Value, KnownCatalogError> {
    let response = client
        .get(url)
        .send()
        .map_err(|error| KnownCatalogError::Request(error.without_url()))?;
    if !response.status().is_success() {
        return Err(KnownCatalogError::Http(response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
    {
        return Err(KnownCatalogError::TooLarge);
    }
    let bytes = response
        .bytes()
        .map_err(|error| KnownCatalogError::Request(error.without_url()))?;
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(KnownCatalogError::TooLarge);
    }
    serde_json::from_slice(&bytes).map_err(KnownCatalogError::Json)
}

pub fn parse_known_models(root: &Value) -> Result<Vec<KnownModel>, KnownCatalogError> {
    let entries = root.as_object().ok_or(KnownCatalogError::InvalidFormat)?;
    let models = entries
        .iter()
        .filter_map(|(id, entry)| {
            if id.trim().is_empty() {
                return None;
            }
            let source = entry.as_object()?;
            if !is_text_generation_model(source) {
                return None;
            }
            Some(known_model(id, source))
        })
        .collect::<Vec<_>>();
    if models.is_empty() {
        Err(KnownCatalogError::Empty)
    } else {
        Ok(models)
    }
}

fn parse_fallback_models(root: &Value) -> Result<Vec<KnownModel>, KnownCatalogError> {
    let groups = root.as_object().ok_or(KnownCatalogError::InvalidFormat)?;
    let mut models = Vec::new();
    for (family, entries) in groups {
        let Some(entries) = entries.as_array() else {
            continue;
        };
        for entry in entries {
            let Some(source) = entry.as_object() else {
                continue;
            };
            let Some(id) = source.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !id.trim().is_empty() {
                models.push(fallback_model(family, id, source));
            }
        }
    }
    if models.is_empty() {
        Err(KnownCatalogError::Empty)
    } else {
        Ok(merge_known_models(Vec::new(), models))
    }
}

fn merge_known_models(primary: Vec<KnownModel>, fallback: Vec<KnownModel>) -> Vec<KnownModel> {
    let mut ids = primary
        .iter()
        .map(|model| model.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut merged = primary;
    for model in fallback {
        if ids.insert(model.id.clone()) {
            merged.push(model);
        }
    }
    merged
}

fn fallback_model(family: &str, id: &str, source: &Map<String, Value>) -> KnownModel {
    let name = source
        .get("display_name")
        .or_else(|| source.get("displayName"))
        .and_then(Value::as_str)
        .filter(|name| *name != id)
        .map(ToOwned::to_owned);
    let mut value = Map::new();
    value.insert("id".into(), Value::String(id.to_owned()));
    if let Some(name) = &name {
        value.insert("name".into(), Value::String(name.clone()));
    }
    if source.get("thinking").is_some_and(Value::is_object) {
        value.insert("reasoning".into(), Value::Bool(true));
    }
    let supports_image = source
        .get("supportedInputModalities")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("image")));
    value.insert(
        "input".into(),
        Value::Array(if supports_image {
            vec![Value::String("text".into()), Value::String("image".into())]
        } else {
            vec![Value::String("text".into())]
        }),
    );
    copy_u64(
        source,
        &mut value,
        &["context_length", "inputTokenLimit"],
        "contextWindow",
    );
    copy_u64(
        source,
        &mut value,
        &["max_completion_tokens", "outputTokenLimit"],
        "maxTokens",
    );
    KnownModel {
        id: id.to_owned(),
        name,
        family: family.to_owned(),
        value: Value::Object(value),
    }
}

fn known_model(id: &str, source: &Map<String, Value>) -> KnownModel {
    let family = source
        .get("litellm_provider")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let mut value = Map::new();
    value.insert("id".into(), Value::String(id.to_owned()));
    if bool_field(source, "supports_reasoning")
        || bool_field(source, "supports_max_reasoning_effort")
        || bool_field(source, "supports_xhigh_reasoning_effort")
    {
        value.insert("reasoning".into(), Value::Bool(true));
    }
    let supports_image = bool_field(source, "supports_vision")
        || source
            .get("supported_modalities")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("image")));
    value.insert(
        "input".into(),
        Value::Array(if supports_image {
            vec![Value::String("text".into()), Value::String("image".into())]
        } else {
            vec![Value::String("text".into())]
        }),
    );
    copy_u64(source, &mut value, &["max_input_tokens"], "contextWindow");
    copy_u64(
        source,
        &mut value,
        &["max_output_tokens", "max_tokens"],
        "maxTokens",
    );
    if let Some(level) = xhigh_mapping(id, source) {
        let mut levels = Map::new();
        levels.insert("xhigh".into(), Value::String(level.into()));
        value.insert("thinkingLevelMap".into(), Value::Object(levels));
    }
    if let Some(cost) = model_cost(source) {
        value.insert("cost".into(), Value::Object(cost));
    }
    KnownModel {
        id: id.to_owned(),
        name: None,
        family,
        value: Value::Object(value),
    }
}

fn xhigh_mapping<'a>(id: &str, source: &'a Map<String, Value>) -> Option<&'a str> {
    let supports_max = bool_field(source, "supports_max_reasoning_effort");
    let supports_xhigh = bool_field(source, "supports_xhigh_reasoning_effort");
    match (supports_max, supports_xhigh) {
        (false, false) => None,
        (true, false) => Some("max"),
        (false, true) => Some("xhigh"),
        (true, true) => {
            if id.to_ascii_lowercase().contains("claude") {
                Some("max")
            } else {
                // GPT 和无法识别的模型均优先使用上游原生 xhigh。
                Some("xhigh")
            }
        }
    }
}

fn model_cost(source: &Map<String, Value>) -> Option<Map<String, Value>> {
    let mut cost = Map::new();
    copy_price(source, &mut cost, "input_cost_per_token", "input");
    copy_price(source, &mut cost, "output_cost_per_token", "output");
    copy_price(
        source,
        &mut cost,
        "cache_read_input_token_cost",
        "cacheRead",
    );
    copy_price(
        source,
        &mut cost,
        "cache_creation_input_token_cost",
        "cacheWrite",
    );
    if cost.is_empty() {
        return None;
    }

    // Pi 要求 cost 一旦存在就必须包含全部四个字段。上游价格目录经常只给出
    // input/output/cacheRead；未知价格使用 0，不能生成缺字段的 cost 对象。
    for field in ["input", "output", "cacheRead", "cacheWrite"] {
        cost.entry(field)
            .or_insert_with(|| Value::Number(Number::from(0)));
    }
    Some(cost)
}

fn copy_price(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    source_key: &str,
    target_key: &str,
) {
    let Some(per_token) = source.get(source_key).and_then(Value::as_f64) else {
        return;
    };
    // sub2api 使用每 token 价格，pi 的 cost 字段使用每百万 token 价格。
    // 乘法可能产生 0.19999999999999998 一类浮点尾差，价格保留 12 位小数即可。
    let per_million = (per_token * 1_000_000.0 * 1_000_000_000_000.0).round() / 1_000_000_000_000.0;
    if let Some(number) = Number::from_f64(per_million) {
        target.insert(target_key.into(), Value::Number(number));
    }
}

fn bool_field(source: &Map<String, Value>, key: &str) -> bool {
    source.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn is_text_generation_model(source: &Map<String, Value>) -> bool {
    source
        .get("mode")
        .and_then(Value::as_str)
        .is_none_or(|mode| matches!(mode, "chat" | "responses" | "completion"))
}

fn copy_u64(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    keys: &[&str],
    key: &str,
) {
    if let Some(number) = keys
        .iter()
        .find_map(|candidate| source.get(*candidate).and_then(Value::as_u64))
    {
        target.insert(key.into(), Value::Number(number.into()));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn converts_capabilities_prices_and_reasoning_map() {
        let models = parse_known_models(&json!({
            "claude-test": {
                "litellm_provider": "anthropic",
                "mode": "chat",
                "max_input_tokens": 200000,
                "max_output_tokens": 64000,
                "supports_reasoning": true,
                "supports_vision": true,
                "supports_max_reasoning_effort": true,
                "input_cost_per_token": 0.000003,
                "output_cost_per_token": 0.000015,
                "cache_read_input_token_cost": 0.0000002,
                "cache_creation_input_token_cost": 0.00000375
            }
        }))
        .unwrap();
        assert_eq!(models[0].family, "anthropic");
        assert_eq!(models[0].value["reasoning"], true);
        assert_eq!(models[0].value["contextWindow"], 200000);
        assert_eq!(models[0].value["input"], json!(["text", "image"]));
        assert_eq!(models[0].value["thinkingLevelMap"], json!({"xhigh":"max"}));
        assert_eq!(models[0].value["cost"]["input"], 3.0);
        assert_eq!(models[0].value["cost"]["output"], 15.0);
        assert_eq!(models[0].value["cost"]["cacheRead"], 0.2);
        assert_eq!(models[0].value["cost"]["cacheWrite"], 3.75);
    }

    #[test]
    fn maps_combined_reasoning_flags_by_model_family() {
        let flags = json!({
            "supports_max_reasoning_effort": true,
            "supports_xhigh_reasoning_effort": true
        });
        let source = flags.as_object().unwrap();
        assert_eq!(xhigh_mapping("claude-opus", source), Some("max"));
        assert_eq!(xhigh_mapping("gpt-5", source), Some("xhigh"));
        assert_eq!(xhigh_mapping("other-model", source), Some("xhigh"));
    }

    #[test]
    fn fills_missing_cost_fields_required_by_pi_schema() {
        let models = parse_known_models(&json!({
            "partial-price-model": {
                "mode": "chat",
                "input_cost_per_token": 0.00000075,
                "output_cost_per_token": 0.0000045,
                "cache_read_input_token_cost": 0.000000075
            }
        }))
        .unwrap();
        assert_eq!(
            models[0].value["cost"],
            json!({
                "input": 0.75,
                "output": 4.5,
                "cacheRead": 0.075,
                "cacheWrite": 0
            })
        );
    }

    #[test]
    fn excludes_non_text_generation_entries() {
        let models = parse_known_models(&json!({
            "chat-model": {"mode":"chat", "litellm_provider":"openai"},
            "embedding-model": {"mode":"embedding", "litellm_provider":"openai"},
            "image-model": {"mode":"image_generation", "litellm_provider":"openai"}
        }))
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "chat-model");
    }

    #[test]
    fn primary_catalog_wins_and_fallback_fills_missing_ids() {
        let primary = parse_known_models(&json!({
            "shared": {
                "mode":"chat",
                "litellm_provider":"openai",
                "max_input_tokens":400000,
                "input_cost_per_token":0.000002
            }
        }))
        .unwrap();
        let fallback = parse_fallback_models(&json!({
            "openai": [
                {"id":"shared", "context_length":128000},
                {"id":"fallback-only", "context_length":200000, "thinking":{}}
            ]
        }))
        .unwrap();
        let merged = merge_known_models(primary, fallback);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, "shared");
        assert_eq!(merged[0].value["contextWindow"], 400000);
        assert_eq!(merged[0].value["cost"]["input"], 2.0);
        assert_eq!(merged[1].id, "fallback-only");
        assert_eq!(merged[1].value["contextWindow"], 200000);
        assert_eq!(merged[1].value["reasoning"], true);
    }
}
