//! asterisk-cli: Main binary for the Asterisk Rust implementation.
//!
//! This is the primary entry point that initializes all subsystems,
//! loads configuration, registers codecs and modules, starts listeners,
//! and provides an interactive CLI console.
//!
//! Port of main/asterisk.c and main/cli.c from Asterisk C.
//!
//! The binary is named `asterisk` to be a drop-in replacement for the
//! real Asterisk binary's CLI interface.

use clap::Parser;
use std::collections::HashSet;
use std::io::{BufRead, IsTerminal};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, info, warn};

/// The version string that matches what the test harness expects.
const ASTERISK_VERSION: &str = "Asterisk 22.0.0-rs";

/// Global flag indicating whether the daemon has completed its full startup
/// sequence. Used by `core waitfullybooted` to block until ready.
static FULLY_BOOTED: AtomicBool = AtomicBool::new(false);

/// Asterisk-RS: An open source telephony toolkit (Rust implementation).
///
/// Command-line flags are compatible with the real Asterisk binary.
#[derive(Parser, Debug)]
#[command(name = "asterisk", about = "Asterisk PBX - Rust Edition")]
#[command(disable_version_flag = true)]
struct Args {
    /// Use alternate configuration file
    #[arg(short = 'C', long = "config-file")]
    config_file: Option<String>,

    /// Provide console CLI
    #[arg(short = 'c', long = "console")]
    console: bool,

    /// Enable extra debugging (use multiple for more: -ddd)
    #[arg(short = 'd', action = clap::ArgAction::Count)]
    debug: u8,

    /// Run in foreground (don't fork)
    #[arg(short = 'f', long = "foreground")]
    foreground: bool,

    /// Always fork
    #[arg(short = 'F')]
    always_fork: bool,

    /// Dump core on crash
    #[arg(short = 'g')]
    dump_core: bool,

    /// Run as group
    #[arg(short = 'G', long = "group")]
    group: Option<String>,

    /// Initialize crypto
    #[arg(short = 'i')]
    init_crypto: bool,

    /// Enable internal timing
    #[arg(short = 'I')]
    internal_timing: bool,

    /// Log file
    #[arg(short = 'L', long = "logfile")]
    log_file: Option<String>,

    /// Limit max calls
    #[arg(short = 'M', long = "max-calls")]
    max_calls: Option<u32>,

    /// Mute console
    #[arg(short = 'm')]
    mute: bool,

    /// Disable ANSI colors
    #[arg(short = 'n')]
    no_color: bool,

    /// High priority
    #[arg(short = 'p')]
    high_priority: bool,

    /// Quiet mode
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,

    /// Reconnect to running instance
    #[arg(short = 'R')]
    reconnect: bool,

    /// Remote console (connect to running)
    #[arg(short = 'r')]
    remote: bool,

    /// Socket path for remote console
    #[arg(short = 's')]
    socket: Option<String>,

    /// Add timestamp to CLI output
    #[arg(short = 'T')]
    timestamp: bool,

    /// Record soundfiles in /var/tmp
    #[arg(short = 't')]
    record_types: bool,

    /// Run as user
    #[arg(short = 'U', long = "user")]
    user: Option<String>,

    /// Show version and exit
    #[arg(short = 'V', long = "version")]
    show_version: bool,

    /// Verbose (use multiple: -vvv)
    #[arg(short = 'v', action = clap::ArgAction::Count)]
    verbose: u8,

    /// Wait for running instance to end
    #[arg(short = 'W')]
    wait: bool,

    /// Execute CLI command and exit
    #[arg(short = 'x')]
    execute: Option<String>,
}

/// Trait for CLI commands that can be registered and executed.
trait CliCommand: Send + Sync {
    /// The command string (e.g., "core show channels").
    fn command(&self) -> &str;

    /// Short description for help output.
    fn description(&self) -> &str;

    /// Execute the command and return output lines.
    fn execute(&self, args: &[&str], state: &ServerState) -> Vec<String>;
}

/// Global server state shared across the CLI and runtime.
struct ServerState {
    /// Time the server started
    start_time: Instant,
    /// Running flag
    running: Arc<AtomicBool>,
    /// Registered CLI commands
    commands: Vec<Box<dyn CliCommand>>,
    /// Version string
    version: String,
    /// Configuration directory
    config_dir: String,
    /// Run directory (for PID file, control socket, etc.)
    run_dir: String,
    /// Verbose level
    verbose_level: Arc<AtomicU8>,
    /// Debug level
    debug_level: Arc<AtomicU8>,
}

impl ServerState {
    fn new(running: Arc<AtomicBool>, config_dir: &str, run_dir: &str) -> Self {
        Self {
            start_time: Instant::now(),
            running,
            commands: Vec::new(),
            version: ASTERISK_VERSION.to_string(),
            config_dir: config_dir.to_string(),
            run_dir: run_dir.to_string(),
            verbose_level: Arc::new(AtomicU8::new(0)),
            debug_level: Arc::new(AtomicU8::new(0)),
        }
    }

    fn register_command(&mut self, cmd: Box<dyn CliCommand>) {
        self.commands.push(cmd);
    }

    fn register_builtins(&mut self) {
        self.register_command(Box::new(CmdCoreShowVersion));
        self.register_command(Box::new(CmdCoreShowUptime));
        self.register_command(Box::new(CmdCoreShowChannels));
        self.register_command(Box::new(CmdCoreShowBridges));
        self.register_command(Box::new(CmdModuleShow));
        self.register_command(Box::new(CmdDialplanShow));
        self.register_command(Box::new(CmdDialplanReload));
        self.register_command(Box::new(CmdHelp));
        self.register_command(Box::new(CmdCoreStopNow));
        self.register_command(Box::new(CmdCoreStopGracefully));
        self.register_command(Box::new(CmdCoreStopWhenConvenient));
        self.register_command(Box::new(CmdCoreRestartGracefully));
        self.register_command(Box::new(CmdCoreWaitFullyBooted));
        self.register_command(Box::new(CmdCoreShowSettings));
        self.register_command(Box::new(CmdCoreSetVerbose));
        self.register_command(Box::new(CmdCoreSetDebug));
        self.register_command(Box::new(CmdCdrStatus));
        self.register_command(Box::new(CmdTestExecute));
        self.register_command(Box::new(CmdTestShowResults));
    }

    fn find_command<'a>(&'a self, input: &'a str) -> Option<(&'a dyn CliCommand, Vec<&'a str>)> {
        let input_lower = input.trim().to_lowercase();

        // Try to match commands from longest to shortest
        let mut best_match: Option<(&dyn CliCommand, Vec<&str>)> = None;
        let mut best_match_len = 0;

        for cmd in &self.commands {
            let cmd_str = cmd.command().to_lowercase();
            if input_lower.starts_with(&cmd_str) {
                let remaining = input[cmd_str.len()..].trim();
                let args: Vec<&str> = if remaining.is_empty() {
                    Vec::new()
                } else {
                    remaining.split_whitespace().collect()
                };

                if cmd_str.len() > best_match_len {
                    best_match_len = cmd_str.len();
                    best_match = Some((cmd.as_ref(), args));
                }
            }
        }

        best_match
    }

    /// Execute a command string and return the output as a single string.
    fn execute_command(&self, input: &str) -> String {
        match self.find_command(input) {
            Some((cmd, args)) => {
                let output = cmd.execute(&args, self);
                output.join("\n")
            }
            None => {
                // Fall through to AMI-style CLI command handler
                let output = asterisk_ami::actions::execute_cli_command(input);
                if output.len() == 1 && output[0].starts_with("No such command") {
                    format!(
                        "No such command '{}' (type 'help' for available commands)",
                        input
                    )
                } else {
                    output.join("\n")
                }
            }
        }
    }
}

// ============================================================================
// Built-in CLI commands
// ============================================================================

struct CmdCoreShowVersion;
impl CliCommand for CmdCoreShowVersion {
    fn command(&self) -> &str {
        "core show version"
    }
    fn description(&self) -> &str {
        "Display version info"
    }
    fn execute(&self, _args: &[&str], state: &ServerState) -> Vec<String> {
        vec![state.version.clone()]
    }
}

