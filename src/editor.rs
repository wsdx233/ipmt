use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Map, Number, Value, json};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::config::SUPPORTED_APIS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldId {
    ProviderId,
    BaseUrl,
    Api,
    ApiKey,
    Oauth,
    AuthHeader,
    Headers,
    Compat,
    ModelId,
    ModelName,
    Reasoning,
    Vision,
    ContextWindow,
    MaxTokens,
    CostInput,
    CostOutput,
    CostCacheRead,
    CostCacheWrite,
    ThinkingLevelMap,
    ModelHeaders,
    ModelCompat,
}

#[derive(Debug, Clone)]
pub enum FieldKind {
    Text,
    Secret,
    Integer,
    Decimal,
    JsonObject,
    Select(Vec<String>),
    Bool,
}

#[derive(Debug, Clone)]
pub struct EditorField {
    pub id: FieldId,
    pub label: &'static str,
    pub value: String,
    pub kind: FieldKind,
    pub hint: &'static str,
}

impl EditorField {
    pub fn display_value(&self, reveal_secrets: bool) -> String {
        match &self.kind {
            FieldKind::Secret if !reveal_secrets && !self.value.is_empty() => "********".into(),
            FieldKind::Bool => {
                if self.bool_value() {
                    "[x]".into()
                } else {
                    "[ ]".into()
                }
            }
            FieldKind::Select(_) if self.value.is_empty() => "<继承/默认>".into(),
            _ if self.value.is_empty() => "<未设置>".into(),
            _ => self.value.clone(),
        }
    }

    pub fn is_editable_text(&self) -> bool {
        matches!(
            self.kind,
            FieldKind::Text
                | FieldKind::Secret
                | FieldKind::Integer
                | FieldKind::Decimal
                | FieldKind::JsonObject
        )
    }

    pub fn bool_value(&self) -> bool {
        self.value == "true"
    }
}

#[derive(Debug, Clone)]
pub enum FormTarget {
    Provider {
        original_id: Option<String>,
    },
    Model {
        provider_id: String,
        original_index: Option<usize>,
    },
}

