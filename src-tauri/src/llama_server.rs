//! Installing / uninstalling the llama.cpp `llama-server` binaries from GitHub releases.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::tasks::{
    CANCELLED, Progress, Tasks, err_str, http_client, remove_dir_quiet,
    storage_root, stream_to_file,
};

const TASK_ID: &str = "llama-server";
const TASK_KIND: &str = "llama-server";
const DIR_NAME: &str = "llama-server";
const INSTALL_FILE: &str = "install.json";
const EXE_NAME: &str = "llama-server.exe";
/// Newest builds first. `releases/latest` can't be used: it may point at a release without
/// Windows binaries (the numbered `bNNNN` builds are published as pre-releases).
const RELEASES_URL: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=15";

#[cfg(target_arch = "aarch64")]
const ARCH: &str = "arm64";
#[cfg(not(target_arch = "aarch64"))]
const ARCH: &str = "x64";

/// NVIDIA drivers needed by the CUDA builds (Windows minimums for CUDA 13.x / 12.4).
const CUDA13_DRIVER: f64 = 580.0;
const CUDA12_DRIVER: f64 = 551.61;

/// A llama.cpp GPU backend. Every Windows build of a release ships the same files plus its own
/// `ggml-<backend>.dll`, loaded at runtime, so several backends can live in one install.
/// Declaration order = which build is installed in full (the others only add their DLL).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Backend {
    Cuda13,
    Cuda12,
    Vulkan,
    OpenclAdreno,
    Cpu,
}

impl Backend {
    fn label(self) -> &'static str {
        match self {
            Self::Cuda13 => "CUDA 13",
            Self::Cuda12 => "CUDA 12",
            Self::Vulkan => "Vulkan",
            Self::OpenclAdreno => "OpenCL",
            Self::Cpu => "CPU",
        }
    }

    /// Part of the archive name after `llama-<tag>-bin-win-`, e.g. `cuda-13.` (version varies).
    fn asset_key(self) -> &'static str {
        match self {
            Self::Cuda13 => "cuda-13.",
            Self::Cuda12 => "cuda-12.",
            Self::Vulkan => "vulkan-",
            Self::OpenclAdreno => "opencl-adreno-",
            Self::Cpu => "cpu-",
        }
    }

    fn is_cuda(self) -> bool {
        matches!(self, Self::Cuda13 | Self::Cuda12)
    }

    /// The main archive in a release.
    fn find_asset(self, assets: &[Asset]) -> Option<&Asset> {
        let key = format!("-bin-win-{}", self.asset_key());
        let suffix = format!("-{ARCH}.zip");
        assets.iter().find(|a| a.name.starts_with("llama-") && a.name.contains(&key) && a.name.ends_with(&suffix))
    }

    /// CUDA builds need the CUDA runtime DLLs, shipped as a separate archive.
    fn find_runtime(self, assets: &[Asset]) -> Option<&Asset> {
        let key = format!("-win-{}", self.asset_key());
        let suffix = format!("-{ARCH}.zip");
        assets.iter().find(|a| a.name.starts_with("cudart-") && a.name.contains(&key) && a.name.ends_with(&suffix))
    }
}

/// CUDA 13 dropped GPUs older than Turing (GTX 16xx / RTX 20xx); spot them by name.
fn is_pre_turing(name: &str) -> bool {
    const OLD: [&str; 16] = [
        "GTX 9", "GTX 10", "GT 10", "GT 7", "TITAN X", "TITAN V", "QUADRO P", "QUADRO M", "QUADRO K",
        "TESLA P", "TESLA M", "TESLA K", "TESLA V", "V100", "P100", "P40",
    ];
    let name = name.to_uppercase();
    OLD.iter().any(|old| name.contains(old))
}

/// Backends for every GPU in the system: Vulkan for all of them, plus CUDA for NVIDIA (if the
/// driver is new enough) and OpenCL for Snapdragon; CPU without GPUs. The CPU code ships in
/// every build, so CPU is always available too.
struct Plan {
    /// Sorted; the first one is installed in full.
    backends: Vec<Backend>,
    gpus: Vec<String>,
}