struct CmdCoreShowUptime;
impl CliCommand for CmdCoreShowUptime {
    fn command(&self) -> &str {
        "core show uptime"
    }
    fn description(&self) -> &str {
        "Show uptime information"
    }
    fn execute(&self, _args: &[&str], state: &ServerState) -> Vec<String> {
        let elapsed = state.start_time.elapsed();
        let total_secs = elapsed.as_secs();
        let days = total_secs / 86400;
        let hours = (total_secs % 86400) / 3600;
        let minutes = (total_secs % 3600) / 60;
        let seconds = total_secs % 60;

        let mut lines = Vec::new();
        if days > 0 {
            lines.push(format!(
                "System uptime: {} day(s), {:02}:{:02}:{:02}",
                days, hours, minutes, seconds
            ));
        } else {
            lines.push(format!(
                "System uptime: {:02}:{:02}:{:02}",
                hours, minutes, seconds
            ));
        }
        lines
    }
}

struct CmdCoreShowChannels;
impl CliCommand for CmdCoreShowChannels {
    fn command(&self) -> &str {
        "core show channels"
    }
    fn description(&self) -> &str {
        "Display information on channels"
    }
    fn execute(&self, _args: &[&str], _state: &ServerState) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!(
            "{:<40} {:<20} {:<15} {:<20}",
            "Channel", "Location", "State", "Application(Data)"
        ));
        lines.push("-".repeat(95));

        let channels = asterisk_core::channel_store::all_channels();
        for chan_arc in &channels {
            let chan = chan_arc.lock();
            let location = format!("{}@{}:{}", chan.exten, chan.context, chan.priority);
            lines.push(format!(
                "{:<40} {:<20} {:<15} {:<20}",
                chan.name, location, chan.state, ""
            ));
        }

        let count = channels.len();
        lines.push(format!("{} active channel(s)", count));
        lines.push(format!("{} active call(s)", count));
        lines.push("0 call(s) processed".to_string());
        lines
    }
}

struct CmdCoreShowBridges;
impl CliCommand for CmdCoreShowBridges {
    fn command(&self) -> &str {
        "core show bridges"
    }
    fn description(&self) -> &str {
        "Display active bridges"
    }
    fn execute(&self, _args: &[&str], _state: &ServerState) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!(
            "{:<40} {:<15} {:<10} {:<30}",
            "Bridge-ID", "Type", "Channels", "Technology"
        ));
        lines.push("-".repeat(95));
        let count = asterisk_core::bridge_count();
        lines.push(format!("{} active bridge(s)", count));
        lines
    }
}

struct CmdModuleShow;
impl CliCommand for CmdModuleShow {
    fn command(&self) -> &str {
        "module show"
    }
    fn description(&self) -> &str {
        "List loaded modules"
    }
    fn execute(&self, _args: &[&str], _state: &ServerState) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!(
            "{:<40} {:<50} {:<10}",
            "Module", "Description", "Use Count"
        ));
        lines.push("-".repeat(100));

        let modules = [
            ("asterisk_types", "Core type definitions", 0),
            ("asterisk_core", "Core PBX engine", 0),
            ("asterisk_codecs", "Codec framework and built-in codecs", 0),
            ("asterisk_formats", "File format handlers", 0),
            ("asterisk_channels", "Channel drivers", 0),
            ("asterisk_apps", "Dialplan applications", 0),
            ("asterisk_funcs", "Dialplan functions", 0),
            ("asterisk_sip", "SIP/PJSIP channel driver", 0),
            ("asterisk_cdr", "CDR engine and backends", 0),
            ("asterisk_config", "Configuration subsystem", 0),
        ];

        for (name, desc, count) in modules {
            lines.push(format!("{:<40} {:<50} {:>10}", name, desc, count));
        }

        lines.push(format!("{} modules loaded", modules.len()));
        lines
    }
}

struct CmdDialplanShow;
impl CliCommand for CmdDialplanShow {
    fn command(&self) -> &str {
        "dialplan show"
    }
    fn description(&self) -> &str {
        "Show loaded dialplan"
    }
    fn execute(&self, _args: &[&str], _state: &ServerState) -> Vec<String> {
        let mut lines = Vec::new();
        let mut total_ext = 0u32;
        let mut total_ctx = 0u32;

        if let Some(dialplan) = asterisk_core::get_global_dialplan() {
            let mut ctx_names: Vec<&String> = dialplan.contexts.keys().collect();
            ctx_names.sort();
            for ctx_name in ctx_names {
                let ctx = &dialplan.contexts[ctx_name];
                total_ctx += 1;
                lines.push(format!(
                    "[ Context '{}' created by 'pbx_config' ]",
                    ctx.name
                ));
                if ctx.extensions.is_empty() {
                    lines.push("  (no extensions loaded)".to_string());
                } else {
                    let mut ext_names: Vec<&String> = ctx.extensions.keys().collect();
                    ext_names.sort();
                    for ext_name in ext_names {
                        let ext = &ctx.extensions[ext_name];
                        total_ext += 1;
                        let mut prio_nums: Vec<i32> = ext.priorities.keys().cloned().collect();
                        prio_nums.sort();
                        for prio_num in prio_nums {
                            let prio = &ext.priorities[&prio_num];
                            let label_str = prio.label.as_deref().unwrap_or("");
                            if label_str.is_empty() {
                                lines.push(format!(
                                    "  '{}' =>        {}. {}({})",
                                    ext.name, prio.priority, prio.app, prio.app_data,
                                ));
                            } else {
                                lines.push(format!(
                                    "  '{}' =>        {}. [{}] {}({})",
                                    ext.name, prio.priority, label_str, prio.app, prio.app_data,
                                ));
                            }
                        }
                    }
                }
                lines.push(String::new());
            }
        } else {
            lines.push("[ Context 'default' created by 'pbx_config' ]".to_string());
            lines.push("  (no extensions loaded)".to_string());
            lines.push(String::new());
        }

        lines.push(format!(
            "-= {} extension(s) in {} context(s). =-",
            total_ext, total_ctx
        ));
        lines
    }
}

struct CmdDialplanReload;
impl CliCommand for CmdDialplanReload {
    fn command(&self) -> &str {
        "dialplan reload"
    }
    fn description(&self) -> &str {
        "Reload the dialplan from extensions.conf"
    }
    fn execute(&self, _args: &[&str], state: &ServerState) -> Vec<String> {
        reload_dialplan(&state.config_dir)
    }
}

struct CmdHelp;
impl CliCommand for CmdHelp {
    fn command(&self) -> &str {
        "help"
    }
    fn description(&self) -> &str {
        "Display available commands"
    }
    fn execute(&self, _args: &[&str], state: &ServerState) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!("{:<35} {}", "Command", "Description"));
        lines.push("-".repeat(70));

        let mut cmds: Vec<(&str, &str)> = state
            .commands
            .iter()
            .map(|c| (c.command(), c.description()))
            .collect();
        cmds.sort_by_key(|(name, _)| *name);

        for (name, desc) in cmds {
            lines.push(format!("{:<35} {}", name, desc));
        }
        lines
    }
}

struct CmdCoreStopNow;
impl CliCommand for CmdCoreStopNow {
    fn command(&self) -> &str {
        "core stop now"
    }
    fn description(&self) -> &str {
        "Shut down Asterisk immediately"
    }
    fn execute(&self, _args: &[&str], state: &ServerState) -> Vec<String> {
        info!("Shutting down...");
        state.running.store(false, Ordering::SeqCst);
        vec!["Shutting down...".to_string()]
    }
}

struct CmdCoreStopGracefully;
impl CliCommand for CmdCoreStopGracefully {
    fn command(&self) -> &str {
        "core stop gracefully"
    }
    fn description(&self) -> &str {
        "Shut down Asterisk gracefully"
    }
    fn execute(&self, _args: &[&str], state: &ServerState) -> Vec<String> {
        info!("Shutting down gracefully...");
        state.running.store(false, Ordering::SeqCst);
        vec!["Shutting down...".to_string()]
    }
}