#[derive(Debug, Clone)]
pub struct FormState {
    pub title: String,
    pub target: FormTarget,
    pub fields: Vec<EditorField>,
    pub selected: usize,
    pub cursor: usize,
    pub scroll: usize,
    pub reveal_secrets: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormAction {
    None,
    Submit,
    Cancel,
}

#[derive(Debug, Clone)]
pub enum FormSubmission {
    Provider {
        original_id: Option<String>,
        id: String,
        value: Value,
    },
    Model {
        provider_id: String,
        original_index: Option<usize>,
        value: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderTemplate {
    OpenAi,
    Ollama,
    LmStudio,
    Anthropic,
    Google,
    Blank,
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderTemplateInfo {
    pub template: ProviderTemplate,
    pub name: &'static str,
    pub description: &'static str,
    pub suggested_id: &'static str,
}

pub const PROVIDER_TEMPLATES: &[ProviderTemplateInfo] = &[
    ProviderTemplateInfo {
        template: ProviderTemplate::OpenAi,
        name: "OpenAI 兼容",
        description: "适用于标准 Chat Completions / Responses 网关",
        suggested_id: "openai-compatible",
    },
    ProviderTemplateInfo {
        template: ProviderTemplate::Ollama,
        name: "Ollama",
        description: "本机 11434 端口，预置兼容性选项",
        suggested_id: "ollama",
    },
    ProviderTemplateInfo {
        template: ProviderTemplate::LmStudio,
        name: "LM Studio",
        description: "本机 1234 端口的 OpenAI 兼容服务",
        suggested_id: "lm-studio",
    },
    ProviderTemplateInfo {
        template: ProviderTemplate::Anthropic,
        name: "Anthropic 兼容",
        description: "Messages API 或兼容代理",
        suggested_id: "anthropic-compatible",
    },
    ProviderTemplateInfo {
        template: ProviderTemplate::Google,
        name: "Google AI Studio",
        description: "Generative AI v1beta 接口",
        suggested_id: "google-ai-studio",
    },
    ProviderTemplateInfo {
        template: ProviderTemplate::Blank,
        name: "空白配置",
        description: "仅创建 providers 条目",
        suggested_id: "provider",
    },
];

impl FormState {
    pub fn provider(original_id: Option<String>, id: String, value: &Value) -> Self {
        let object = value.as_object();
        let title = if original_id.is_some() {
            format!("编辑提供商  {id}")
        } else {
            "新增提供商".into()
        };
        Self {
            title,
            target: FormTarget::Provider { original_id },
            fields: vec![
                field(
                    FieldId::ProviderId,
                    "提供商 ID",
                    id.clone(),
                    FieldKind::Text,
                    "配置键；保存后可用 --provider <ID> 选择",
                ),
                field(
                    FieldId::BaseUrl,
                    "Base URL",
                    object_string(object, "baseUrl"),
                    FieldKind::Text,
                    "例如 https://host.example/v1",
                ),
                field(
                    FieldId::Api,
                    "API 类型",
                    object_string(object, "api"),
                    FieldKind::Select(
                        std::iter::once(String::new())
                            .chain(SUPPORTED_APIS.iter().map(|item| (*item).to_owned()))
                            .collect(),
                    ),
                    "左右键选择；自定义模型必须由提供商或模型指定 API",
                ),
                field(
                    FieldId::ApiKey,
                    "API Key",
                    object_string(object, "apiKey"),
                    FieldKind::Secret,
                    "推荐 $ENV_VAR；也支持字面值或 !command",
                ),
                field(
                    FieldId::Oauth,
                    "OAuth",
                    object_string(object, "oauth"),
                    FieldKind::Select(vec![String::new(), "radius".into()]),
                    "models.json 当前仅支持 radius",
                ),
                field(
                    FieldId::AuthHeader,
                    "Bearer 认证",
                    bool_string(object_bool(object, "authHeader")),
                    FieldKind::Bool,
                    "开启后自动添加 Authorization: Bearer <apiKey>",
                ),
                field(
                    FieldId::Headers,
                    "自定义 Headers",
                    object_json(object, "headers"),
                    FieldKind::JsonObject,
                    "单行 JSON 对象；值支持环境变量插值",
                ),
                field(
                    FieldId::Compat,
                    "兼容性 Compat",
                    object_json(object, "compat"),
                    FieldKind::JsonObject,
                    "单行 JSON 对象；用于代理或本地服务兼容选项",
                ),
            ],
            selected: 0,
            cursor: grapheme_count(&id),
            scroll: 0,
            reveal_secrets: false,
            error: None,
        }
    }

    pub fn model(provider_id: String, original_index: Option<usize>, value: &Value) -> Self {
        let object = value.as_object();
        let id = object_string(object, "id");
        let title = if original_index.is_some() {
            format!("编辑模型  {id}")
        } else {
            format!("新增模型  /  {provider_id}")
        };
        let cost = object
            .and_then(|object| object.get("cost"))
            .and_then(Value::as_object);
        let vision = object
            .and_then(|object| object.get("input"))
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("image")));
        Self {
            title,
            target: FormTarget::Model {
                provider_id,
                original_index,
            },
            fields: vec![
                field(
                    FieldId::ModelId,
                    "模型 ID",
                    id.clone(),
                    FieldKind::Text,
                    "必填；原样发送给上游 API",
                ),
                field(
                    FieldId::ModelName,
                    "显示名称",
                    object_string(object, "name"),
                    FieldKind::Text,
                    "可选；用于匹配和模型详情",
                ),
                field(
                    FieldId::Api,
                    "API 覆盖",
                    object_string(object, "api"),
                    FieldKind::Select(
                        std::iter::once(String::new())
                            .chain(SUPPORTED_APIS.iter().map(|item| (*item).to_owned()))
                            .collect(),
                    ),
                    "留空继承提供商 API 类型",
                ),
                field(
                    FieldId::Reasoning,
                    "扩展思考",
                    bool_string(object_bool(object, "reasoning")),
                    FieldKind::Bool,
                    "模型是否支持 reasoning / thinking",
                ),
                field(
                    FieldId::Vision,
                    "图像输入",
                    bool_string(vision),
                    FieldKind::Bool,
                    "开启后 input 为 [\"text\", \"image\"]",
                ),
                field(
                    FieldId::ContextWindow,
                    "上下文窗口",
                    object_number(object, "contextWindow"),
                    FieldKind::Integer,
                    "token 数；留空使用 pi 默认值 128000",
                ),
                field(
                    FieldId::MaxTokens,
                    "最大输出",
                    object_number(object, "maxTokens"),
                    FieldKind::Integer,
                    "token 数；留空使用 pi 默认值 16384",
                ),
                field(
                    FieldId::CostInput,
                    "输入价格",
                    object_number(cost, "input"),
                    FieldKind::Decimal,
                    "每百万 token；留空为 0",
                ),
                field(
                    FieldId::CostOutput,
                    "输出价格",
                    object_number(cost, "output"),
                    FieldKind::Decimal,
                    "每百万 token；留空为 0",
                ),
                field(
                    FieldId::CostCacheRead,
                    "缓存读取价格",
                    object_number(cost, "cacheRead"),
                    FieldKind::Decimal,
                    "每百万 token；留空为 0",
                ),
                field(
                    FieldId::CostCacheWrite,
                    "缓存写入价格",
                    object_number(cost, "cacheWrite"),
                    FieldKind::Decimal,
                    "每百万 token；留空为 0",
                ),
                field(
                    FieldId::ThinkingLevelMap,
                    "思考级别映射",
                    object_json(object, "thinkingLevelMap"),
                    FieldKind::JsonObject,
                    "例如 {\"xhigh\":\"max\"}，null 表示不支持",
                ),
                field(
                    FieldId::ModelHeaders,
                    "模型 Headers",
                    object_json(object, "headers"),
                    FieldKind::JsonObject,
                    "仅对此模型生效的请求头",
                ),
                field(
                    FieldId::ModelCompat,
                    "模型 Compat",
                    object_json(object, "compat"),
                    FieldKind::JsonObject,
                    "覆盖提供商级兼容性配置",
                ),
            ],
            selected: 0,
            cursor: grapheme_count(&id),
            scroll: 0,
            reveal_secrets: false,
            error: None,
        }
    }

    pub fn current(&self) -> &EditorField {
        &self.fields[self.selected]
    }

    pub fn current_mut(&mut self) -> &mut EditorField {
        &mut self.fields[self.selected]
    }

    pub fn mouse_select_field(&mut self, index: usize, value_column: usize) {
        if index >= self.fields.len() {
            return;
        }
        self.selected = index;
        self.error = None;
        let field = self.current();
        if !field.is_editable_text() {
            self.cursor = 0;
            return;
        }
        if matches!(field.kind, FieldKind::Secret) && !self.reveal_secrets {
            self.cursor = grapheme_count(&field.value);
            return;
        }

        let mut width = 0;
        let mut cursor = 0;
        for grapheme in field.value.graphemes(true) {
            let next = width + grapheme.width();
            if value_column < next {
                break;
            }
            width = next;
            cursor += 1;
        }
        self.cursor = cursor;
    }

    pub fn mouse_activate_field(&mut self, direction: isize) {
        match self.current().kind {
            FieldKind::Bool => self.toggle_bool(),
            FieldKind::Select(_) => self.cycle_select(direction),
            _ => {}
        }
    }

    pub fn cursor_display_width(&self) -> usize {
        let field = self.current();
        if matches!(field.kind, FieldKind::Secret) && !self.reveal_secrets {
            return field.display_value(false).width();
        }
        prefix_by_graphemes(&field.value, self.cursor).width()
    }

    pub fn handle_key(&mut self, event: KeyEvent) -> FormAction {
        self.error = None;
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            match event.code {
                KeyCode::Char('s') => return FormAction::Submit,
                KeyCode::Char('a') => {
                    self.cursor = 0;
                    return FormAction::None;
                }
                KeyCode::Char('e') => {
                    self.cursor = grapheme_count(&self.current().value);
                    return FormAction::None;
                }
                KeyCode::Char('u') | KeyCode::Char('k') => {
                    if self.current().is_editable_text() {
                        self.current_mut().value.clear();
                        self.cursor = 0;
                    }
                    return FormAction::None;
                }
                KeyCode::Char('w') => {
                    self.delete_previous_word();
                    return FormAction::None;
                }
                _ => {}
            }
        }

        match event.code {
            KeyCode::Esc => FormAction::Cancel,
            KeyCode::F(3) => {
                self.reveal_secrets = !self.reveal_secrets;
                FormAction::None
            }
            KeyCode::Tab | KeyCode::Down => {
                self.select_next();
                FormAction::None
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.select_previous();
                FormAction::None
            }
            KeyCode::Enter => {
                if matches!(self.current().kind, FieldKind::Bool) {
                    self.toggle_bool();
                    FormAction::None
                } else if self.selected + 1 == self.fields.len() {
                    FormAction::Submit
                } else {
                    self.select_next();
                    FormAction::None
                }
            }
            KeyCode::Char(' ') if matches!(self.current().kind, FieldKind::Bool) => {
                self.toggle_bool();
                FormAction::None
            }
            KeyCode::Left if matches!(self.current().kind, FieldKind::Select(_)) => {
                self.cycle_select(-1);
                FormAction::None
            }
            KeyCode::Right if matches!(self.current().kind, FieldKind::Select(_)) => {
                self.cycle_select(1);
                FormAction::None
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                FormAction::None
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(grapheme_count(&self.current().value));
                FormAction::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                FormAction::None
            }
            KeyCode::End => {
                self.cursor = grapheme_count(&self.current().value);
                FormAction::None
            }
            KeyCode::Backspace => {
                self.backspace();
                FormAction::None
            }
            KeyCode::Delete => {
                self.delete();
                FormAction::None
            }
            KeyCode::Char(character)
                if !event
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_char(character);
                FormAction::None
            }
            _ => FormAction::None,
        }
    }

