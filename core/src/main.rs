//! `dd-core`: run the core (`serve`) or drive a running one.

use std::{io::Write as _, path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use dd_core::{Core, ServerInfo, VERSION, client::Client, paths::Paths, server, store::Config};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "dd-core", version, about = "Double Damage core: model providers behind a local Anthropic/OpenAI-compatible API")]
struct Cli {
    /// Data folder (default: DD_DATA_DIR, else the folder dd-core.exe is in).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Server to talk to (default: the one running for the data folder).
    #[arg(long, global = true, env = "DD_CORE_URL", value_name = "URL")]
    url: Option<String>,
    /// API token (default: read from the data folder).
    #[arg(long, global = true, env = "DD_CORE_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Print raw JSON answers.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (the API and everything the other commands use).
    Serve {
        /// Address to listen on (default from config.toml: 127.0.0.1).
        #[arg(long)]
        host: Option<String>,
        /// Port (default from config.toml: 11488; 0 picks a free one).
        #[arg(long, short)]
        port: Option<u16>,
        /// Print the ready line as JSON, token included (for programs starting the core).
        #[arg(long)]
        ready_json: bool,
    },
    /// Server, providers and loaded models at a glance.
    Status,
    /// Print the API token, or replace it.
    Token {
        #[arg(long)]
        regenerate: bool,
    },
    /// Models the API serves right now (`/v1/models`).
    Models,
    /// Sign in to a provider (claude, devin).
    Login {
        provider: String,
        /// Don't open the browser; just print the sign-in link.
        #[arg(long)]
        no_browser: bool,
    },
    /// Sign out of a provider.
    Logout { provider: String },
    /// Plan, balance and usage limits of the signed-in accounts.
    Usage {
        /// Just this provider.
        provider: Option<String>,
        /// Ask the provider again instead of using a recent answer.
        #[arg(long)]
        refresh: bool,
    },
    /// Install, remove or inspect llama-server.
    Llama {
        #[command(subcommand)]
        command: LlamaCommand,
    },
    /// Start a local model (downloads it first when given a URL).
    Load(LoadArgs),
    /// Stop a local model.
    Unload {
        /// Model id, with or without the `llama-server/` prefix.
        id: String,
    },
    /// Running local models.
    Ps,
    /// What a model file says about itself (context, tools, reasoning, sampling).
    Inspect { model: String },
    /// Stop the server.
    Shutdown,
}

#[derive(Subcommand)]
enum LlamaCommand {
    /// Installed build, or the build an install would pick.
    Status,
    /// Download llama-server from the llama.cpp releases.
    Install {
        /// Only these backends: cuda13, cuda12, vulkan, opencl, cpu (default: what the GPUs need).
        #[arg(long, value_delimiter = ',')]
        backend: Vec<String>,
        /// A release tag such as b6500 (default: the newest with the needed builds).
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove llama-server (stops the running models).
    Uninstall,
    /// Devices llama-server can run models on.
    Devices,
    /// GGUF files in the models folder.
    Models,
}

#[derive(Args)]
struct LoadArgs {
    /// https:// URL, hf:owner/repo/file.gguf, a path, or a file in the models folder.
    model: String,
    /// Model id (default: the file name without .gguf); served as llama-server/<id>.
    #[arg(long)]
    id: Option<String>,
    /// Context size.
    #[arg(long, short = 'c')]
    ctx: Option<u32>,
    /// Device: CUDA0, Vulkan1, … (see `dd-core llama devices`) or cpu.
    #[arg(long, short = 'd')]
    device: Option<String>,
    /// Layers on the GPU.
    #[arg(long = "gpu-layers", visible_alias = "ngl")]
    gpu_layers: Option<u32>,
    /// KV cache type: f16, q8_0, q4_0, …
    #[arg(long)]
    kv: Option<String>,
    /// Flash attention: auto, on, off.
    #[arg(long = "flash-attn", visible_alias = "fa")]
    flash_attn: Option<String>,
    /// CPU threads.
    #[arg(long, short = 't')]
    threads: Option<u32>,
    /// Parallel slots (default 1).
    #[arg(long = "parallel", visible_alias = "np")]
    parallel: Option<u32>,
    /// Anything else, passed to llama-server as is (after `--`).
    #[arg(last = true)]
    extra: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let paths = Paths::resolve(cli.data_dir.clone());
    let result = match cli.command {
        Command::Serve { host, port, ready_json } => serve(paths, host, port, ready_json).await,
        ref command => run_client(&cli, &paths, command).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dd-core: {error}");
            ExitCode::FAILURE
        }
    }
}