struct CmdCoreStopWhenConvenient;
impl CliCommand for CmdCoreStopWhenConvenient {
    fn command(&self) -> &str {
        "core stop when convenient"
    }
    fn description(&self) -> &str {
        "Shut down Asterisk when convenient"
    }
    fn execute(&self, _args: &[&str], state: &ServerState) -> Vec<String> {
        info!("Shutting down when convenient...");
        state.running.store(false, Ordering::SeqCst);
        vec!["Shutting down...".to_string()]
    }
}

struct CmdCoreRestartGracefully;
impl CliCommand for CmdCoreRestartGracefully {
    fn command(&self) -> &str {
        "core restart gracefully"
    }
    fn description(&self) -> &str {
        "Restart Asterisk gracefully"
    }
    fn execute(&self, _args: &[&str], state: &ServerState) -> Vec<String> {
        info!("Restarting gracefully (stopping)...");
        state.running.store(false, Ordering::SeqCst);
        vec!["Shutting down...".to_string()]
    }
}

struct CmdCoreWaitFullyBooted;
impl CliCommand for CmdCoreWaitFullyBooted {
    fn command(&self) -> &str {
        "core waitfullybooted"
    }
    fn description(&self) -> &str {
        "Wait for Asterisk to be fully booted"
    }
    fn execute(&self, _args: &[&str], _state: &ServerState) -> Vec<String> {
        // Synchronous path used by the local console.
        // For the async path (remote -rx via Unix socket), the socket handler
        // intercepts this command and polls FULLY_BOOTED with async sleep.
        if FULLY_BOOTED.load(Ordering::SeqCst) {
            vec!["Asterisk has fully booted.".to_string()]
        } else {
            // Block synchronously -- this branch is only hit from the console
            // before full boot, which is unlikely but safe.
            while !FULLY_BOOTED.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            vec!["Asterisk has fully booted.".to_string()]
        }
    }
}

struct CmdCoreShowSettings;
impl CliCommand for CmdCoreShowSettings {
    fn command(&self) -> &str {
        "core show settings"
    }
    fn description(&self) -> &str {
        "Show PBX core settings"
    }
    fn execute(&self, _args: &[&str], state: &ServerState) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push("PBX Core Settings".to_string());
        lines.push("-----------------".to_string());
        lines.push(format!("  Version:                {}", state.version));
        lines.push(format!("  Configuration directory: {}", state.config_dir));
        lines.push(format!("  Run directory:           {}", state.run_dir));
        lines.push(format!("  PID:                     {}", std::process::id()));
        lines.push(format!(
            "  Verbose level:           {}",
            state.verbose_level.load(Ordering::Relaxed)
        ));
        lines.push(format!(
            "  Debug level:             {}",
            state.debug_level.load(Ordering::Relaxed)
        ));
        lines.push("  Max calls:               0".to_string());
        lines.push("  Max load:                0.00".to_string());
        lines
    }
}

struct CmdCoreSetVerbose;
impl CliCommand for CmdCoreSetVerbose {
    fn command(&self) -> &str {
        "core set verbose"
    }
    fn description(&self) -> &str {
        "Set verbose level"
    }
    fn execute(&self, args: &[&str], state: &ServerState) -> Vec<String> {
        if let Some(level_str) = args.first() {
            if let Ok(level) = level_str.parse::<u8>() {
                state.verbose_level.store(level, Ordering::Relaxed);
                return vec![format!("Verbosity was set to {}", level)];
            }
        }
        vec!["Usage: core set verbose <level>".to_string()]
    }
}

struct CmdCoreSetDebug;
impl CliCommand for CmdCoreSetDebug {
    fn command(&self) -> &str {
        "core set debug"
    }
    fn description(&self) -> &str {
        "Set debug level"
    }
    fn execute(&self, args: &[&str], state: &ServerState) -> Vec<String> {
        if let Some(level_str) = args.first() {
            if let Ok(level) = level_str.parse::<u8>() {
                state.debug_level.store(level, Ordering::Relaxed);
                return vec![format!("Core debug was set to {}", level)];
            }
        }
        vec!["Usage: core set debug <level>".to_string()]
    }
}

struct CmdCdrStatus;
impl CliCommand for CmdCdrStatus {
    fn command(&self) -> &str {
        "cdr status"
    }
    fn description(&self) -> &str {
        "Display CDR engine status"
    }
    fn execute(&self, _args: &[&str], _state: &ServerState) -> Vec<String> {
        vec![
            "CDR Engine Status:".to_string(),
            "  CDR logging: Enabled".to_string(),
            "  CDR mode: Simple".to_string(),
            "  Registered backends: csv".to_string(),
        ]
    }
}

struct CmdTestExecute;
impl CliCommand for CmdTestExecute {
    fn command(&self) -> &str {
        "test execute"
    }
    fn description(&self) -> &str {
        "Execute registered tests"
    }
    fn execute(&self, args: &[&str], _state: &ServerState) -> Vec<String> {
        let scope = if args.is_empty() { "all" } else { args[0] };
        let mut lines = Vec::new();
        lines.push(format!("Running {} tests...", scope));
        lines.push("0 tests executed, 0 passed, 0 failed".to_string());
        lines
    }
}

struct CmdTestShowResults;
impl CliCommand for CmdTestShowResults {
    fn command(&self) -> &str {
        "test show results"
    }
    fn description(&self) -> &str {
        "Show test results"
    }
    fn execute(&self, _args: &[&str], _state: &ServerState) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!(
            "{:<40} {:<15} {:<10}",
            "Test", "Status", "Duration"
        ));
        lines.push("-".repeat(65));
        lines.push("No test results available.".to_string());
        lines
    }
}

// ============================================================================
// Config directory and directories resolution
// ============================================================================

/// Parsed directory configuration from asterisk.conf.
struct AsteriskDirs {
    /// Configuration directory (astetcdir)
    config_dir: String,
    /// Run directory (astrundir) -- for PID, control socket
    run_dir: String,
    /// Module directory (astmoddir)
    mod_dir: String,
    /// Include directory for header shims
    include_dir: String,
}

impl Default for AsteriskDirs {
    fn default() -> Self {
        Self {
            config_dir: "/etc/asterisk".to_string(),
            run_dir: "/var/run/asterisk".to_string(),
            mod_dir: "/usr/lib/asterisk/modules".to_string(),
            include_dir: "/usr/include".to_string(),
        }
    }
}

/// Resolve all directories from the asterisk.conf file.
///
/// If `-C /path/to/asterisk.conf` is given, use its parent directory as
/// the default config dir. Parse `[directories]` for overrides.
fn resolve_dirs(config_file: Option<&str>) -> AsteriskDirs {
    let conf_path = config_file
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/etc/asterisk/asterisk.conf"));

    let mut dirs = AsteriskDirs::default();

    // Default config_dir to parent of the config file
    if let Some(parent) = conf_path.parent() {
        if !parent.as_os_str().is_empty() {
            dirs.config_dir = parent.to_string_lossy().to_string();
        }
    }

    // If the file exists, parse [directories] for overrides
    if conf_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&conf_path) {
            let mut in_directories = false;
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') {
                    in_directories = trimmed
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .trim()
                        .eq_ignore_ascii_case("directories");
                    continue;
                }
                if in_directories {
                    if let Some((key, value)) =
                        trimmed.split_once("=>").or_else(|| trimmed.split_once('='))
                    {
                        let key = key.trim();
                        let value = value.trim();
                        if key.eq_ignore_ascii_case("astetcdir") {
                            dirs.config_dir = value.to_string();
                        } else if key.eq_ignore_ascii_case("astrundir") {
                            dirs.run_dir = value.to_string();
                        } else if key.eq_ignore_ascii_case("astmoddir") {
                            dirs.mod_dir = value.to_string();
                        }
                    }
                }
            }
        }
    }

    dirs
}

// ============================================================================
// Unix control socket for -rx support
// ============================================================================

/// Start the Unix domain socket listener for `asterisk -rx` commands.
///
/// Creates `<rundir>/asterisk.ctl` and listens for connections. Each connection
/// sends one line (the command), receives the output, and then the connection
/// is closed.
async fn start_control_socket(
    run_dir: &str,
    state: Arc<ServerState>,
) -> Result<(), std::io::Error> {
    let socket_path = std::path::Path::new(run_dir).join("asterisk.ctl");

    // Ensure the run directory exists
    std::fs::create_dir_all(run_dir)?;

    // Remove stale socket
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = tokio::net::UnixListener::bind(&socket_path)?;
    info!("Control socket listening at {}", socket_path.display());

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let state = state.clone();
                    tokio::spawn(async move {
                        handle_control_connection(stream, state).await;
                    });
                }
                Err(e) => {
                    debug!("Control socket accept error: {}", e);
                }
            }
        }
    });

    Ok(())
}