    pub fn insert_paste(&mut self, text: &str) {
        if !self.current().is_editable_text() {
            return;
        }
        for character in text.replace(['\r', '\n'], " ").chars() {
            self.insert_char(character);
        }
    }

    pub fn submit(&self, existing: Option<&Value>) -> Result<FormSubmission, String> {
        match &self.target {
            FormTarget::Provider { original_id } => {
                let id = self.required_trimmed(FieldId::ProviderId, "提供商 ID")?;
                let mut value = existing
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| "提供商配置不是 JSON 对象".to_string())?;
                self.apply_optional_string(object, FieldId::BaseUrl, "baseUrl");
                self.apply_optional_string(object, FieldId::Api, "api");
                self.apply_optional_string(object, FieldId::ApiKey, "apiKey");
                self.apply_optional_string(object, FieldId::Oauth, "oauth");
                self.apply_bool(object, FieldId::AuthHeader, "authHeader");
                self.apply_json_object(object, FieldId::Headers, "headers")?;
                self.apply_json_object(object, FieldId::Compat, "compat")?;
                Ok(FormSubmission::Provider {
                    original_id: original_id.clone(),
                    id,
                    value,
                })
            }
            FormTarget::Model {
                provider_id,
                original_index,
            } => {
                let id = self.required_trimmed(FieldId::ModelId, "模型 ID")?;
                let mut value = existing
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| "模型配置不是 JSON 对象".to_string())?;
                object.insert("id".into(), Value::String(id));
                self.apply_optional_string(object, FieldId::ModelName, "name");
                self.apply_optional_string(object, FieldId::Api, "api");
                self.apply_bool(object, FieldId::Reasoning, "reasoning");
                if self.field(FieldId::Vision).bool_value() {
                    object.insert("input".into(), json!(["text", "image"]));
                } else {
                    object.remove("input");
                }
                self.apply_integer(object, FieldId::ContextWindow, "contextWindow")?;
                self.apply_integer(object, FieldId::MaxTokens, "maxTokens")?;
                self.apply_cost(object)?;
                self.apply_json_object(object, FieldId::ThinkingLevelMap, "thinkingLevelMap")?;
                self.apply_json_object(object, FieldId::ModelHeaders, "headers")?;
                self.apply_json_object(object, FieldId::ModelCompat, "compat")?;
                Ok(FormSubmission::Model {
                    provider_id: provider_id.clone(),
                    original_index: *original_index,
                    value,
                })
            }
        }
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1) % self.fields.len();
        self.cursor = grapheme_count(&self.current().value);
    }

    fn select_previous(&mut self) {
        self.selected = self
            .selected
            .checked_sub(1)
            .unwrap_or(self.fields.len() - 1);
        self.cursor = grapheme_count(&self.current().value);
    }

    fn toggle_bool(&mut self) {
        let value = !self.current().bool_value();
        self.current_mut().value = bool_string(value);
    }

    fn cycle_select(&mut self, direction: isize) {
        let field = self.current_mut();
        let FieldKind::Select(options) = &field.kind else {
            return;
        };
        let current = options
            .iter()
            .position(|option| option == &field.value)
            .unwrap_or(0) as isize;
        let next = (current + direction).rem_euclid(options.len() as isize) as usize;
        field.value.clone_from(&options[next]);
        self.cursor = grapheme_count(&field.value);
    }

    fn insert_char(&mut self, character: char) {
        if !self.current().is_editable_text() || !self.accepts(character) {
            return;
        }
        let cursor = self.cursor;
        let byte = byte_index(&self.current().value, cursor);
        self.current_mut().value.insert(byte, character);
        self.cursor += 1;
    }

    fn accepts(&self, character: char) -> bool {
        match self.current().kind {
            FieldKind::Integer => character.is_ascii_digit(),
            FieldKind::Decimal => {
                character.is_ascii_digit()
                    || (character == '.' && !self.current().value.contains('.'))
            }
            _ => true,
        }
    }

    fn backspace(&mut self) {
        if !self.current().is_editable_text() || self.cursor == 0 {
            return;
        }
        let start = byte_index(&self.current().value, self.cursor - 1);
        let end = byte_index(&self.current().value, self.cursor);
        self.current_mut().value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        if !self.current().is_editable_text() {
            return;
        }
        let count = grapheme_count(&self.current().value);
        if self.cursor >= count {
            return;
        }
        let start = byte_index(&self.current().value, self.cursor);
        let end = byte_index(&self.current().value, self.cursor + 1);
        self.current_mut().value.replace_range(start..end, "");
    }

    fn delete_previous_word(&mut self) {
        if !self.current().is_editable_text() || self.cursor == 0 {
            return;
        }
        let graphemes: Vec<&str> = self.current().value.graphemes(true).collect();
        let mut start = self.cursor;
        while start > 0 && graphemes[start - 1].chars().all(char::is_whitespace) {
            start -= 1;
        }
        while start > 0 && !graphemes[start - 1].chars().all(char::is_whitespace) {
            start -= 1;
        }
        let from = byte_index(&self.current().value, start);
        let to = byte_index(&self.current().value, self.cursor);
        self.current_mut().value.replace_range(from..to, "");
        self.cursor = start;
    }

    fn field(&self, id: FieldId) -> &EditorField {
        self.fields
            .iter()
            .find(|field| field.id == id)
            .expect("form contains all required fields")
    }

    fn required_trimmed(&self, id: FieldId, label: &str) -> Result<String, String> {
        let value = self.field(id).value.trim();
        if value.is_empty() {
            Err(format!("{label}不能为空"))
        } else {
            Ok(value.to_owned())
        }
    }

    fn apply_optional_string(&self, object: &mut Map<String, Value>, id: FieldId, key: &str) {
        let value = self.field(id).value.trim();
        if value.is_empty() {
            object.remove(key);
        } else {
            object.insert(key.into(), Value::String(value.to_owned()));
        }
    }

    fn apply_bool(&self, object: &mut Map<String, Value>, id: FieldId, key: &str) {
        if self.field(id).bool_value() {
            object.insert(key.into(), Value::Bool(true));
        } else {
            object.remove(key);
        }
    }

    fn apply_integer(
        &self,
        object: &mut Map<String, Value>,
        id: FieldId,
        key: &str,
    ) -> Result<(), String> {
        let raw = self.field(id).value.trim();
        if raw.is_empty() {
            object.remove(key);
            return Ok(());
        }
        let value = raw
            .parse::<u64>()
            .map_err(|_| format!("{} 必须是正整数", self.field(id).label))?;
        if value == 0 {
            return Err(format!("{} 必须大于 0", self.field(id).label));
        }
        object.insert(key.into(), Value::Number(value.into()));
        Ok(())
    }

    fn apply_json_object(
        &self,
        object: &mut Map<String, Value>,
        id: FieldId,
        key: &str,
    ) -> Result<(), String> {
        let raw = self.field(id).value.trim();
        if raw.is_empty() {
            object.remove(key);
            return Ok(());
        }
        let value: Value = serde_json::from_str(raw)
            .map_err(|error| format!("{} JSON 无效: {error}", self.field(id).label))?;
        if !value.is_object() {
            return Err(format!("{} 必须是 JSON 对象", self.field(id).label));
        }
        object.insert(key.into(), value);
        Ok(())
    }

    fn apply_cost(&self, object: &mut Map<String, Value>) -> Result<(), String> {
        let mut cost = object
            .get("cost")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (id, key) in [
            (FieldId::CostInput, "input"),
            (FieldId::CostOutput, "output"),
            (FieldId::CostCacheRead, "cacheRead"),
            (FieldId::CostCacheWrite, "cacheWrite"),
        ] {
            let raw = self.field(id).value.trim();
            if raw.is_empty() {
                cost.remove(key);
                continue;
            }
            let value = raw
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .ok_or_else(|| format!("{} 必须是非负数字", self.field(id).label))?;
            if value.as_f64().is_some_and(|value| value < 0.0) {
                return Err(format!("{} 必须是非负数字", self.field(id).label));
            }
            cost.insert(key.into(), Value::Number(value));
        }
        if cost.is_empty() {
            object.remove("cost");
        } else {
            object.insert("cost".into(), Value::Object(cost));
        }
        Ok(())
    }
}