impl Plan {
    fn detect() -> Self {
        use crate::hardware::{VENDOR_NVIDIA, VENDOR_QUALCOMM};
        let gpus = crate::hardware::gpus();

        // One CUDA version for all NVIDIA cards (both builds ship `ggml-cuda.dll`): the oldest
        // card and the shared driver decide.
        let nvidia: Vec<_> = gpus.iter().filter(|gpu| gpu.vendor == VENDOR_NVIDIA).collect();
        let driver = nvidia.iter().filter_map(|gpu| gpu.driver).fold(f64::INFINITY, f64::min);
        let cuda = if !driver.is_finite() {
            None
        } else if driver >= CUDA13_DRIVER && !nvidia.iter().any(|gpu| is_pre_turing(&gpu.name)) {
            Some(Backend::Cuda13)
        } else if driver >= CUDA12_DRIVER && ARCH == "x64" {
            Some(Backend::Cuda12)
        } else {
            None
        };

        // Vulkan runs on every GPU, so it is always there to choose; CUDA / OpenCL are added for
        // the cards that support them (usually faster).
        let mut backends = Vec::new();
        for gpu in &gpus {
            backends.push(Backend::Vulkan);
            if gpu.vendor == VENDOR_NVIDIA && let Some(cuda) = cuda {
                backends.push(cuda);
            }
            if VENDOR_QUALCOMM.contains(&gpu.vendor) && ARCH == "arm64" {
                backends.push(Backend::OpenclAdreno);
            }
        }
        backends.sort();
        backends.dedup();
        if backends.is_empty() {
            backends.push(Backend::Cpu);
        }
        Self { backends, gpus: gpus.into_iter().map(|gpu| gpu.name).collect() }
    }

    fn backend_label(&self) -> String {
        self.backends.iter().map(|backend| backend.label()).collect::<Vec<_>>().join(" + ")
    }

    fn gpu_label(&self) -> Option<String> {
        (!self.gpus.is_empty()).then(|| self.gpus.join(" + "))
    }

    /// Archives to download from `release` (each backend, plus the CUDA runtime), or None if
    /// the release lacks any of them.
    fn assets<'a>(&self, release: &'a Release) -> Option<Vec<&'a Asset>> {
        let mut assets = Vec::new();
        for backend in &self.backends {
            assets.push(backend.find_asset(&release.assets)?);
        }
        for backend in self.backends.iter().filter(|backend| backend.is_cuda()) {
            assets.push(backend.find_runtime(&release.assets)?);
        }
        Some(assets)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlamaServerStatus {
    installed: bool,
    version: Option<String>,
    /// Backends installed (e.g. "CUDA 13 + Vulkan"), or the ones an install would pick.
    backend: Option<String>,
    /// GPUs they were picked for.
    gpu: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallInfo {
    version: String,
    exe: String,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    gpu: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

fn read_install(dir: &Path) -> Option<InstallInfo> {
    let info: InstallInfo = serde_json::from_slice(&std::fs::read(dir.join(INSTALL_FILE)).ok()?).ok()?;
    (!info.exe.is_empty() && dir.join(&info.exe).is_file()).then_some(info)
}

/// The installed `llama-server.exe` under the storage root, if any.
pub fn installed_exe(root: &Path) -> Option<PathBuf> {
    let dir = root.join(DIR_NAME);
    read_install(&dir).map(|info| dir.join(info.exe))
}

/// Installed build, or (when not installed) the build an install would pick for this machine.
fn read_status(dir: &Path) -> LlamaServerStatus {
    match read_install(dir) {
        Some(info) => LlamaServerStatus {
            installed: true,
            version: Some(info.version),
            backend: info.backend,
            gpu: info.gpu,
        },
        None => {
            let plan = Plan::detect();
            LlamaServerStatus {
                installed: false,
                version: None,
                backend: Some(plan.backend_label()),
                gpu: plan.gpu_label(),
            }
        }
    }
}

#[tauri::command]
pub async fn llama_server_status(app: AppHandle) -> Result<LlamaServerStatus, String> {
    let dir = storage_root(&app)?.join(DIR_NAME);
    tauri::async_runtime::spawn_blocking(move || read_status(&dir)).await.map_err(err_str)
}

/// A device llama-server can run a model on, as listed by `llama-server --list-devices`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// What `--device` takes, e.g. "CUDA0", "Vulkan1".
    id: String,
    /// "CUDA", "Vulkan", …
    backend: String,
    name: String,
    /// Memory in bytes.
    memory: u64,
}

/// Parses `  CUDA0: NVIDIA GeForce RTX 4060 (8187 MiB, 7106 MiB free)`.
fn parse_device(line: &str) -> Option<Device> {
    let (id, rest) = line.trim().split_once(": ")?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let (name, memory) = rest.rsplit_once(" (").unwrap_or((rest, ""));
    let mib: u64 = memory.split(" MiB").next().and_then(|n| n.trim().parse().ok()).unwrap_or(0);
    Some(Device {
        id: id.into(),
        backend: id.trim_end_matches(|c: char| c.is_ascii_digit()).into(),
        name: name.trim().into(),
        memory: mib * 1024 * 1024,
    })
}

/// GPUs the installed llama-server can use, each once per backend (a card with CUDA and Vulkan
/// shows up twice). Empty when llama-server is not installed.
#[tauri::command]
pub async fn llama_server_devices(app: AppHandle) -> Result<Vec<Device>, String> {
    let dir = storage_root(&app)?.join(DIR_NAME);
    let Some(info) = read_install(&dir) else { return Ok(Vec::new()) };
    let exe = dir.join(&info.exe);
    tauri::async_runtime::spawn_blocking(move || {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = std::process::Command::new(&exe)
            .arg("--list-devices")
            .current_dir(exe.parent().unwrap_or(&dir))
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(err_str)?;
        let text = [output.stdout, output.stderr].concat();
        let text = String::from_utf8_lossy(&text);
        Ok(text
            .lines()
            .skip_while(|line| !line.starts_with("Available devices"))
            .skip(1)
            .filter_map(parse_device)
            .filter(|device| device.backend != "CPU")
            .collect())
    })
    .await
    .map_err(err_str)?
}

#[tauri::command]
pub async fn llama_server_uninstall(app: AppHandle) -> Result<(), String> {
    let dir = storage_root(&app)?.join(DIR_NAME);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(err_str(e)),
    }
}