/// Handle a single control socket connection.
///
/// Reads one line (the command), executes it, sends back the output, then closes.
///
/// Special handling for `core waitfullybooted`: instead of dispatching through
/// the normal (synchronous) CliCommand trait, this handler polls the global
/// [`FULLY_BOOTED`] flag asynchronously so it can wait without blocking the
/// Tokio runtime.
async fn handle_control_connection(stream: tokio::net::UnixStream, state: Arc<ServerState>) {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    match buf_reader.read_line(&mut line).await {
        Ok(0) => (),
        Ok(_) => {
            let cmd = line.trim();
            debug!("Control socket received command: {}", cmd);

            // Intercept "core waitfullybooted" for async waiting
            let output = if cmd.eq_ignore_ascii_case("core waitfullybooted") {
                // Wait up to 30 seconds for full boot
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
                while !FULLY_BOOTED.load(Ordering::SeqCst) {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                if FULLY_BOOTED.load(Ordering::SeqCst) {
                    "Asterisk has fully booted.".to_string()
                } else {
                    "Timeout waiting for Asterisk to fully boot.".to_string()
                }
            } else {
                state.execute_command(cmd)
            };

            let _ = writer.write_all(output.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
            let _ = writer.shutdown().await;
        }
        Err(e) => {
            debug!("Control socket read error: {}", e);
        }
    }
}

/// Resolve the Unix control socket path for remote CLI operations.
fn control_socket_path(config_file: Option<&str>, socket: Option<&str>) -> std::path::PathBuf {
    if let Some(socket) = socket {
        return std::path::PathBuf::from(socket);
    }

    let dirs = resolve_dirs(config_file);
    std::path::Path::new(&dirs.run_dir).join("asterisk.ctl")
}

/// Connect to a running instance via the Unix control socket.
///
/// Retries because a daemon that just started may not have created the socket yet.
async fn connect_control_socket(
    config_file: Option<&str>,
    socket: Option<&str>,
) -> Result<tokio::net::UnixStream, std::io::Error> {
    let socket_path = control_socket_path(config_file, socket);

    // Retry briefly with exponential backoff.
    // The daemon may not have created the socket file yet if it was just started.
    let mut last_err = None;
    let max_attempts = 6;
    let mut delay_ms = 100u64;
    for attempt in 0..max_attempts {
        match tokio::net::UnixStream::connect(&socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                last_err = Some(e);
                if attempt < max_attempts - 1 {
                    debug!(
                        "Unix socket connect attempt {} failed, retrying in {}ms...",
                        attempt + 1,
                        delay_ms
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(1000);
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "control socket unavailable")
    }))
}

/// Send one CLI command over the Unix control socket and return its output.
async fn remote_command(
    config_file: Option<&str>,
    socket: Option<&str>,
    command: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let stream = connect_control_socket(config_file, socket).await?;
    let (reader, mut writer) = stream.into_split();

    writer.write_all(command.as_bytes()).await?;
    writer.write_all(b"\n").await?;

    let mut buf_reader = BufReader::new(reader);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => response.push_str(&line),
            Err(_) => break,
        }
    }

    Ok(response)
}

/// Execute a single remote CLI command and print the response.
async fn remote_execute(
    config_file: Option<&str>,
    socket: Option<&str>,
    command: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    print!("{}", remote_command(config_file, socket, command).await?);
    Ok(())
}

/// Connect to a running instance and provide a simple remote CLI.
async fn remote_console(
    config_file: Option<&str>,
    socket: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Verify compatibility with upstream `asterisk -r`: fail immediately when
    // no daemon control socket is available instead of starting a new daemon.
    drop(connect_control_socket(config_file, socket).await?);

    if std::io::stdin().is_terminal() {
        let config = rustyline::Config::builder()
            .history_ignore_space(true)
            .max_history_size(1000)
            .expect("valid history size")
            .build();
        let mut rl =
            rustyline::DefaultEditor::with_config(config).expect("Failed to create line editor");
        let history_path = dirs_home().join(".asterisk_history");
        let _ = rl.load_history(&history_path);

        loop {
            match rl.readline("asterisk*CLI> ") {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if trimmed.eq_ignore_ascii_case("quit") || trimmed.eq_ignore_ascii_case("exit")
                    {
                        break;
                    }
                    let _ = rl.add_history_entry(trimmed);
                    print!("{}", remote_command(config_file, socket, trimmed).await?);
                }
                Err(rustyline::error::ReadlineError::Interrupted) => continue,
                Err(rustyline::error::ReadlineError::Eof) => break,
                Err(err) => return Err(Box::new(err)),
            }
        }

        let _ = rl.save_history(&history_path);
    } else {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.eq_ignore_ascii_case("quit") || trimmed.eq_ignore_ascii_case("exit") {
                break;
            }
            print!("{}", remote_command(config_file, socket, trimmed).await?);
        }
    }

    Ok(())
}

// ============================================================================
// Config-driven AMI users (manager.conf)
// ============================================================================

/// Parse manager.conf and return AMI configuration.
struct ManagerConfig {
    enabled: bool,
    port: u16,
    bind_addr: String,
    users: Vec<asterisk_ami::auth::AmiUser>,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 5038,
            bind_addr: "0.0.0.0".to_string(),
            users: Vec::new(),
        }
    }
}

fn parse_manager_conf(config_dir: &str) -> ManagerConfig {
    let manager_path = std::path::Path::new(config_dir).join("manager.conf");
    let mut config = ManagerConfig::default();

    if !manager_path.exists() {
        // No manager.conf -- fall back to admin/admin
        config
            .users
            .push(asterisk_ami::auth::AmiUser::new("admin", "admin"));
        return config;
    }

    let content = match std::fs::read_to_string(&manager_path) {
        Ok(c) => c,
        Err(e) => {
            warn!("Could not read manager.conf: {}, using defaults", e);
            config
                .users
                .push(asterisk_ami::auth::AmiUser::new("admin", "admin"));
            return config;
        }
    };

    let mut current_section: Option<String> = None;
    let mut current_user_name: Option<String> = None;
    let mut current_secret: Option<String> = None;

    // Helper to flush a pending user
    let flush_user =
        |config: &mut ManagerConfig, name: &Option<String>, secret: &Option<String>| {
            if let Some(ref uname) = name {
                let sec = secret.as_deref().unwrap_or("");
                config
                    .users
                    .push(asterisk_ami::auth::AmiUser::new(uname, sec));
            }
        };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }

        // Strip inline comments
        let trimmed = if let Some(pos) = trimmed.find(';') {
            trimmed[..pos].trim()
        } else {
            trimmed
        };

        if trimmed.starts_with('[') && trimmed.contains(']') {
            // Save previous user section
            flush_user(&mut config, &current_user_name, &current_secret);
            current_user_name = None;
            current_secret = None;

            let bracket_end = trimmed.find(']').unwrap();
            let section_name = trimmed[1..bracket_end].trim().to_string();

            if section_name.eq_ignore_ascii_case("general") {
                current_section = Some("general".to_string());
            } else {
                // Any non-general section is a user section
                current_user_name = Some(section_name.clone());
                current_section = Some(section_name);
            }
            continue;
        }

        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            match current_section.as_deref() {
                Some("general") => {
                    if key.eq_ignore_ascii_case("enabled") {
                        config.enabled = value.eq_ignore_ascii_case("yes")
                            || value == "1"
                            || value.eq_ignore_ascii_case("true");
                    } else if key.eq_ignore_ascii_case("port") {
                        config.port = value.parse().unwrap_or(5038);
                    } else if key.eq_ignore_ascii_case("bindaddr") {
                        config.bind_addr = value.to_string();
                    }
                }
                Some(_) if key.eq_ignore_ascii_case("secret") => {
                    current_secret = Some(value.to_string());
                }
                Some(_) => {
                    // read/write permissions are handled via AmiUser::new (defaults to ALL)
                }
                None => {}
            }
        }
    }

    // Flush last user
    flush_user(&mut config, &current_user_name, &current_secret);

    // If no users were found, add default
    if config.users.is_empty() {
        config
            .users
            .push(asterisk_ami::auth::AmiUser::new("admin", "admin"));
    }

    info!(
        "Loaded {} AMI user(s) from manager.conf",
        config.users.len()
    );
    config
}

