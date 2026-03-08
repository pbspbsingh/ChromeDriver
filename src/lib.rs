mod sleepable;

pub use chromiumoxide;
pub use chromiumoxide::error::CdpError;
pub use chromiumoxide::{Browser, Element, Handler, Page};

use futures::StreamExt;
use log::{debug, info, warn};
pub use sleepable::Sleepable;
use std::path::PathBuf;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use thiserror::Error;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("No Chrome process with a remote debugging port found")]
    NoChromiumProcess,

    #[error("Remote debug argument missing from Chrome process command")]
    MissingDebugArg,

    #[error("Could not parse debug port from '{arg}': {source}")]
    InvalidDebugPort {
        arg: String,
        #[source]
        source: std::num::ParseIntError,
    },

    #[error("Failed to connect to Chrome at {url}: {source}")]
    ConnectFailed {
        url: String,
        #[source]
        source: CdpError,
    },

    #[error("Failed to spawn Chrome process: {0}")]
    SpawnFailed(#[source] std::io::Error),

    #[error("Could not capture Chrome stderr")]
    StderrUnavailable,

    #[error("Error reading Chrome stderr: {0}")]
    StderrReadFailed(#[source] std::io::Error),

    #[error("Chrome exited before printing a DevTools websocket URL on stderr")]
    DevToolsUrlNotFound,

    #[error(
        "Automatic browser launching is disabled. \
         Launch Chrome manually with:\n  {command}"
    )]
    AutoLaunchDisabled { command: String },

    #[error("Failed to query browser version: {0}")]
    VersionQueryFailed(#[source] CdpError),
}

// ── Config & builder ──────────────────────────────────────────────────────────

const REMOTE_DEBUG_ARG: &str = "--remote-debugging-port";

/// Configuration for connecting to (or spawning) a Chrome instance.
///
/// # Example
/// ```rust,no_run
/// # tokio_test::block_on(async {
/// use chrome_driver::ChromeDriverConfig;
///
/// let browser = ChromeDriverConfig::new("/usr/bin/google-chrome")
///     .user_data_dir("/tmp/chrome-profile")
///     .arg("--disable-gpu")
///     .arg("--headless=new")
///     .launch_if_needed(true)
///     .connect()
///     .await?;
/// # Ok::<_, chrome_driver::BrowserError>(())
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct ChromeDriverConfig {
    chrome_path: PathBuf,
    user_data_dir: Option<PathBuf>,
    /// Extra CLI args passed verbatim to Chrome when spawning a new process.
    /// Do not include `--remote-debugging-port`; it is injected automatically.
    chrome_args: Vec<String>,
    /// When `true`, a new Chrome process is spawned if none is found already
    /// running with a remote debugging port. Default: `false`.
    launch_if_needed: bool,
}

impl ChromeDriverConfig {
    /// Create a new config with the path to the Chrome/Chromium binary.
    pub fn new(chrome_path: impl Into<PathBuf>) -> Self {
        Self {
            chrome_path: chrome_path.into(),
            user_data_dir: None,
            chrome_args: Vec::new(),
            launch_if_needed: false,
        }
    }

    /// Directory Chrome uses to store its profile data.
    pub fn user_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.user_data_dir = Some(dir.into());
        self
    }

    /// Append a single extra Chrome CLI argument (e.g. `"--disable-gpu"`).
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.chrome_args.push(arg.into());
        self
    }

    /// Replace all extra Chrome CLI arguments at once.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.chrome_args = args.into_iter().map(Into::into).collect();
        self
    }

    /// When `true`, spawn a new Chrome process if none is already running with
    /// a remote debugging port. Default: `false`.
    pub fn launch_if_needed(mut self, value: bool) -> Self {
        self.launch_if_needed = value;
        self
    }

    /// Connect to an existing Chrome session, or spawn a new one if
    /// `launch_if_needed` is set.
    pub async fn connect(self) -> Result<Browser, BrowserError> {
        init_browser(self).await
    }
}

// ── Core logic ────────────────────────────────────────────────────────────────

