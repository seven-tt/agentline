use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod config;
mod login;
mod run;
mod service;
mod state;
mod web;

#[derive(Parser, Debug)]
#[command(name = "agentline", version, about = "IM ↔ coding-agent bridge")]
struct Cli {
    /// Path to config.toml (default: ~/.agentline/config.toml).
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the bridge in the foreground (default if no subcommand given).
    Run {
        /// Start as an ACP server on stdio instead of connecting to IM adapters.
        #[arg(long)]
        acp: bool,
    },
    /// Run the IM-side login flow (e.g. iLink QR-code scan) and persist the token.
    Login,
    /// Manage the background service (macOS launchd today).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Subcommand, Debug)]
enum ServiceAction {
    /// Write the launchd plist and start the daemon.
    Install,
    /// Stop the daemon and remove the launchd plist.
    Uninstall,
    /// Print whether the daemon is running, plus its PID.
    Status,
    /// Show recent log lines from the daemon.
    Logs {
        /// Follow the log (`tail -f`).
        #[arg(long, short)]
        tail: bool,
    },
}

fn main() -> Result<()> {
    // Must run before any other thread exists: tokio's multi-thread runtime
    // spawns worker threads immediately on build, after which `time` refuses
    // to trust the OS local offset (soundness guard) and every `LocalTime`
    // timestamp silently renders as UTC instead of erroring. Capture the
    // offset here, in the still-single-threaded entry point, and thread it
    // through explicitly instead of relying on a later now_local() call.
    let utc_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start tokio runtime")?
        .block_on(async_main(utc_offset))
}

async fn async_main(utc_offset: time::UtcOffset) -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(config::default_config_path);

    // `service` subcommands don't need a parsed config; handle them before load.
    if let Some(Cmd::Service { action }) = &cli.command {
        init_tracing("info", utc_offset);
        return run_service(action, cli.config.as_deref()).await;
    }

    let (mut cfg, created) = config::AppConfig::load_or_init(&config_path)
        .with_context(|| format!("could not load config from {}", config_path.display()))?;
    cfg.config_path = Some(config_path.clone());
    let log_handle = init_tracing(&cfg.log.level, utc_offset);
    if created {
        eprintln!(
            "✨ created default config at {}\n   edit it to set IM/agent credentials, then re-run.",
            config_path.display()
        );
    }

    match cli.command.unwrap_or(Cmd::Run { acp: false }) {
        Cmd::Run { acp } => run::run(cfg, acp, log_handle).await?,
        Cmd::Login => login::run(cfg).await?,
        Cmd::Service { .. } => unreachable!("handled above"),
    }
    Ok(())
}

/// Lets `[log] level` changes from the settings UI take effect immediately —
/// without it, a process-lifetime-static `EnvFilter` means the user has to
/// restart the whole daemon just to turn on debug logging.
#[derive(Clone)]
pub struct LogReloadHandle(
    tracing_subscriber::reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>,
);

impl LogReloadHandle {
    /// Swap the active filter to `level`. A no-op if an explicit `RUST_LOG`
    /// env var is set — that override is meant to win for the whole process
    /// lifetime, same as it does at startup.
    pub fn set_level(&self, level: &str) {
        if std::env::var("RUST_LOG").is_ok() {
            return;
        }
        let _ = self.0.reload(build_filter(level));
    }
}

async fn run_service(action: &ServiceAction, config: Option<&std::path::Path>) -> Result<()> {
    match action {
        ServiceAction::Install => service::install(config),
        ServiceAction::Uninstall => service::uninstall(),
        ServiceAction::Status => service::print_status(),
        ServiceAction::Logs { tail } => service::show_logs(*tail),
    }
}

/// Max rendered length of any single log field value before it's truncated.
const MAX_LOG_FIELD_CHARS: usize = 240;