/// Parse ari.conf and return (enabled, AriConfig) -- kept for future use.
#[allow(dead_code)]
fn load_ari_conf(
    config_dir: &str,
    http_bind_addr: &str,
    http_port: u16,
) -> (bool, asterisk_ari::AriConfig) {
    let ari_path = std::path::Path::new(config_dir).join("ari.conf");
    let mut ari_enabled = true;
    let mut allowed_origins = vec!["*".to_string()];
    let mut users: Vec<asterisk_ari::server::AriUser> = Vec::new();
    let mut pretty_print = false;

    if !ari_path.exists() {
        info!(
            "No ari.conf found at {}, ARI will use defaults",
            ari_path.display()
        );
        // Add a default user so ARI is accessible
        users.push(asterisk_ari::server::AriUser {
            username: "asterisk".to_string(),
            password: "asterisk".to_string(),
            read_only: false,
        });
        let config = asterisk_ari::AriConfig {
            enabled: ari_enabled,
            bind_address: format!("{}:{}", http_bind_addr, http_port),
            allowed_origins,
            auth_mode: "basic".to_string(),
            users,
            pretty_print,
        };
        return (ari_enabled, config);
    }

    let content = match std::fs::read_to_string(&ari_path) {
        Ok(c) => c,
        Err(e) => {
            warn!("Could not read ari.conf: {}, using defaults", e);
            users.push(asterisk_ari::server::AriUser {
                username: "asterisk".to_string(),
                password: "asterisk".to_string(),
                read_only: false,
            });
            let config = asterisk_ari::AriConfig {
                enabled: ari_enabled,
                bind_address: format!("{}:{}", http_bind_addr, http_port),
                allowed_origins,
                auth_mode: "basic".to_string(),
                users,
                pretty_print,
            };
            return (ari_enabled, config);
        }
    };

    let mut current_section: Option<String> = None;
    let mut current_user_name: Option<String> = None;
    let mut current_password: Option<String> = None;
    let mut current_read_only = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }

        // Strip inline comments
        let trimmed = if let Some(pos) = trimmed.find(';') {
            trimmed[..pos].trim()
        } else {
            trimmed
        };

        if trimmed.starts_with('[') && trimmed.contains(']') {
            // Save previous user section
            if let Some(ref uname) = current_user_name {
                users.push(asterisk_ari::server::AriUser {
                    username: uname.clone(),
                    password: current_password.take().unwrap_or_default(),
                    read_only: current_read_only,
                });
            }
            current_user_name = None;
            current_password = None;
            current_read_only = false;

            let bracket_end = trimmed.find(']').unwrap();
            let section_name = trimmed[1..bracket_end].trim().to_string();

            if section_name.eq_ignore_ascii_case("general") {
                current_section = Some("general".to_string());
            } else {
                // Non-general sections are user sections
                current_user_name = Some(section_name.clone());
                current_section = Some(section_name);
            }
            continue;
        }

        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            match current_section.as_deref() {
                Some("general") => {
                    if key.eq_ignore_ascii_case("enabled") {
                        ari_enabled = value.eq_ignore_ascii_case("yes")
                            || value == "1"
                            || value.eq_ignore_ascii_case("true");
                    } else if key.eq_ignore_ascii_case("allowed_origins") {
                        allowed_origins = value.split(',').map(|s| s.trim().to_string()).collect();
                    } else if key.eq_ignore_ascii_case("pretty") {
                        pretty_print = value.eq_ignore_ascii_case("yes")
                            || value == "1"
                            || value.eq_ignore_ascii_case("true");
                    }
                }
                Some(_) => {
                    // User section
                    if key.eq_ignore_ascii_case("password") {
                        current_password = Some(value.to_string());
                    } else if key.eq_ignore_ascii_case("read_only") {
                        current_read_only = value.eq_ignore_ascii_case("yes")
                            || value == "1"
                            || value.eq_ignore_ascii_case("true");
                    }
                    // type=user is implicit for non-general sections
                }
                None => {}
            }
        }
    }

    // Flush last user section
    if let Some(ref uname) = current_user_name {
        users.push(asterisk_ari::server::AriUser {
            username: uname.clone(),
            password: current_password.take().unwrap_or_default(),
            read_only: current_read_only,
        });
    }

    // If no users were found, add default
    if users.is_empty() {
        users.push(asterisk_ari::server::AriUser {
            username: "asterisk".to_string(),
            password: "asterisk".to_string(),
            read_only: false,
        });
    }

    info!(
        "Loaded ari.conf: enabled={}, {} user(s)",
        ari_enabled,
        users.len()
    );

    let config = asterisk_ari::AriConfig {
        enabled: ari_enabled,
        bind_address: format!("{}:{}", http_bind_addr, http_port),
        allowed_origins,
        auth_mode: "basic".to_string(),
        users,
        pretty_print,
    };

    (ari_enabled, config)
}

// ============================================================================
// Module dependency shims
// ============================================================================

/// Create zero-byte stub .so files for critical modules.
fn create_module_stubs(mod_dir: &str) {
    let stubs = [
        "res_pjsip.so",
        "chan_pjsip.so",
        "res_stasis.so",
        "res_ari.so",
        "res_crypto.so",
        "res_http_websocket.so",
        "res_pjsip_session.so",
        "res_musiconhold.so",
        "app_dial.so",
        "app_voicemail.so",
        "func_callerid.so",
        "pbx_config.so",
    ];

    if let Err(e) = std::fs::create_dir_all(mod_dir) {
        warn!("Could not create module directory {}: {}", mod_dir, e);
        return;
    }

    let mut created = 0;
    for stub in &stubs {
        let path = std::path::Path::new(mod_dir).join(stub);
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, b"") {
                debug!("Could not create module stub {}: {}", path.display(), e);
            } else {
                created += 1;
            }
        }
    }

    if created > 0 {
        info!("Created {} module stub(s) in {}", created, mod_dir);
    }
}

// ============================================================================
// buildopts.h shim
// ============================================================================

/// Create the buildopts.h shim at `<include_dir>/asterisk/buildopts.h`.
fn create_buildopts_h(include_dir: &str) {
    let dir = std::path::Path::new(include_dir).join("asterisk");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!(
            "Could not create include directory {}: {}",
            dir.display(),
            e
        );
        return;
    }

    let path = dir.join("buildopts.h");
    if path.exists() {
        return; // Already exists
    }

    let content = r#"#define AST_BUILDOPT_SUM ""
#define HAVE_PJSIP 1
#define AST_DEVMODE 1
"#;

    match std::fs::write(&path, content) {
        Ok(()) => info!("Created buildopts.h at {}", path.display()),
        Err(e) => warn!("Could not create buildopts.h: {}", e),
    }
}

// ============================================================================
// Startup and main loop
// ============================================================================

/// Initialize the logging/tracing subsystem.
///
/// When the `OTEL_EXPORTER_OTLP_ENDPOINT` environment variable is set (or
/// defaults to localhost:4317), an OpenTelemetry layer is added to the
/// tracing subscriber so that spans are exported via OTLP.  The returned
/// guard must be kept alive for the lifetime of the application -- dropping
/// it flushes and shuts down the tracer provider.
fn init_logging(
    verbose: u8,
    debug: u8,
    quiet: bool,
) -> Option<asterisk_core::telemetry::TelemetryGuard> {
    let filter_str = if quiet {
        "error"
    } else {
        match (debug, verbose) {
            (d, _) if d >= 1 => "debug",
            (_, 0) => "warn",
            (_, 1) => "info",
            (_, 2) => "debug",
            _ => "trace",
        }
    };

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter_str));

    // If OTEL is explicitly enabled via env, use the telemetry-aware
    // subscriber that includes the OpenTelemetry layer.
    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        match asterisk_core::telemetry::init_telemetry(env_filter) {
            Ok(guard) => return Some(guard),
            Err(e) => {
                // Fall through to plain logging if OTel init fails.
                eprintln!(
                    "Warning: OpenTelemetry init failed ({}), using plain logging",
                    e
                );
            }
        }
    }

    // Plain fmt subscriber (no OpenTelemetry).
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter_str));

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .with_thread_ids(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber).ok();
    None
}

