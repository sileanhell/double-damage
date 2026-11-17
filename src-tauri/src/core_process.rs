//! The core (`dd-core.exe`) as a child process: providers, sign-ins, local models and the
//! Anthropic/OpenAI-compatible API all live there; the app is a client of its HTTP API.
//!
//! The app starts `dd-core serve --data-dir <app data> --port <setting> --ready-json` and reads
//! the URL and token from the first stdout line. The exe is `DD_CORE_EXE` or `dd-core.exe`
//! next to `dd.exe`. It joins the app's job object, so it never outlives the app.

use std::{
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{data_dir::DataDir, store::Store};

/// Settings key of the port (written by the settings page).
const PORT_KEY: &str = "dd:gateway-port";
pub const DEFAULT_PORT: u16 = 11488;
/// Sent with the new `CoreInfo` whenever the core starts, stops or fails.
const EVENT: &str = "core-info";
const READY_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct CoreInfo {
    /// Base URL, e.g. `http://127.0.0.1:11488`; None while not running.
    pub url: Option<String>,
    pub port: u16,
    pub token: String,
    /// Why it isn't running (exe missing, port taken, crashed, …).
    pub error: Option<String>,
    pub version: Option<String>,
    /// The exe that was started.
    pub exe: Option<String>,
}

#[derive(Default)]
pub struct CoreProcess {
    info: Mutex<CoreInfo>,
    child: Mutex<Option<Child>>,
    /// Bumped on every start, so a watcher of an older process stays quiet.
    generation: Mutex<u64>,
}

fn port_setting(store: &Store) -> u16 {
    store.get(PORT_KEY).and_then(|p| p.trim().parse().ok()).filter(|p| *p > 0).unwrap_or(DEFAULT_PORT)
}

fn exe_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("DD_CORE_EXE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("dd-core.exe");
    candidate.is_file().then_some(candidate)
}

impl CoreProcess {
    pub fn info(&self) -> CoreInfo {
        self.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_info(&self, app: &AppHandle, info: CoreInfo) {
        *self.info.lock().unwrap_or_else(|e| e.into_inner()) = info.clone();
        let _ = app.emit(EVENT, info);
    }
}

/// Starts the core for the current data folder (stopping a running one first). Blocking:
/// waits until it is listening or has failed.
pub fn start(app: &AppHandle) -> CoreInfo {
    let state = app.state::<CoreProcess>();
    stop(app);
    let port = port_setting(&app.state::<Store>());
    let data = app.state::<DataDir>().get();
    let generation = {
        let mut generation = state.generation.lock().unwrap_or_else(|e| e.into_inner());
        *generation += 1;
        *generation
    };

    let info = match launch(&data, port) {
        Ok((child, info)) => {
            *state.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
            watch(app.clone(), generation);
            info
        }
        Err((error, exe)) => CoreInfo { port, error: Some(error), exe, ..Default::default() },
    };
    state.set_info(app, info.clone());
    info
}

fn launch(data: &Path, port: u16) -> Result<(Child, CoreInfo), (String, Option<String>)> {
    let exe = exe_path().ok_or_else(|| ("dd-core.exe was not found next to the app (or in DD_CORE_EXE)".to_string(), None))?;
    let shown = Some(exe.to_string_lossy().into_owned());
    let fail = |error: String| (error, shown.clone());

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = Command::new(&exe)
        .arg("--data-dir")
        .arg(data)
        .args(["serve", "--port", &port.to_string(), "--ready-json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| fail(format!("can't start {}: {e}", exe.display())))?;
    crate::job::adopt_std(&child);

    // The ready line, read on a thread so a hung start can time out.
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Some(stdout) = stdout else { return };
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
        // Keep draining so the core never blocks on a full pipe.
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    let line = rx.recv_timeout(READY_TIMEOUT).unwrap_or_default();
    let ready: Option<serde_json::Value> = serde_json::from_str(line.trim()).ok();
    let Some(ready) = ready.filter(|r| r["event"] == "ready") else {
        let _ = child.kill();
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        let reason = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).last().map(str::to_owned);
        return Err(fail(reason.unwrap_or_else(|| "dd-core didn't start".into())));
    };
    // Drain stderr into the app's log.
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[dd-core] {line}");
            }
        });
    }
    let text = |key: &str| ready[key].as_str().map(str::to_owned);
    Ok((
        child,
        CoreInfo {
            url: text("url"),
            port: ready["port"].as_u64().map_or(port, |p| p as u16),
            token: text("token").unwrap_or_default(),
            error: None,
            version: text("version"),
            exe: shown,
        },
    ))
}

/// Reports an unexpected exit of the process started as `generation`.
fn watch(app: AppHandle, generation: u64) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let state = app.state::<CoreProcess>();
            if *state.generation.lock().unwrap_or_else(|e| e.into_inner()) != generation {
                return;
            }
            let status = {
                let mut child = state.child.lock().unwrap_or_else(|e| e.into_inner());
                match child.as_mut().map(|c| c.try_wait()) {
                    None => return,
                    Some(Ok(Some(status))) => {
                        child.take();
                        status
                    }
                    Some(_) => continue,
                }
            };
            let mut info = state.info();
            info.url = None;
            info.error = Some(format!("dd-core exited ({status})"));
            state.set_info(&app, info);
            return;
        }
    });
}

/// Asks the core to stop (so it can stop its models and clean up), then makes sure it's gone.
pub fn stop(app: &AppHandle) {
    let state = app.state::<CoreProcess>();
    *state.generation.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    let Some(mut child) = state.child.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    let info = state.info();
    if let Some(url) = &info.url {
        request_shutdown(url, &info.token);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut info = state.info();
    info.url = None;
    state.set_info(app, info);
}

/// `POST /dd/shutdown` over a plain socket (no runtime needed, works during app exit).
fn request_shutdown(url: &str, token: &str) {
    use std::io::Write;
    let Some(addr) = url.strip_prefix("http://") else { return };
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(
        &match addr.parse() {
            Ok(addr) => addr,
            Err(_) => return,
        },
        Duration::from_secs(1),
    ) else {
        return;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let request = format!(
        "POST /dd/shutdown HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(request.as_bytes());
    let _ = stream.read(&mut [0u8; 256]);
}

// --- commands ------------------------------------------------------------------

#[tauri::command]
pub fn core_info(state: State<'_, CoreProcess>) -> CoreInfo {
    state.info()
}

/// Restarts the core (e.g. on the port from settings).
#[tauri::command]
pub async fn core_restart(app: AppHandle) -> Result<CoreInfo, String> {
    tauri::async_runtime::spawn_blocking(move || start(&app)).await.map_err(|e| e.to_string())
}

/// The page replaced the token through the core's API; keep the copy handed out in step.
#[tauri::command]
pub fn core_set_token(app: AppHandle, state: State<'_, CoreProcess>, token: String) -> CoreInfo {
    let mut info = state.info();
    info.token = token;
    state.set_info(&app, info.clone());
    info
}