// --- serve ---------------------------------------------------------------------

async fn serve(paths: Paths, host: Option<String>, port: Option<u16>, ready_json: bool) -> Result<(), String> {
    let config = Config::load(&paths);
    let host = host.unwrap_or(config.server.host);
    let port = port.unwrap_or(config.server.port);
    let core = Core::new(paths.clone());
    let (listener, addr) = server::bind(&host, port).await?;
    let shown_host = if host == "0.0.0.0" || host == "::" { "127.0.0.1".to_string() } else { host.clone() };
    let info = ServerInfo {
        url: format!("http://{shown_host}:{}", addr.port()),
        port: addr.port(),
        pid: std::process::id(),
        version: VERSION.into(),
        data_dir: paths.data.to_string_lossy().into_owned(),
    };
    dd_core::paths::write_atomic(&paths.server_file(), &serde_json::to_vec_pretty(&info).unwrap_or_default())?;

    if ready_json {
        let mut ready = serde_json::to_value(&info).unwrap_or_default();
        ready["event"] = json!("ready");
        ready["token"] = json!(&*core.token());
        println!("{ready}");
    } else {
        println!("dd-core {VERSION} listening on {} (data: {})", info.url, info.data_dir);
        println!("API token: `dd-core token`; Anthropic base URL: {}", info.url);
    }
    let _ = std::io::stdout().flush();

    core.refresh_remote_models();
    let app = server::router(core.clone())?;
    let requested = core.on_shutdown();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let mut running = tokio::spawn(server::serve(listener, app, async move {
        let _ = stop_rx.await;
    }));
    let result = tokio::select! {
        finished = &mut running => finished.map_err(|e| e.to_string()).and_then(|r| r),
        _ = async {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = requested => {}
            }
        } => {
            let _ = stop_tx.send(());
            // Open event streams would keep a graceful shutdown waiting forever.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), running).await;
            Ok(())
        }
    };
    core.stop_models();
    // Only remove the file if it still describes this process.
    let ours = std::fs::read(paths.server_file())
        .ok()
        .and_then(|b| serde_json::from_slice::<ServerInfo>(&b).ok())
        .is_some_and(|i| i.pid == std::process::id());
    if ours {
        let _ = std::fs::remove_file(paths.server_file());
    }
    result
}

// --- client commands -------------------------------------------------------------