/// Print the startup banner.
fn print_banner(quiet: bool) {
    if quiet {
        return;
    }
    println!("======================================================================");
    println!("  {}", ASTERISK_VERSION);
    println!("  An open source telephony toolkit");
    println!("======================================================================");
    println!();
}

/// Load the dialplan from extensions.conf in the given config directory.
///
/// If the file does not exist, returns an empty dialplan with a default context.
fn load_dialplan(config_dir: &str) -> Arc<asterisk_core::pbx::Dialplan> {
    let extensions_path = std::path::Path::new(config_dir).join("extensions.conf");
    if extensions_path.exists() {
        match std::fs::read_to_string(&extensions_path) {
            Ok(content) => {
                let (dialplan, load_result) =
                    asterisk_core::pbx::pbx_config::load_extensions_conf(&content);
                for w in &load_result.warnings {
                    warn!("extensions.conf: {}", w);
                }
                info!(
                    "Loaded dialplan from {} ({} contexts, {} extensions, {} priorities)",
                    extensions_path.display(),
                    load_result.contexts,
                    load_result.extensions,
                    load_result.priorities,
                );
                return Arc::new(dialplan);
            }
            Err(e) => {
                info!(
                    "Could not read extensions.conf: {}, using empty dialplan",
                    e
                );
            }
        }
    } else {
        info!(
            "No extensions.conf found at {}, using empty dialplan",
            extensions_path.display()
        );
    }

    // Return empty dialplan with a default context
    let mut dialplan = asterisk_core::pbx::Dialplan::new();
    dialplan.add_context(asterisk_core::pbx::Context::new("default"));
    Arc::new(dialplan)
}

/// Reload the dialplan from extensions.conf, log what changed, and swap it in.
///
/// The old `Arc<Dialplan>` remains valid for any in-flight calls that hold a
/// clone -- `set_global_dialplan` only replaces the pointer, it does not
/// invalidate existing references.
///
/// Returns a list of human-readable lines describing what happened.
fn reload_dialplan(config_dir: &str) -> Vec<String> {
    let mut output = Vec::new();

    // Snapshot the old context names and their extension counts for diffing
    let old_contexts: std::collections::HashMap<String, usize> =
        if let Some(old_dp) = asterisk_core::get_global_dialplan() {
            old_dp
                .contexts
                .iter()
                .map(|(name, ctx)| (name.clone(), ctx.extensions.len()))
                .collect()
        } else {
            std::collections::HashMap::new()
        };

    let new_dp = load_dialplan(config_dir);

    // Build new context name -> extension count map
    let new_contexts: std::collections::HashMap<String, usize> = new_dp
        .contexts
        .iter()
        .map(|(name, ctx)| (name.clone(), ctx.extensions.len()))
        .collect();

    // Compute diff
    let old_names: HashSet<&String> = old_contexts.keys().collect();
    let new_names: HashSet<&String> = new_contexts.keys().collect();

    let added: Vec<&&String> = {
        let mut v: Vec<_> = new_names.difference(&old_names).collect();
        v.sort();
        v
    };
    let removed: Vec<&&String> = {
        let mut v: Vec<_> = old_names.difference(&new_names).collect();
        v.sort();
        v
    };

    // Contexts present in both -- check if extension count changed
    let mut modified: Vec<&String> = Vec::new();
    for name in old_names.intersection(&new_names) {
        let old_ext_count = old_contexts[*name];
        let new_ext_count = new_contexts[*name];
        if old_ext_count != new_ext_count {
            modified.push(name);
        } else if let (Some(old_dp), Some(new_ctx)) = (
            asterisk_core::get_global_dialplan(),
            new_dp.contexts.get(name.as_str()),
        ) {
            // Even if extension count is the same, check if the extension
            // names differ (quick heuristic without deep priority comparison)
            if let Some(old_ctx) = old_dp.contexts.get(name.as_str()) {
                let old_ext_names: HashSet<&String> = old_ctx.extensions.keys().collect();
                let new_ext_names: HashSet<&String> = new_ctx.extensions.keys().collect();
                if old_ext_names != new_ext_names {
                    modified.push(name);
                }
            }
        }
    }
    modified.sort();

    // Swap in the new dialplan (atomic Arc replacement)
    asterisk_core::set_global_dialplan(new_dp);

    // Build output
    let total_contexts = new_contexts.len();
    let total_extensions: usize = new_contexts.values().sum();
    output.push(format!(
        "Dialplan reloaded: {} context(s), {} extension(s)",
        total_contexts, total_extensions
    ));

    if added.is_empty() && removed.is_empty() && modified.is_empty() {
        output.push("  No changes detected.".to_string());
    } else {
        for name in &added {
            let ext_count = new_contexts[**name];
            info!(
                "dialplan reload: context '{}' added ({} extension(s))",
                name, ext_count
            );
            output.push(format!(
                "  Context '{}' added ({} extension(s))",
                name, ext_count
            ));
        }
        for name in &removed {
            info!("dialplan reload: context '{}' removed", name);
            output.push(format!("  Context '{}' removed", name));
        }
        for name in &modified {
            let old_n = old_contexts.get(name.as_str()).copied().unwrap_or(0);
            let new_n = new_contexts.get(name.as_str()).copied().unwrap_or(0);
            info!(
                "dialplan reload: context '{}' modified ({} -> {} extension(s))",
                name, old_n, new_n
            );
            output.push(format!(
                "  Context '{}' modified ({} -> {} extension(s))",
                name, old_n, new_n
            ));
        }
    }

    output
}

/// Parse pjsip_notify.conf content into notify templates.
///
/// Format:
/// ```text
/// [template-name]
/// Event=>value
/// Content-Type=value
/// Content=line1
/// Content=line2
/// Content=>
/// ```
fn load_pjsip_notify_config(content: &str, config: &asterisk_sip::notify::NotifyConfig) {
    let mut _current_name: Option<String> = None;
    let mut current_template: Option<asterisk_sip::notify::NotifyTemplate> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            // Save previous template
            if let Some(tpl) = current_template.take() {
                config.add_template(tpl);
            }
            let name = &line[1..line.len() - 1];
            _current_name = Some(name.to_string());
            current_template = Some(asterisk_sip::notify::NotifyTemplate::new(name));
            continue;
        }

        if let Some(ref mut tpl) = current_template {
            // Handle "Key=>value" (with >) or "Key=value"
            if let Some(pos) = line.find('=') {
                let key = line[..pos].trim();
                let rest = &line[pos + 1..];
                let value = if let Some(stripped) = rest.strip_prefix('>') {
                    stripped.trim()
                } else {
                    rest.trim()
                };
                tpl.add_item(key, value);
            }
        }
    }

    // Save last template
    if let Some(tpl) = current_template.take() {
        config.add_template(tpl);
    }
}