pub fn provider_template(template: ProviderTemplate) -> Value {
    match template {
        ProviderTemplate::OpenAi => json!({
            "baseUrl": "https://api.example.com/v1",
            "api": "openai-completions",
            "apiKey": "$API_KEY",
            "models": []
        }),
        ProviderTemplate::Ollama => json!({
            "baseUrl": "http://localhost:11434/v1",
            "api": "openai-completions",
            "apiKey": "ollama",
            "compat": {
                "supportsDeveloperRole": false,
                "supportsReasoningEffort": false
            },
            "models": []
        }),
        ProviderTemplate::LmStudio => json!({
            "baseUrl": "http://localhost:1234/v1",
            "api": "openai-completions",
            "apiKey": "lm-studio",
            "compat": {
                "supportsDeveloperRole": false,
                "supportsReasoningEffort": false
            },
            "models": []
        }),
        ProviderTemplate::Anthropic => json!({
            "baseUrl": "https://api.anthropic.com",
            "api": "anthropic-messages",
            "apiKey": "$ANTHROPIC_API_KEY",
            "models": []
        }),
        ProviderTemplate::Google => json!({
            "baseUrl": "https://generativelanguage.googleapis.com/v1beta",
            "api": "google-generative-ai",
            "apiKey": "$GEMINI_API_KEY",
            "models": []
        }),
        ProviderTemplate::Blank => json!({}),
    }
}

