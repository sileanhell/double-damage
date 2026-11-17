//! End-to-end checks. The llama-server one is ignored by default; run it with
//! `DD_TEST_LLAMA_EXE=… DD_TEST_LLAMA_MODEL=… cargo test -p dd-core e2e -- --ignored --nocapture`.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::{
    Core,
    llama::{LaunchSpec, State, files},
    paths::Paths,
    server,
};

async fn start(core: Core) -> (String, reqwest::Client) {
    let (listener, addr) = server::bind("127.0.0.1", 0).await.unwrap();
    let app = server::router(core.clone()).unwrap();
    let stop = core.on_shutdown();
    tokio::spawn(async move {
        let _ = server::serve(listener, app, async move {
            let _ = stop.await;
        })
        .await;
    });
    (format!("http://127.0.0.1:{}", addr.port()), reqwest::Client::builder().no_proxy().build().unwrap())
}

async fn json_of(response: reqwest::Response) -> Value {
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_routes_without_models() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::new(Paths::new(dir.path()));
    let token = core.token().to_string();
    let (base, client) = start(core.clone()).await;

    assert_eq!(client.get(format!("{base}/health")).send().await.unwrap().status(), 200);
    assert_eq!(client.get(format!("{base}/dd/providers")).send().await.unwrap().status(), 401);

    let providers = json_of(client.get(format!("{base}/dd/providers")).bearer_auth(&token).send().await.unwrap()).await;
    let ids: Vec<_> = providers.as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(ids, ["llama-server", "claude", "devin"]);
    assert_eq!(providers[1]["signedIn"], false);

    // Nothing loaded, nobody signed in: empty model list, no accounts.
    let models = json_of(client.get(format!("{base}/v1/models")).header("x-api-key", &token).send().await.unwrap()).await;
    assert_eq!(models["data"], json!([]));
    let accounts = json_of(client.get(format!("{base}/dd/accounts")).bearer_auth(&token).send().await.unwrap()).await;
    assert_eq!(accounts, json!({}));
    let r = client.get(format!("{base}/dd/providers/claude/account")).bearer_auth(&token).send().await.unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(json_of(r).await["error"]["message"], "not signed in");

    // Loading needs llama-server.
    let r = client.post(format!("{base}/dd/llama/load")).bearer_auth(&token).json(&json!({ "model": "x.gguf" })).send().await.unwrap();
    assert_eq!(r.status(), 400);

    // A new token locks the old one out.
    let answer = json_of(client.post(format!("{base}/dd/token/regenerate")).bearer_auth(&token).send().await.unwrap()).await;
    let fresh = answer["token"].as_str().unwrap().to_owned();
    assert_ne!(fresh, token);
    assert_eq!(client.get(format!("{base}/dd/info")).bearer_auth(&token).send().await.unwrap().status(), 401);
    assert_eq!(client.get(format!("{base}/dd/info")).bearer_auth(&fresh).send().await.unwrap().status(), 200);
    // …and survives a restart.
    assert_eq!(crate::store::Store::open(&Paths::new(dir.path())).token(), fresh);

    core.request_shutdown();
}

/// Prints what the template heuristics make of every model in `DD_TEST_MODELS_DIR`.
#[test]
#[ignore]
fn caps_of_local_models() {
    let dir = std::env::var("DD_TEST_MODELS_DIR").expect("DD_TEST_MODELS_DIR");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "gguf") {
            continue;
        }
        let meta = crate::gguf::read_meta(&path).unwrap();
        let t = meta.chat_template.as_deref();
        println!("== {:?} arch={:?} template={} bytes", path.file_name().unwrap(), meta.info_arch(), t.map_or(0, str::len));
        println!("   caps: {:?}", crate::caps::from_template(t));
        println!("   sampling: {:?}", meta.sampling);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn e2e_load_route_unload() {
    let exe = PathBuf::from(std::env::var("DD_TEST_LLAMA_EXE").expect("DD_TEST_LLAMA_EXE"));
    let model = PathBuf::from(std::env::var("DD_TEST_LLAMA_MODEL").expect("DD_TEST_LLAMA_MODEL"));

    let dir = tempfile::tempdir().unwrap();
    let core = Core::new(Paths::new(dir.path()));
    let token = core.token().to_string();
    let (base, client) = start(core.clone()).await;

    let spec = LaunchSpec {
        model: model.to_string_lossy().into_owned(),
        ctx: Some(4096),
        device: std::env::var("DD_TEST_LLAMA_DEVICE").ok().or(Some("cpu".into())),
        gpu_layers: Some(999),
        ..Default::default()
    };
    // Flags the core owns are refused before anything starts.
    let bad = LaunchSpec { extra_args: vec!["--host".into(), "0.0.0.0".into()], ..spec.clone() };
    assert!(core.llama.load(exe.clone(), model.clone(), bad, Default::default()).await.is_err());

    let meta = core.meta.file_meta(&model);
    let t = std::time::Instant::now();
    let info = core.llama.load(exe, model.clone(), spec, meta).await.expect("load");
    println!("loaded {} in {:?}, caps: {:?}", info.id, t.elapsed(), info.capabilities);
    assert_eq!(info.state, State::Ready);
    println!("inspect: {:?}", files::resolve(&core.paths, &model.to_string_lossy()));

    let models = json_of(client.get(format!("{base}/v1/models")).bearer_auth(&token).send().await.unwrap()).await;
    println!("models: {models}");
    assert_eq!(models["data"][0]["id"], info.id);

    let body = json!({
        "model": info.id,
        "max_tokens": 40,
        "stream": true,
        "messages": [{ "role": "user", "content": "Say hello in five words." }],
    });
    let r = client.post(format!("{base}/v1/messages")).header("x-api-key", &token).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let text = String::from_utf8(r.bytes().await.unwrap().to_vec()).unwrap();
    assert!(text.contains("message_stop"), "{text}");

    let catalog = json_of(client.get(format!("{base}/dd/catalog")).bearer_auth(&token).send().await.unwrap()).await;
    println!("catalog: {catalog}");

    assert!(core.llama.unload(&info.id).await);
    let models = json_of(client.get(format!("{base}/v1/models")).bearer_auth(&token).send().await.unwrap()).await;
    assert_eq!(models["data"], json!([]));
    core.request_shutdown();
}