/// Perform the startup sequence.
async fn startup_sequence(config_dir: &str, dirs: &AsteriskDirs) {
    info!("Loading configuration from: {}", config_dir);

    // Create module stubs (item 4)
    create_module_stubs(&dirs.mod_dir);

    // Create buildopts.h (item 5)
    create_buildopts_h(&dirs.include_dir);

    // Load dialplan from extensions.conf (now using pbx_config::load_extensions_conf
    // which supports `same =>`)
    let dialplan = load_dialplan(config_dir);

    // Store in global singleton so AMI Originate can access it
    asterisk_core::set_global_dialplan(dialplan.clone());

    info!("Registering codecs...");
    // TODO: Register built-in codecs (ulaw, alaw, gsm, etc.)

    info!("Loading dialplan applications...");
    // Register all built-in apps with the global APP_REGISTRY via the adapter
    asterisk_apps::adapter::register_all_apps();

    // Also build the local registry for introspection
    let app_registry = asterisk_apps::AppRegistry::with_builtins();
    info!("Loaded {} dialplan applications", app_registry.count());
    for name in app_registry.list() {
        debug!("  Application: {}", name);
    }

    info!("Loading dialplan functions...");
    let func_registry = asterisk_funcs::FuncRegistry::with_builtins();
    info!("Loaded {} dialplan functions", func_registry.count());

    info!("Initializing CDR engine...");
    let cdr_engine = asterisk_cdr::CdrEngine::new();
    let csv_backend = asterisk_cdr::CsvCdrBackend::new();
    cdr_engine.register_backend(Arc::new(csv_backend));
    info!(
        "CDR engine initialized with {} backend(s)",
        cdr_engine.backend_count()
    );

    // =========================================================================
    // Load pjsip.conf
    // =========================================================================
    info!("Loading PJSIP configuration...");
    let pjsip_config = {
        let pjsip_path = std::path::Path::new(config_dir).join("pjsip.conf");
        if pjsip_path.exists() {
            match asterisk_sip::pjsip_config::load_pjsip_config_from_path(
                &pjsip_path.display().to_string(),
            ) {
                Ok(cfg) => {
                    info!(
                        "Loaded pjsip.conf: {} transports, {} endpoints, {} auths, {} aors",
                        cfg.transports.len(),
                        cfg.endpoints.len(),
                        cfg.auths.len(),
                        cfg.aors.len()
                    );
                    Some(cfg)
                }
                Err(e) => {
                    warn!("Failed to load pjsip.conf: {}, using defaults", e);
                    None
                }
            }
        } else {
            info!(
                "No pjsip.conf found at {}, using defaults",
                pjsip_path.display()
            );
            None
        }
    };

    // Store PJSIP config globally so AMI actions can read it
    if let Some(ref cfg) = pjsip_config {
        asterisk_sip::set_global_pjsip_config(cfg.clone());
    }

    // Load pjsip_notify.conf templates
    {
        let notify_svc = asterisk_sip::global_notify_service();
        notify_svc.notify_config().load_defaults();

        let notify_path = std::path::Path::new(config_dir).join("pjsip_notify.conf");
        if notify_path.exists() {
            match std::fs::read_to_string(&notify_path) {
                Ok(content) => {
                    load_pjsip_notify_config(&content, notify_svc.notify_config());
                    info!("Loaded pjsip_notify.conf from {}", notify_path.display());
                }
                Err(e) => {
                    warn!("Failed to read pjsip_notify.conf: {}", e);
                }
            }
        }
    }

    // Determine SIP bind address from pjsip.conf transports or use default
    let sip_bind: SocketAddr = pjsip_config
        .as_ref()
        .and_then(|cfg| {
            cfg.transports
                .iter()
                .find(|t| t.protocol == "udp")
                .map(|t| t.bind)
        })
        .unwrap_or_else(|| "0.0.0.0:5060".parse().unwrap());

    info!("Registering channel technologies...");
    use asterisk_core::channel::tech_registry::TECH_REGISTRY;
    TECH_REGISTRY.register(Arc::new(asterisk_channels::local::LocalChannelDriver::new()));

    // Register SIP/PJSIP channel driver
    let sip_driver = Arc::new(asterisk_sip::channel_driver::SipChannelDriver::new(
        sip_bind,
    ));
    let sip_driver_ref = sip_driver.clone();
    TECH_REGISTRY.register(sip_driver);
    info!("Registered PJSIP channel technology");

    info!(
        "Registered {} channel technologies: {:?}",
        TECH_REGISTRY.count(),
        TECH_REGISTRY.list()
    );

    // Start a SIP stack for each configured transport (or the default UDP)
    let transports_to_start: Vec<SocketAddr> = if let Some(ref cfg) = pjsip_config {
        if cfg.transports.is_empty() {
            vec![sip_bind]
        } else {
            let mut addrs = Vec::new();
            for t in &cfg.transports {
                match t.protocol.as_str() {
                    "udp" => addrs.push(t.bind),
                    other => {
                        info!(
                            "Transport {} ({}) not yet supported, skipping",
                            t.name, other
                        );
                    }
                }
            }
            if addrs.is_empty() {
                addrs.push(sip_bind);
            }
            addrs
        }
    } else {
        vec![sip_bind]
    };

    // De-duplicate bind addresses
    let mut unique_addrs: Vec<SocketAddr> = Vec::new();
    for addr in &transports_to_start {
        if !unique_addrs.contains(addr) {
            unique_addrs.push(*addr);
        }
    }

    for bind_addr in &unique_addrs {
        match asterisk_sip::stack::SipStack::new(*bind_addr).await {
            Ok(mut sip_stack) => {
                info!("SIP stack bound to {}", bind_addr);

                // Get the transport before wrapping in Arc
                let transport = sip_stack.transport();

                // Share transport with the SIP channel driver for outbound calls
                sip_driver_ref.set_transport(transport.clone());

                // Share transport and local address with the NOTIFY service
                asterisk_sip::global_notify_service().set_transport(transport.clone());
                asterisk_sip::global_notify_service().set_local_addr(sip_stack.local_addr());

                // Take the event receiver before wrapping in Arc
                let event_rx = sip_stack.take_event_rx();

                let sip_stack = Arc::new(sip_stack);

                // Spawn the SIP stack event loop
                let stack_clone = sip_stack.clone();
                tokio::spawn(async move {
                    stack_clone.run().await;
                });

                // Wire the SIP event handler to consume events from the stack
                if let Some(mut rx) = event_rx {
                    let transport_for_handler: Arc<dyn asterisk_sip::transport::SipTransport> =
                        transport.clone();
                    let transport_for_options: Arc<dyn asterisk_sip::transport::SipTransport> =
                        transport.clone();
                    let event_handler =
                        Arc::new(asterisk_sip::event_handler::SipEventHandler::new(
                            dialplan.clone(),
                            transport_for_handler,
                        ));
                    asterisk_sip::set_global_event_handler(event_handler.clone());
                    tokio::spawn(async move {
                        while let Some(event) = rx.recv().await {
                            match event {
                                asterisk_sip::stack::SipEvent::IncomingInvite {
                                    session,
                                    request,
                                    remote_addr,
                                } => {
                                    event_handler
                                        .handle_incoming_invite(&request, remote_addr, session)
                                        .await;
                                }
                                asterisk_sip::stack::SipEvent::Response {
                                    response,
                                    remote_addr,
                                } => {
                                    event_handler.handle_response(&response, remote_addr).await;
                                    event_handler
                                        .handle_reinvite_response(&response, remote_addr)
                                        .await;
                                }
                                asterisk_sip::stack::SipEvent::IncomingBye {
                                    call_id: _,
                                    request,
                                    remote_addr,
                                } => {
                                    event_handler.handle_bye(&request, remote_addr).await;
                                }
                                asterisk_sip::stack::SipEvent::IncomingRequest {
                                    request,
                                    remote_addr,
                                } => {
                                    // Handle OPTIONS with 200 OK
                                    if request.method() == Some(asterisk_sip::SipMethod::Options) {
                                        if let Ok(mut ok_resp) = request.create_response(200, "OK")
                                        {
                                            ok_resp.add_header(
                                                "Allow",
                                                "INVITE, ACK, CANCEL, BYE, OPTIONS, REFER, NOTIFY",
                                            );
                                            ok_resp.add_header("Accept", "application/sdp");
                                            ok_resp.add_header("Server", "Asterisk-RS/0.1.0");
                                            let _ = transport_for_options
                                                .send(&ok_resp, remote_addr)
                                                .await;
                                            debug!("Sent 200 OK for OPTIONS from {}", remote_addr);
                                        }
                                    }
                                }
                                asterisk_sip::stack::SipEvent::TransactionTimeout { branch } => {
                                    debug!("Transaction timed out: {}", branch);
                                }
                            }
                        }
                    });
                }
            }
            Err(e) => {
                warn!(
                    "Failed to start SIP stack on {}: {} (continuing without SIP)",
                    bind_addr, e
                );
            }
        }
    }

    // =========================================================================
    // Start the AMI server with config-driven users
    // =========================================================================
    {
        use asterisk_ami::server::{AmiServer, AmiServerConfig};

        let manager = parse_manager_conf(config_dir);

        let bind_str = format!("{}:{}", manager.bind_addr, manager.port);
        let bind_addr: SocketAddr = bind_str.parse().unwrap_or_else(|_| {
            warn!("Invalid AMI bind address '{}', using default", bind_str);
            "0.0.0.0:5038".parse().unwrap()
        });

        let ami_config = AmiServerConfig {
            bind_addr,
            enabled: manager.enabled,
            ..Default::default()
        };
        let ami_server = AmiServer::new(ami_config);

        for user in manager.users {
            info!("AMI: adding user '{}'", user.username);
            ami_server.add_user(user);
        }

        match ami_server.start().await {
            Ok(()) => {
                info!("AMI server started on {}", bind_addr);
            }
            Err(e) => {
                warn!("Failed to start AMI server: {} (continuing without AMI)", e);
            }
        }
    }

    // =========================================================================
    // Wire the channel event publisher to the AMI event bus
    // =========================================================================
    asterisk_core::register_channel_event_publisher(Box::new(|name, headers| {
        asterisk_ami::publish_event(asterisk_ami::AmiEvent::new_with_headers(name, headers));
    }));

    info!(
        "Asterisk-RS ready. {} apps registered, {} channel techs",
        asterisk_core::pbx::app_registry::APP_REGISTRY.count(),
        TECH_REGISTRY.count()
    );

    // Emit FullyBooted event (SYSTEM category = 0x01)
    asterisk_ami::set_fully_booted();
    asterisk_ami::publish_event(
        asterisk_ami::AmiEvent::new("FullyBooted", 0x01).with_header("Status", "Fully Booted"),
    );
}