async fn run_client(cli: &Cli, paths: &Paths, command: &Command) -> Result<(), String> {
    let client = Client::locate(paths, cli.url.clone(), cli.token.clone())?;
    let print = |value: &Value| println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
    match command {
        Command::Serve { .. } => unreachable!(),
        Command::Status => {
            let info = client.get("/dd/info").await?;
            let providers = client.get("/dd/providers").await?;
            let instances = client.get("/dd/llama/instances").await?;
            if cli.json {
                print(&json!({ "info": info, "providers": providers, "instances": instances }));
                return Ok(());
            }
            println!("dd-core {} at {} (pid {}, data: {})", text(&info["version"]), client.base, info["pid"], text(&info["dataDir"]));
            for p in providers.as_array().into_iter().flatten() {
                let state = match (p["kind"].as_str(), p["signedIn"].as_bool()) {
                    (Some("local"), Some(true)) => "installed",
                    (Some("local"), _) => "not installed",
                    (_, Some(true)) => "signed in",
                    _ => "signed out",
                };
                println!("  {:<14} {:<14} {} models", text(&p["name"]), state, p["models"]);
            }
            print_instances(&instances);
        }
        Command::Token { regenerate } => {
            if *regenerate {
                let answer = client.post("/dd/token/regenerate", json!({})).await?;
                println!("{}", text(&answer["token"]));
            } else {
                let text = std::fs::read_to_string(paths.credentials()).map_err(|e| e.to_string())?;
                let creds: dd_core::store::Credentials = toml::from_str(&text).map_err(|e| e.to_string())?;
                println!("{}", creds.gateway.token);
            }
        }
        Command::Models => {
            let models = client.get("/v1/models").await?;
            if cli.json {
                print(&models);
            } else {
                for m in models["data"].as_array().into_iter().flatten() {
                    let reasoning = text(&m["capabilities"]["reasoning"]);
                    let tools = if m["capabilities"]["tools"].as_bool() == Some(true) { "tools" } else { "" };
                    println!("{:<48} ctx {:<9} {reasoning:<8} {tools}", text(&m["id"]), m["context_length"]);
                }
            }
        }
        Command::Login { provider, no_browser } => login(&client, provider, *no_browser).await?,
        Command::Logout { provider } => {
            let status = client.post(&format!("/dd/providers/{provider}/logout"), json!({})).await?;
            if cli.json { print(&status) } else { println!("signed out of {provider}") }
        }
        Command::Usage { provider, refresh } => {
            let query = if *refresh { "?refresh=1" } else { "" };
            let accounts = match provider {
                Some(id) => {
                    let account = client.get(&format!("/dd/providers/{id}/account{query}")).await?;
                    json!({ id.clone(): account })
                }
                None => client.get(&format!("/dd/accounts{query}")).await?,
            };
            if cli.json {
                print(&accounts);
            } else if accounts.as_object().is_none_or(|a| a.is_empty()) {
                println!("no provider is signed in (dd-core login claude|devin)");
            } else {
                for account in accounts.as_object().into_iter().flat_map(|a| a.values()) {
                    print_account(account);
                }
            }
        }
        Command::Llama { command } => match command {
            LlamaCommand::Status => {
                let status = client.get("/dd/llama").await?;
                if cli.json {
                    print(&status);
                } else if status["installed"].as_bool() == Some(true) {
                    println!("llama-server {} ({}) for {}", text(&status["version"]), text(&status["backend"]), text(&status["gpu"]));
                } else {
                    println!("llama-server is not installed; an install would pick {} for {}", text(&status["backend"]), text(&status["gpu"]));
                }
            }
            LlamaCommand::Install { backend, version } => {
                let body = json!({ "backends": (!backend.is_empty()).then_some(backend), "version": version });
                let status = with_progress(&client, client.post("/dd/llama/install", body)).await?;
                if cli.json { print(&status) } else { println!("installed llama-server {} ({})", text(&status["version"]), text(&status["backend"])) }
            }
            LlamaCommand::Uninstall => {
                client.post("/dd/llama/uninstall", json!({})).await?;
                println!("llama-server removed");
            }
            LlamaCommand::Devices => {
                let devices = client.get("/dd/llama/devices").await?;
                if cli.json {
                    print(&devices);
                } else {
                    for d in devices.as_array().into_iter().flatten() {
                        println!("{:<10} {} ({})", text(&d["id"]), text(&d["name"]), gib(d["memory"].as_f64()));
                    }
                    println!("{:<10} CPU only", "cpu");
                }
            }
            LlamaCommand::Models => {
                let files = client.get("/dd/llama/models").await?;
                if cli.json {
                    print(&files);
                } else {
                    for f in files.as_array().into_iter().flatten() {
                        println!(
                            "{:<56} {:>9}  ctx {:<8} {}",
                            text(&f["file"]),
                            gib(f["size"].as_f64()),
                            f["contextLength"],
                            text(&f["capabilities"]["reasoning"])
                        );
                    }
                }
            }
        },
        Command::Load(args) => {
            let spec = json!({
                "model": args.model,
                "id": args.id,
                "ctx": args.ctx,
                "device": args.device,
                "gpuLayers": args.gpu_layers,
                "kv": args.kv,
                "flashAttn": args.flash_attn,
                "threads": args.threads,
                "parallel": args.parallel,
                "extraArgs": args.extra,
            });
            eprintln!("loading {} …", args.model);
            let instance = with_progress(&client, client.post("/dd/llama/load", spec)).await?;
            if cli.json {
                print(&instance);
            } else {
                println!(
                    "{} is ready (ctx {}, reasoning: {}, tools: {})",
                    text(&instance["id"]),
                    instance["loadedContext"],
                    text(&instance["capabilities"]["reasoning"]),
                    instance["capabilities"]["tools"]
                );
            }
        }
        Command::Unload { id } => {
            client.post("/dd/llama/unload", json!({ "id": id })).await?;
            println!("{id} unloaded");
        }
        Command::Ps => {
            let instances = client.get("/dd/llama/instances").await?;
            if cli.json { print(&instances) } else { print_instances(&instances) }
        }
        Command::Inspect { model } => {
            let inspection = client.post("/dd/llama/inspect", json!({ "model": model })).await?;
            print(&inspection);
        }
        Command::Shutdown => {
            client.post("/dd/shutdown", json!({})).await?;
            println!("dd-core is stopping");
        }
    }
    Ok(())
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

fn gib(bytes: Option<f64>) -> String {
    bytes.map_or("-".into(), |b| format!("{:.1} GiB", b / (1u64 << 30) as f64))
}

fn print_instances(instances: &Value) {
    let list = instances.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no local models loaded");
    }
    for i in list {
        let state = match i["state"]["kind"].as_str() {
            Some("failed") => format!("failed: {}", text(&i["state"]["error"]).lines().next().unwrap_or("")),
            Some(kind) => kind.to_string(),
            None => "-".into(),
        };
        println!("{:<48} {:<10} ctx {:<8} active {}", text(&i["id"]), state, i["loadedContext"], i["active"]);
    }
}