fn field(
    id: FieldId,
    label: &'static str,
    value: String,
    kind: FieldKind,
    hint: &'static str,
) -> EditorField {
    EditorField {
        id,
        label,
        value,
        kind,
        hint,
    }
}

fn object_string(object: Option<&Map<String, Value>>, key: &str) -> String {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn object_bool(object: Option<&Map<String, Value>>, key: &str) -> bool {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn object_number(object: Option<&Map<String, Value>>, key: &str) -> String {
    object
        .and_then(|object| object.get(key))
        .and_then(|value| match value {
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn object_json(object: Option<&Map<String, Value>>, key: &str) -> String {
    object
        .and_then(|object| object.get(key))
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_default()
}

fn bool_string(value: bool) -> String {
    if value { "true" } else { "false" }.into()
}

fn grapheme_count(value: &str) -> usize {
    value.graphemes(true).count()
}

fn byte_index(value: &str, grapheme_index: usize) -> usize {
    value
        .grapheme_indices(true)
        .nth(grapheme_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

fn prefix_by_graphemes(value: &str, count: usize) -> &str {
    &value[..byte_index(value, count)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_submission_preserves_models_and_unknown_fields() {
        let existing = json!({
            "baseUrl": "https://old.example/v1",
            "api": "openai-completions",
            "models": [{"id":"m", "future": true}],
            "futureProvider": 42
        });
        let mut form = FormState::provider(Some("old".into()), "old".into(), &existing);
        form.fields
            .iter_mut()
            .find(|field| field.id == FieldId::BaseUrl)
            .unwrap()
            .value = "https://new.example/v1".into();
        let FormSubmission::Provider { value, .. } = form.submit(Some(&existing)).unwrap() else {
            panic!("wrong submission kind")
        };
        assert_eq!(value["futureProvider"], 42);
        assert_eq!(value["models"][0]["future"], true);
        assert_eq!(value["baseUrl"], "https://new.example/v1");
    }

    #[test]
    fn model_submission_preserves_cost_tiers() {
        let existing = json!({
            "id":"m",
            "cost": {"input": 1, "tiers": [{"inputTokensAbove": 100, "input": 2}]}
        });
        let mut form = FormState::model("p".into(), Some(0), &existing);
        form.fields
            .iter_mut()
            .find(|field| field.id == FieldId::CostInput)
            .unwrap()
            .value = "1.5".into();
        let FormSubmission::Model { value, .. } = form.submit(Some(&existing)).unwrap() else {
            panic!("wrong submission kind")
        };
        assert_eq!(value["cost"]["input"], 1.5);
        assert!(value["cost"]["tiers"].is_array());
    }

    #[test]
    fn text_editing_uses_grapheme_boundaries() {
        let mut form = FormState::provider(None, "模型".into(), &json!({}));
        assert_eq!(form.cursor, 2);
        form.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(form.current().value, "模");
        form.handle_key(KeyEvent::new(KeyCode::Char('型'), KeyModifiers::NONE));
        assert_eq!(form.current().value, "模型");
    }
}