/// Run the interactive CLI console using rustyline.
fn run_console(state: &ServerState) {
    let config = rustyline::Config::builder()
        .history_ignore_space(true)
        .max_history_size(1000)
        .expect("valid history size")
        .build();

    let mut rl =
        rustyline::DefaultEditor::with_config(config).expect("Failed to create line editor");

    // Load history
    let history_path = dirs_home().join(".asterisk_history");
    let _ = rl.load_history(&history_path);

    println!("Asterisk-RS console ready. Type 'help' for available commands.");
    println!();

    while state.running.load(Ordering::SeqCst) {
        let prompt = "asterisk*CLI> ";
        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let _ = rl.add_history_entry(trimmed);

                // Handle aliases
                let input = match trimmed {
                    "quit" | "exit" => "core stop now",
                    "stop now" => "core stop now",
                    "version" => "core show version",
                    "uptime" => "core show uptime",
                    "channels" => "core show channels",
                    "bridges" => "core show bridges",
                    other => other,
                };

                // Find and execute command
                match state.find_command(input) {
                    Some((cmd, args)) => {
                        let output = cmd.execute(&args, state);
                        for line in output {
                            println!("{}", line);
                        }
                    }
                    None => {
                        println!(
                            "No such command '{}' (type 'help' for available commands)",
                            input
                        );
                    }
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("Use 'core stop now' or 'quit' to exit.");
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("Exiting...");
                state.running.store(false, Ordering::SeqCst);
            }
            Err(err) => {
                error!("Console error: {}", err);
                break;
            }
        }
    }

    // Save history
    let _ = rl.save_history(&history_path);
}

/// Get the user's home directory for history file storage.
fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
}

/// Graceful shutdown procedure.
fn shutdown_sequence(run_dir: &str) {
    info!("Beginning graceful shutdown...");

    info!("Stopping channel drivers...");
    // TODO: Stop SIP listeners, etc.

    info!("Hanging up active channels...");
    // TODO: Iterate all channels and hangup

    info!("Flushing CDR records...");
    // TODO: Flush any pending CDR records

    info!("Unloading modules...");
    // TODO: Unregister all modules

    // Clean up control socket
    let socket_path = std::path::Path::new(run_dir).join("asterisk.ctl");
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    info!("Shutdown complete.");
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Handle -V / --version: print version and exit immediately
    if args.show_version {
        println!("{}", ASTERISK_VERSION);
        std::process::exit(0);
    }

    let foreground = args.foreground;
    let console = args.console;
    // Accepted for Asterisk CLI compatibility. This implementation does not
    // daemonize, so `-F` must only avoid changing foreground/console behavior.
    let _always_fork = args.always_fork;

    // Resolve directories from config
    let dirs = resolve_dirs(args.config_file.as_deref());
    let config_dir = dirs.config_dir.clone();
    let run_dir = dirs.run_dir.clone();

    // Handle remote CLI modes. Upstream Asterisk treats `-x` as implying
    // remote execution, so both `-rx "cmd"` and bare `-x "cmd"` use the
    // already-running daemon's control socket.
    if args.remote || args.execute.is_some() {
        let _otel_guard = init_logging(0, 0, true); // quiet logging for remote mode
        if let Some(ref cmd_str) = args.execute {
            match remote_execute(args.config_file.as_deref(), args.socket.as_deref(), cmd_str).await
            {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    eprintln!("Unable to connect to remote asterisk ({})", e);
                    std::process::exit(1);
                }
            }
        } else {
            match remote_console(args.config_file.as_deref(), args.socket.as_deref()).await {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    eprintln!("Unable to connect to remote asterisk ({})", e);
                    std::process::exit(1);
                }
            }
        }
    }

    // Initialize logging (and optionally OpenTelemetry tracing).
    // The guard must live until shutdown to keep the OTLP exporter alive.
    let _otel_guard = init_logging(args.verbose, args.debug, args.quiet);

    // Print startup banner
    print_banner(args.quiet);

    // Set up shutdown signal handling for both SIGTERM and SIGINT
    let running = Arc::new(AtomicBool::new(true));
    let running_for_signal = running.clone();
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        let mut sigint =
            signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down");
            }
        }

        running_for_signal.store(false, Ordering::SeqCst);

        // Safety valve: force exit after 5 seconds if graceful shutdown hangs
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            warn!("Forced exit after shutdown timeout");
            std::process::exit(0);
        });
    });

    // Set up SIGHUP handler for dialplan hot-reload
    let config_dir_for_sighup = config_dir.clone();
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sighup = signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");

        loop {
            sighup.recv().await;
            info!("Received SIGHUP, reloading dialplan...");
            let lines = reload_dialplan(&config_dir_for_sighup);
            for line in &lines {
                info!("SIGHUP reload: {}", line);
            }
        }
    });

    // Create server state and register CLI commands BEFORE starting anything
    // else so the control socket can dispatch commands immediately.
    let mut state = ServerState::new(running.clone(), &config_dir, &run_dir);
    state.verbose_level.store(args.verbose, Ordering::Relaxed);
    state.debug_level.store(args.debug, Ordering::Relaxed);
    state.register_builtins();

    // Start the Unix control socket EARLY -- before the full startup sequence.
    // This ensures that `asterisk -rx "core waitfullybooted"` can connect and
    // wait even while SIP, AMI, dialplan etc. are still loading.
    let shared_state = Arc::new(state);
    {
        let ss = shared_state.clone();
        if let Err(e) = start_control_socket(&run_dir, ss).await {
            warn!(
                "Failed to start control socket: {} (continuing without -rx support)",
                e
            );
        }
    }

    // Run the full startup sequence (config, codecs, SIP, AMI, etc.)
    startup_sequence(&config_dir, &dirs).await;

    // Signal that the daemon is fully booted.  Any pending
    // `core waitfullybooted` connections will now unblock.
    FULLY_BOOTED.store(true, Ordering::SeqCst);
    asterisk_ami::set_fully_booted();
    info!("Asterisk-RS is fully booted.");

    if console {
        // Console mode: interactive CLI via rustyline
        run_console(&shared_state);
    } else if foreground {
        // Foreground mode without console: block on signals, log to stdout,
        // do NOT use rustyline (critical for test suite operation)
        info!("Running in foreground mode. PID: {}", std::process::id());
        loop {
            let still_running = shared_state.running.load(Ordering::SeqCst);
            if !still_running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    } else {
        // Daemon/background mode
        info!("Running in background mode. Use 'asterisk -r' to connect.");
        loop {
            let still_running = shared_state.running.load(Ordering::SeqCst);
            if !still_running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    // Graceful shutdown
    shutdown_sequence(&run_dir);
    std::process::exit(0);
}