#[tauri::command]
pub async fn llama_server_install(app: AppHandle) -> Result<LlamaServerStatus, String> {
    let guard = app.state::<Tasks>().start(TASK_ID)?;
    let root = storage_root(&app)?;
    let downloads = root.join(".llama-server-download");
    let tmp_dir = root.join(".llama-server-new");
    let mut progress = Progress::new(&app, &guard, TASK_KIND, DIR_NAME);

    let result = install_inner(&root, &downloads, &tmp_dir, &mut progress).await;
    progress.finish();

    remove_dir_quiet(&downloads);
    if result.is_err() {
        remove_dir_quiet(&tmp_dir);
    }
    drop(guard);
    result
}

async fn install_inner(
    root: &Path,
    downloads: &Path,
    tmp_dir: &Path,
    progress: &mut Progress,
) -> Result<LlamaServerStatus, String> {
    // Leftovers from an earlier crashed attempt.
    remove_dir_quiet(downloads);
    remove_dir_quiet(tmp_dir);
    tokio::fs::create_dir_all(downloads).await.map_err(err_str)?;

    let plan = tauri::async_runtime::spawn_blocking(Plan::detect).await.map_err(err_str)?;
    let label = plan.backend_label();

    let client = http_client()?;
    let releases: Vec<Release> = {
        let resp = client
            .get(RELEASES_URL)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(err_str)?
            .error_for_status()
            .map_err(|e| format!("failed to query llama.cpp releases: {e}"))?;
        let bytes = resp.bytes().await.map_err(err_str)?;
        serde_json::from_slice(&bytes).map_err(|e| format!("unexpected GitHub release response: {e}"))?
    };
    progress.check_cancel()?;

    // Newest release that has every needed archive (all from one build: the DLLs must match).
    let (release, assets) = releases
        .iter()
        .filter(|release| !release.draft)
        .find_map(|release| Some((release, plan.assets(release)?)))
        .ok_or_else(|| format!("no recent llama.cpp release has {label} builds for Windows {ARCH}"))?;
    progress.set_label(format!("{} В· {label}", release.tag_name));

    // All archives as one transfer.
    let total: u64 = assets.iter().map(|asset| asset.size).sum();
    let total = (total > 0).then_some(total);
    let mut received = 0;
    let mut files = Vec::new();
    for asset in &assets {
        let path = downloads.join(&asset.name);
        received = download(&client, asset, &path, progress, received, total).await?;
        files.push(path);
    }

    // The first archive is the full install; the others (other backends, CUDA runtime) only add
    // the files it lacks, next to the executable.
    let cancel = progress.cancel_flag();
    let tmp_owned = tmp_dir.to_path_buf();
    let exe_rel = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let (base, extras) = files.split_first().ok_or("nothing to install")?;
        extract_zip(base, &tmp_owned, &cancel)?;
        let exe = find_file(&tmp_owned, EXE_NAME)
            .ok_or_else(|| format!("{EXE_NAME} not found in the downloaded archive"))?;
        let exe_dir = exe.parent().ok_or("bad archive layout")?;
        for extra in extras {
            extract_missing(extra, exe_dir, &cancel)?;
        }
        let rel = exe.strip_prefix(&tmp_owned).map_err(err_str)?;
        Ok(rel.to_string_lossy().replace('\\', "/"))
    })
    .await
    .map_err(err_str)??;
    remove_dir_quiet(downloads);
    progress.check_cancel()?;

    let info = InstallInfo {
        version: release.tag_name.clone(),
        exe: exe_rel,
        backend: Some(label),
        gpu: plan.gpu_label(),
    };
    let json = serde_json::to_vec_pretty(&info).map_err(err_str)?;
    tokio::fs::write(tmp_dir.join(INSTALL_FILE), json)
        .await
        .map_err(err_str)?;
    progress.check_cancel()?;

    // Swap the fresh install into place.
    let dest = root.join(DIR_NAME);
    match tokio::fs::remove_dir_all(&dest).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("failed to remove previous llama-server install: {e}")),
    }
    tokio::fs::rename(tmp_dir, &dest)
        .await
        .map_err(err_str)?;

    Ok(read_status(&dest))
}