fn print_account(account: &Value) {
    let provider = text(&account["provider"]);
    if let Some(error) = account["error"].as_str() {
        println!("{provider}: {error}");
        return;
    }
    let plan = &account["plan"];
    let mut head = vec![provider, text(&plan["name"])];
    if let Some(tier) = plan["tier"].as_str() {
        head.push(format!("({tier})"));
    }
    if let Some(email) = account["email"].as_str() {
        head.push(format!("· {email}"));
    }
    if let Some(until) = plan["expiresAt"].as_str() {
        head.push(format!("· until {until}"));
    }
    if let Some(trial) = plan["trialEndsAt"].as_str() {
        head.push(format!("· trial until {trial}"));
    }
    println!("{}", head.join(" "));
    for limit in account["limits"].as_array().into_iter().flatten() {
        let used = limit["usedPercent"].as_f64().map_or("-".into(), |p| format!("{p:.0}%"));
        let reset = limit["resetsAt"].as_str().map(|r| format!("resets {r}")).unwrap_or_default();
        println!("  {:<22} {used:>5} used  {reset}", text(&limit["label"]));
    }
    if let Some(balance) = account["balance"].as_object() {
        let money = |v: &Value| v.as_f64().map_or("-".into(), |v| format!("{v:.2}"));
        let currency = balance.get("currency").map(text).unwrap_or_default();
        let enabled = if balance.get("enabled").and_then(Value::as_bool) == Some(true) { "on" } else { "off" };
        println!(
            "  extra usage ({enabled}): {} / {} {currency} used",
            money(balance.get("used").unwrap_or(&Value::Null)),
            money(balance.get("limit").unwrap_or(&Value::Null)),
        );
    }
}

/// Runs `request` while printing task progress (downloads, installs) from the event stream.
async fn with_progress(client: &Client, request: impl std::future::Future<Output = Result<Value, String>>) -> Result<Value, String> {
    let progress = client.events(|name, data| {
        if name != "task" {
            return;
        }
        let label = text(&data["label"]);
        let received = data["received"].as_f64().unwrap_or(0.0);
        match data["total"].as_f64().filter(|t| *t > 0.0) {
            Some(total) => eprint!("\r{label}: {:.0}% of {}   ", received / total * 100.0, gib(Some(total))),
            None => eprint!("\r{label}: {}   ", gib(Some(received))),
        }
        if data["done"].as_bool() == Some(true) {
            eprintln!();
        }
    });
    tokio::select! {
        result = request => result,
        // The event stream only ends with the server.
        _ = progress => Err("lost the connection to dd-core".into()),
    }
}

async fn login(client: &Client, provider: &str, no_browser: bool) -> Result<(), String> {
    let path = format!("/dd/providers/{provider}/login");
    let request = client.post(&path, json!({ "open": !no_browser }));
    tokio::pin!(request);
    let mut shown = false;
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    loop {
        tokio::select! {
            result = &mut request => {
                let status = result?;
                match status["email"].as_str() {
                    Some(email) => println!("signed in to {provider} as {email}"),
                    None if status["signedIn"].as_bool() == Some(true) => println!("signed in to {provider}"),
                    None => println!("sign-in cancelled"),
                }
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(400)), if !shown => {
                let status = client.get(&format!("/dd/providers/{provider}")).await?;
                if let Some(url) = status["loginUrl"].as_str() {
                    shown = true;
                    println!("Sign in at:\n  {url}");
                    if let Some(manual) = status["manualUrl"].as_str() {
                        println!("If that page can't reach this machine, open this one and paste the code it shows here:\n  {manual}");
                    }
                }
            }
            read = tokio::io::AsyncBufReadExt::read_line(&mut stdin, &mut line), if shown && provider == "claude" => {
                if read.map_err(|e| e.to_string())? == 0 {
                    continue;
                }
                let code = line.trim().to_string();
                line.clear();
                if !code.is_empty()
                    && let Err(error) = client.post(&format!("/dd/providers/{provider}/code"), json!({ "code": code })).await
                {
                    eprintln!("{error}");
                }
            }
        }
    }
}