/// Initialize the global tracing subscriber.
///
/// - `level` is the base level for everything; noisy transitive deps
///   (hyper/reqwest/rustls/…) are pinned to `warn` so even `debug` stays
///   readable. `RUST_LOG`, if set, overrides all of this.
/// - Levels are colorized (ERROR red, WARN yellow, INFO green, …).
/// - Every field value is collapsed to a single line and truncated with `…`
///   so one event never spills across the terminal (e.g. a file-read result).
fn init_tracing(level: &str, utc_offset: time::UtcOffset) -> LogReloadHandle {
    use tracing_subscriber::prelude::*;

    let filter = build_filter(level);
    let make_timer = || {
        tracing_subscriber::fmt::time::OffsetTime::new(
            utc_offset,
            time::macros::format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second][offset_hour sign:mandatory][offset_minute]"
            ),
        )
    };
    let (filter, reload_handle) = tracing_subscriber::reload::Layer::new(filter);
    let use_ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());

    // In foreground mode, tee logs to ~/.agentline/agentline.log so the
    // dashboard can display them.  In daemon mode launchd already redirects
    // stderr to that file, so a single stderr writer is enough.
    if use_ansi {
        let log_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".agentline/agentline.log");
        if let Some(p) = log_path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let stderr_layer = tracing_subscriber::fmt::layer()
                .with_timer(make_timer())
                .with_ansi(true)
                .fmt_fields(TruncatingFields)
                .with_writer(std::io::stderr);
            let file_layer = tracing_subscriber::fmt::layer()
                .with_timer(make_timer())
                .with_ansi(false)
                .fmt_fields(TruncatingFields)
                .with_writer(std::sync::Mutex::new(file));
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .with(file_layer)
                .init();
            return LogReloadHandle(reload_handle);
        }
    }

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_timer(make_timer())
        .with_ansi(use_ansi)
        .fmt_fields(TruncatingFields)
        .with_writer(std::io::stderr);
    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .init();
    LogReloadHandle(reload_handle)
}

/// `level` plus noisy transitive deps (hyper/reqwest/rustls/…) pinned to
/// `warn` so even `debug` stays readable. An explicit `RUST_LOG` env var
/// overrides this entirely, both at startup and on every later reload.
fn build_filter(level: &str) -> tracing_subscriber::EnvFilter {
    let level = match level.to_ascii_lowercase().as_str() {
        l @ ("error" | "warn" | "info" | "debug" | "trace") => l.to_string(),
        _ => "info".to_string(),
    };
    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!(
            "{level},hyper=warn,hyper_util=warn,reqwest=warn,h2=warn,rustls=warn,\
             tower=warn,mio=warn,tungstenite=warn,tokio_tungstenite=warn"
        ))
    })
}

/// Field formatter that keeps every event on a single line: interior newlines
/// are shown as `⏎` and over-long values are cut to [`MAX_LOG_FIELD_CHARS`]
/// with a trailing `…`.
struct TruncatingFields;

impl<'writer>
    tracing_subscriber::field::MakeVisitor<tracing_subscriber::fmt::format::Writer<'writer>>
    for TruncatingFields
{
    type Visitor = TruncVisitor<'writer>;
    fn make_visitor(
        &self,
        target: tracing_subscriber::fmt::format::Writer<'writer>,
    ) -> Self::Visitor {
        TruncVisitor {
            writer: target,
            first: true,
            err: Ok(()),
        }
    }
}

struct TruncVisitor<'writer> {
    writer: tracing_subscriber::fmt::format::Writer<'writer>,
    first: bool,
    err: std::fmt::Result,
}

impl TruncVisitor<'_> {
    fn emit(&mut self, name: &str, value: String) {
        if self.err.is_err() {
            return;
        }
        let value = sanitize_log_value(&value);
        let sep = if self.first { "" } else { " " };
        self.first = false;
        self.err = if name == "message" {
            write!(self.writer, "{sep}{value}")
        } else {
            write!(self.writer, "{sep}{name}={value}")
        };
    }
}

impl tracing::field::Visit for TruncVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.emit(field.name(), value.to_string());
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.emit(field.name(), format!("{value:?}"));
    }
}

impl tracing_subscriber::field::VisitOutput<std::fmt::Result> for TruncVisitor<'_> {
    fn finish(self) -> std::fmt::Result {
        self.err
    }
}

impl tracing_subscriber::field::VisitFmt for TruncVisitor<'_> {
    fn writer(&mut self) -> &mut dyn std::fmt::Write {
        &mut self.writer
    }
}

fn sanitize_log_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_LOG_FIELD_CHARS + 8));
    for (i, c) in s.chars().enumerate() {
        if i >= MAX_LOG_FIELD_CHARS {
            out.push('…');
            break;
        }
        match c {
            '\n' => out.push('⏎'),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{MAX_LOG_FIELD_CHARS, sanitize_log_value};

    #[test]
    fn collapses_newlines_to_single_line() {
        let s = sanitize_log_value("line1\nline2\r\nline3");
        assert!(!s.contains('\n'));
        assert!(!s.contains('\r'));
        assert_eq!(s, "line1⏎line2⏎line3");
    }

    #[test]
    fn truncates_overlong_values_with_ellipsis() {
        let long = "x".repeat(MAX_LOG_FIELD_CHARS + 50);
        let s = sanitize_log_value(&long);
        assert!(s.ends_with('…'));
        // MAX chars kept + the ellipsis
        assert_eq!(s.chars().count(), MAX_LOG_FIELD_CHARS + 1);
    }

    #[test]
    fn short_values_pass_through() {
        assert_eq!(sanitize_log_value("hello"), "hello");
    }
}
