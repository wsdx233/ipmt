use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use crate::config::ConfigFormat;

const TEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_OUTPUT_CHARS: usize = 32_000;
const TEST_PROMPT: &str =
    "This is a connectivity test. Reply briefly with: IPMT model test successful.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTestResult {
    pub success: bool,
    pub output: String,
}

pub fn test_model(
    root: &Value,
    config_path: &Path,
    provider_id: &str,
    model_id: &str,
) -> ModelTestResult {
    let mut result = match run_model_test(root, config_path, provider_id, model_id) {
        Ok(result) => result,
        Err(error) => ModelTestResult {
            success: false,
            output: error,
        },
    };
    redact_config_secrets(root, &mut result.output);
    result
}

fn run_model_test(
    root: &Value,
    config_path: &Path,
    provider_id: &str,
    model_id: &str,
) -> Result<ModelTestResult, String> {
    let format = ConfigFormat::from_path(config_path);
    let command = match format {
        ConfigFormat::Json => "pi",
        ConfigFormat::Yaml => "omp",
    };
    let agent_dir = prepare_agent_dir(root, config_path, format)
        .map_err(|error| format!("无法创建临时 {command} 配置：{error}"))?;
    let mut child = Command::new(command)
        .env("PI_CODING_AGENT_DIR", agent_dir.path())
        .args([
            "--provider",
            provider_id,
            "--model",
            model_id,
            "--mode",
            "text",
            "--print",
            "--no-session",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-context-files",
            TEST_PROMPT,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                format!("找不到 {command} 命令；请先安装并确保它位于 PATH 中")
            } else {
                format!("无法启动 {command}：{error}")
            }
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("无法读取 {command} 标准输出"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("无法读取 {command} 错误输出"))?;
    let stdout_reader = thread::spawn(move || read_all(stdout));
    let stderr_reader = thread::spawn(move || read_all(stderr));

    let started = Instant::now();
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status, false),
            Ok(None) if started.elapsed() < TEST_TIMEOUT => {
                thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let status = child
                    .wait()
                    .map_err(|error| format!("终止超时测试失败：{error}"))?;
                break (status, true);
            }
            Err(error) => return Err(format!("等待 {command} 测试进程失败：{error}")),
        }
    };
    let stdout = join_reader(stdout_reader, "标准输出", command)?;
    let stderr = join_reader(stderr_reader, "错误输出", command)?;
    if timed_out {
        let details = combined_output(&stdout, &stderr);
        let message = if details.is_empty() {
            "模型测试超过 60 秒，已终止".to_owned()
        } else {
            format!("模型测试超过 60 秒，已终止\n\n{details}")
        };
        return Ok(ModelTestResult {
            success: false,
            output: truncate_output(message),
        });
    }
    Ok(result_from_output(status, &stdout, &stderr))
}

fn read_all(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
    label: &str,
    command: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("读取 {command} {label}的任务意外结束"))?
        .map_err(|error| format!("读取 {command} {label}失败：{error}"))
}

fn prepare_agent_dir(
    root: &Value,
    config_path: &Path,
    format: ConfigFormat,
) -> io::Result<TempDir> {
    let directory = tempfile::Builder::new()
        .prefix("ipmt-model-test-")
        .tempdir()?;
    let (name, mut bytes) = match format {
        ConfigFormat::Json => (
            "models.json",
            serde_json::to_vec_pretty(root).map_err(io::Error::other)?,
        ),
        ConfigFormat::Yaml => (
            "models.yml",
            serde_yaml::to_string(root)
                .map(String::into_bytes)
                .map_err(io::Error::other)?,
        ),
    };
    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        bytes.pop();
    }
    bytes.push(b'\n');
    fs::write(directory.path().join(name), bytes)?;

    if let Some(parent) = config_path.parent() {
        copy_if_present(parent.join("auth.json"), directory.path().join("auth.json"))?;
    }
    Ok(directory)
}