async fn init_browser(config: ChromeDriverConfig) -> Result<Browser, BrowserError> {
    let (browser, mut handler) = match try_connect_existing_session().await {
        Ok(pair) => pair,
        Err(e) => {
            warn!("Could not attach to existing session ({e})");
            if config.launch_if_needed {
                info!("Spawning a new Chrome process...");
                start_new_session(&config).await?
            } else {
                let user_data = config
                    .user_data_dir
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let command = format!(
                    r#""{}" --user-data-dir="{}" {} {}=<port>"#,
                    config.chrome_path.display(),
                    user_data,
                    config.chrome_args.join(" "),
                    REMOTE_DEBUG_ARG,
                );
                return Err(BrowserError::AutoLaunchDisabled { command });
            }
        }
    };

    // Drive the CDP event loop in the background.
    tokio::spawn(async move {
        while let Some(h) = handler.next().await {
            if let Err(e) = h {
                warn!("Chrome handler error: {e}");
                break;
            }
        }
    });

    let version = browser
        .version()
        .await
        .map_err(BrowserError::VersionQueryFailed)?;

    info!(
        "Connected: {} ({})",
        version.product,
        browser.websocket_address()
    );

    Ok(browser)
}

/// Scans running processes for a Chrome instance launched with
/// `--remote-debugging-port` and connects to it.
async fn try_connect_existing_session() -> Result<(Browser, Handler), BrowserError> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );

    debug!("Scanning {} processes for Chrome with debug port", sys.processes().len());

    let chrome_proc = sys
        .processes()
        .values()
        .filter(|p| {
            let cmd = p.cmd().iter().map(|s| s.to_string_lossy().to_lowercase()).collect::<Vec<_>>().join(" ");
            cmd.contains("chrome")
        })
        .find(|p| {
            let cmd = p.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            cmd.contains(REMOTE_DEBUG_ARG)
        })
        .ok_or(BrowserError::NoChromiumProcess)?;

    let full_cmd = chrome_proc.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
    debug!("Found Chrome process: {}", full_cmd);

    let debug_arg = full_cmd
        .split_whitespace()
        .find(|a| a.starts_with(REMOTE_DEBUG_ARG))
        .map(|s| s.to_owned())
        .ok_or(BrowserError::MissingDebugArg)?;

    let port_str = debug_arg
        .split('=')
        .nth(1)
        .map(str::trim)
        .unwrap_or("")
        .to_owned();

    let port = port_str
        .parse::<u16>()
        .map_err(|e| BrowserError::InvalidDebugPort {
            arg: debug_arg.clone(),
            source: e,
        })?;

    let url = format!("http://localhost:{port}");
    info!("Connecting to existing Chrome session at {url}");

    Browser::connect(&url)
        .await
        .map_err(|e| BrowserError::ConnectFailed { url, source: e })
}

/// Spawns a new Chrome process, waits for it to print its DevTools websocket
/// URL on stderr, then connects to it.
///
/// Chrome is started in its own process group (`process_group(0)`) so it
/// survives the parent process exiting — it will keep running until explicitly
/// closed or the machine reboots.
async fn start_new_session(
    config: &ChromeDriverConfig,
) -> Result<(Browser, Handler), BrowserError> {
    let port = find_free_port().await.unwrap_or(8888);

    let mut args: Vec<String> = config.chrome_args.clone();
    args.push(format!("{}={}", REMOTE_DEBUG_ARG, port));

    if let Some(dir) = &config.user_data_dir {
        args.push(format!("--user-data-dir={}", dir.display()));
    }

    let mut cmd = tokio::process::Command::new(&config.chrome_path);
    cmd.args(&args).stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().map_err(BrowserError::SpawnFailed)?;

    let stderr = child.stderr.take().ok_or(BrowserError::StderrUnavailable)?;
    let ws_url = wait_for_devtools_url(stderr).await?;

    info!("Chrome ready, connecting to {ws_url}");

    Browser::connect(&ws_url)
        .await
        .map_err(|e| BrowserError::ConnectFailed {
            url: ws_url,
            source: e,
        })
}

/// Reads Chrome's stderr line-by-line until it sees
/// `DevTools listening on ws://...` and returns the websocket URL.
/// This line is Chrome's signal that the debug port is bound and ready.
async fn wait_for_devtools_url(
    stderr: tokio::process::ChildStderr,
) -> Result<String, BrowserError> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    const DEVTOOLS_PREFIX: &str = "DevTools listening on ";

    let mut lines = BufReader::new(stderr).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(BrowserError::StderrReadFailed)?
    {
        debug!("[chrome stderr] {line}");
        if let Some(ws_url) = line.strip_prefix(DEVTOOLS_PREFIX) {
            return Ok(ws_url.trim().to_owned());
        }
    }

    Err(BrowserError::DevToolsUrlNotFound)
}

/// Binds to port 0 and lets the OS assign a free ephemeral port, then returns it.
async fn find_free_port() -> Option<u16> {
    tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}
