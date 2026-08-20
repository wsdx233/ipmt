use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use directories::BaseDirs;
use ipmt::app::App;
use ipmt::config::{
    ConfigDocument, Severity, configured_agent_directory, configured_omp_agent_directory,
    existing_model_path, existing_yaml_path,
};
use ipmt::ui;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

#[derive(Debug, Parser)]
#[command(
    name = "ipmt",
    version,
    about = "Edit pi providers and models in a terminal UI"
)]
struct Cli {
    /// Edit a specific models.json, models.yml, or models.yaml file.
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,

    /// Validate and summarize the file without opening the TUI.
    #[arg(long)]
    check: bool,

    /// Allow browsing and discovery, but disable all mutations and saves.
    #[arg(long)]
    read_only: bool,

    /// Do not create a timestamped backup on normal saves.
    #[arg(long)]
    no_backup: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ipmt: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let path = config_path(cli.file.as_deref())?;
    let doc =
        ConfigDocument::load(&path).with_context(|| format!("无法载入 {}", path.display()))?;

    if cli.check {
        return Ok(check_document(&doc));
    }

    let mut app = App::new(doc, cli.read_only, !cli.no_backup);
    run_tui(&mut app)?;
    Ok(ExitCode::SUCCESS)
}

fn config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(expand_tilde(path));
    }
    if let Some(directory) = configured_agent_directory() {
        return Ok(existing_model_path(&directory).unwrap_or_else(|| directory.join("models.json")));
    }
    if let Some(directory) = configured_omp_agent_directory() {
        return Ok(existing_yaml_path(&directory).unwrap_or_else(|| directory.join("models.yml")));
    }

    let base = BaseDirs::new().context("无法确定用户主目录")?;
    let pi_path = base.home_dir().join(".pi/agent/models.json");
    if pi_path.exists() {
        return Ok(pi_path);
    }
    let omp_directory = base.home_dir().join(".omp/agent");
    Ok(existing_yaml_path(&omp_directory).unwrap_or(pi_path))
}

fn expand_tilde(path: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };
    if (raw == "~" || raw.starts_with("~/"))
        && let Some(base) = BaseDirs::new()
    {
        return if raw == "~" {
            base.home_dir().to_path_buf()
        } else {
            base.home_dir().join(&raw[2..])
        };
    }
    path.to_path_buf()
}

fn check_document(doc: &ConfigDocument) -> ExitCode {
    let providers = doc.providers();
    let models: usize = providers.iter().map(|provider| provider.model_count).sum();
    let diagnostics = doc.validate();
    let errors = diagnostics
        .iter()
        .filter(|item| item.severity == Severity::Error)
        .count();
    let warnings = diagnostics.len() - errors;

    println!("文件: {}", doc.path().display());
    println!(
        "提供商: {providers_len}  模型: {models}",
        providers_len = providers.len()
    );
    for diagnostic in &diagnostics {
        let level = match diagnostic.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "WARN ",
        };
        println!("{level}  {}  {}", diagnostic.path, diagnostic.message);
    }
    if diagnostics.is_empty() {
        println!("OK  配置通过校验");
    } else {
        println!("结果: {errors} 个错误, {warnings} 个警告");
    }

    if errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_tui(app: &mut App) -> Result<()> {
    let (_guard, mut terminal) = enter_terminal()?;

    loop {
        app.poll_background();
        terminal.draw(|frame| ui::draw(frame, app))?;
        if app.should_quit {
            break;
        }
        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                Event::Mouse(mouse) => {
                    let size = terminal.size()?;
                    ui::handle_mouse(
                        app,
                        mouse,
                        ratatui::layout::Rect::new(0, 0, size.width, size.height),
                    );
                }
                event => app.handle_event(event),
            }
        }
    }
    Ok(())
}

fn enter_terminal() -> Result<(TerminalGuard, Terminal<CrosstermBackend<Stdout>>)> {
    enable_raw_mode().context("无法启用终端 raw mode")?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        Hide
    ) {
        let _ = execute!(
            stdout,
            Show,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        return Err(error).context("无法进入终端界面");
    }
    let guard = TerminalGuard;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend).context("无法初始化终端")?;
    Ok((guard, terminal))
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            Show,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_prefix_only() {
        let expanded = expand_tilde(Path::new("~/models.json"));
        assert!(expanded.is_absolute());
        assert!(expanded.ends_with("models.json"));
        assert_eq!(
            expand_tilde(Path::new("~someone/file")),
            PathBuf::from("~someone/file")
        );
    }
}