/// Streams one release asset to `path`, continuing the progress count from `offset`.
/// Returns the bytes received so far in the whole transfer.
async fn download(
    client: &reqwest::Client,
    asset: &Asset,
    path: &Path,
    progress: &mut Progress,
    offset: u64,
    total: Option<u64>,
) -> Result<u64, String> {
    let resp = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(err_str)?
        .error_for_status()
        .map_err(|e| format!("failed to download {}: {e}", asset.name))?;
    Ok(offset + stream_to_file(resp, path, progress, offset, total).await?)
}

fn extract_zip(zip_path: &Path, dest: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(err_str)?;
    let mut archive =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| format!("invalid zip: {e}"))?;
    std::fs::create_dir_all(dest).map_err(err_str)?;
    for i in 0..archive.len() {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        let mut entry = archive.by_index(i).map_err(err_str)?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(err_str)?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(err_str)?;
            }
            let mut f = std::fs::File::create(&out).map_err(err_str)?;
            std::io::copy(&mut entry, &mut f).map_err(err_str)?;
        }
    }
    Ok(())
}

/// Extracts the files of `zip_path` that `dest` doesn't have yet, flattened into `dest`.
fn extract_missing(zip_path: &Path, dest: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(err_str)?;
    let mut archive =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| format!("invalid zip: {e}"))?;
    for i in 0..archive.len() {
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        let mut entry = archive.by_index(i).map_err(err_str)?;
        let Some(name) = entry.enclosed_name().and_then(|path| path.file_name().map(PathBuf::from)) else {
            continue;
        };
        let out = dest.join(name);
        if entry.is_dir() || out.exists() {
            continue;
        }
        let mut f = std::fs::File::create(&out).map_err(err_str)?;
        std::io::copy(&mut entry, &mut f).map_err(err_str)?;
    }
    Ok(())
}

/// Breadth-first search for a file named `name` (case-insensitive) under `dir`.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut queue = vec![dir.to_path_buf()];
    let mut i = 0;
    while i < queue.len() {
        let current = queue[i].clone();
        i += 1;
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                subdirs.push(entry.path());
            } else if ft.is_file() && entry.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
                return Some(entry.path());
            }
        }
        subdirs.sort();
        queue.extend(subdirs);
    }
    None
}