fn copy_if_present(source: PathBuf, target: PathBuf) -> io::Result<()> {
    match fs::copy(source, target) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn result_from_output(status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> ModelTestResult {
    result_from_parts(status.success(), status.to_string(), stdout, stderr)
}

fn result_from_parts(
    success: bool,
    status: String,
    stdout: &[u8],
    stderr: &[u8],
) -> ModelTestResult {
    let stdout = clean_output(stdout);
    let stderr = clean_output(stderr);
    let output = if success && !stdout.is_empty() {
        stdout
    } else if !stdout.is_empty() && !stderr.is_empty() {
        format!("{stdout}\n\n--- stderr ---\n{stderr}")
    } else if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else if success {
        "模型测试成功，但模型没有返回文本。".to_owned()
    } else {
        format!("模型测试退出，状态：{status}")
    };
    ModelTestResult {
        success,
        output: truncate_output(output),
    }
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = clean_output(stdout);
    let stderr = clean_output(stderr);
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n\n--- stderr ---\n{stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => String::new(),
    }
}

fn clean_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn truncate_output(output: String) -> String {
    if output.chars().count() <= MAX_OUTPUT_CHARS {
        return output;
    }
    let mut truncated = output.chars().take(MAX_OUTPUT_CHARS).collect::<String>();
    truncated.push_str("\n\n[输出已截断]");
    truncated
}

fn redact_config_secrets(root: &Value, output: &mut String) {
    let Some(providers) = root.get("providers").and_then(Value::as_object) else {
        return;
    };
    for provider in providers.values().filter_map(Value::as_object) {
        if let Some(secret) = provider.get("apiKey").and_then(Value::as_str)
            && is_literal_secret(secret)
        {
            *output = output.replace(secret, "[REDACTED]");
        }
        for headers in provider
            .get("headers")
            .into_iter()
            .chain(
                provider
                    .get("models")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|model| model.get("headers")),
            )
            .filter_map(Value::as_object)
        {
            for value in headers.values().filter_map(Value::as_str) {
                if is_literal_secret(value) {
                    *output = output.replace(value, "[REDACTED]");
                }
            }
        }
    }
}

fn is_literal_secret(value: &str) -> bool {
    value.len() >= 6 && !value.starts_with('$') && !value.starts_with('!')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_stdout_is_returned() {
        let result = result_from_parts(true, "exit status: 0".into(), b"model response\n", b"");
        assert!(result.success);
        assert_eq!(result.output, "model response");
    }

    #[test]
    fn failed_process_includes_stdout_and_stderr() {
        let result = result_from_parts(
            false,
            "exit status: 1".into(),
            b"partial response",
            b"request failed",
        );
        assert!(!result.success);
        assert!(result.output.contains("partial response"));
        assert!(result.output.contains("request failed"));
    }

    #[test]
    fn test_output_redacts_literal_credentials() {
        let root = serde_json::json!({
            "providers": {
                "test": {
                    "apiKey": "secret-api-key",
                    "headers": {"Authorization": "Bearer secret-token"}
                }
            }
        });
        let mut output = "failed with secret-api-key and Bearer secret-token".to_owned();
        redact_config_secrets(&root, &mut output);
        assert_eq!(output, "failed with [REDACTED] and [REDACTED]");
    }
    #[test]
    fn prepare_agent_dir_writes_yaml_for_omp() {
        let root = serde_json::json!({
            "providers": {
                "gateway": {
                    "models": [{"id": "model-a"}]
                }
            }
        });
        let directory =
            prepare_agent_dir(&root, Path::new("/tmp/models.yml"), ConfigFormat::Yaml).unwrap();
        let bytes = fs::read(directory.path().join("models.yml")).unwrap();
        let parsed: Value = serde_yaml::from_slice(&bytes).unwrap();
        assert_eq!(parsed, root);
        assert!(!directory.path().join("models.json").exists());
    }
}
