//! End-to-end check against a real llama-server. Ignored by default; run with
//! `DD_TEST_LLAMA_EXE=… DD_TEST_LLAMA_MODEL=… cargo test gateway -- --ignored --nocapture`.

use std::{path::PathBuf, sync::{Arc, Mutex}};

use super::{llama::{LaunchSpec, Llama, State}, server};

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
        println!("   caps: {:?}", super::caps::from_template(t));
        println!("   sampling: {:?}", meta.sampling);
        if let Some(t) = t {
            for word in ["enable_thinking", "reasoning_effort", "<think>", "reasoning_content"] {
                if let Some(at) = t.find(word) {
                    let from = t[..at].char_indices().rev().nth(60).map_or(0, |(i, _)| i);
                    let to = t[at..].char_indices().nth(60).map_or(t.len(), |(i, _)| at + i);
                    println!("   {word}: …{}…", t[from..to].replace('\n', "⏎"));
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn load_route_unload() {
    let exe = PathBuf::from(std::env::var("DD_TEST_LLAMA_EXE").expect("DD_TEST_LLAMA_EXE"));
    let model = PathBuf::from(std::env::var("DD_TEST_LLAMA_MODEL").expect("DD_TEST_LLAMA_MODEL"));
    let file = model.file_name().unwrap().to_string_lossy().into_owned();

    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let llama = Llama::new(move |list| {
        let states: Vec<_> = list.iter().map(|i| format!("{:?}/{}", i.state, i.active)).collect();
        seen.lock().unwrap().push(states.join(","));
    });
    let models_dir = model.parent().unwrap().to_path_buf();
    let exe_path = exe.clone();
    let source = super::catalog::Source::new(llama.clone(), move || super::catalog::Paths {
        models_dir: models_dir.clone(),
        llama_exe: Some(exe_path.clone()),
    });
    let server = server::start(0, "test-token".into(), llama.clone(), source).await.unwrap();
    let base = format!("http://127.0.0.1:{}", server.port);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let spec = LaunchSpec {
        file: file.clone(),
        ctx: 4096,
        device: None,
        gpu_layers: 0,
        kv: "f16".into(),
        flash_attn: "auto".into(),
        threads: None,
    };
    // A bad spec is refused before anything starts.
    let bad = LaunchSpec { kv: "--host 0.0.0.0".into(), ..spec.clone() };
    assert!(llama.load(exe.clone(), model.clone(), bad).await.is_err());

    let gpu = std::env::var("DD_TEST_LLAMA_DEVICE").ok();
    let spec = LaunchSpec { gpu_layers: if gpu.is_some() { 999 } else { 0 }, device: gpu, ..spec };
    let t = std::time::Instant::now();
    let info = llama.load(exe, model, spec).await.expect("load");
    println!("loaded {} in {:?}, caps: {}", info.id, t.elapsed(), info.caps.unwrap_or_default());
    assert_eq!(info.state, State::Ready);

    // Auth.
    let r = client.get(format!("{base}/v1/models")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = client.get(format!("{base}/v1/models")).bearer_auth("test-token").send().await.unwrap();
    let models: serde_json::Value = serde_json::from_slice(&r.bytes().await.unwrap()).unwrap();
    println!("models: {models}");
    assert_eq!(models["data"][0]["id"], info.id);

    // Unknown model.
    let r = client
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", "test-token")
        .body(r#"{"model":"llama-server/nope","max_tokens":5,"messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // Streaming Anthropic Messages through the gateway.
    let body = serde_json::json!({
        "model": info.id,
        "max_tokens": 40,
        "stream": true,
        "messages": [{ "role": "user", "content": "Say hello in five words." }],
    });
    let r = client
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", "test-token")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    println!("content-type: {:?}", r.headers().get("content-type"));
    let text = String::from_utf8(r.bytes().await.unwrap().to_vec()).unwrap();
    assert!(text.contains("message_stop"), "{text}");
    println!("stream: {} bytes, {} deltas", text.len(), text.matches("content_block_delta").count());

    // OpenAI chat completions through the gateway.
    let body = serde_json::json!({
        "model": info.id,
        "max_tokens": 10,
        "messages": [{ "role": "user", "content": "Hi" }],
    });
    let r = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth("test-token")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // Scoped path: prefix optional, thinking switched off the Anthropic way.
    let short = info.id.strip_prefix("llama-server/").unwrap();
    let body = serde_json::json!({
        "model": short,
        "max_tokens": 60,
        "thinking": { "type": "disabled" },
        "messages": [{ "role": "user", "content": "What is 2+2?" }],
    });
    let r = client
        .post(format!("{base}/llama-server/v1/messages"))
        .header("x-api-key", "test-token")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let reply: serde_json::Value = serde_json::from_slice(&r.bytes().await.unwrap()).unwrap();
    let kinds: Vec<_> = reply["content"].as_array().unwrap().iter().map(|b| b["type"].as_str().unwrap().to_owned()).collect();
    println!("thinking disabled -> {kinds:?}");
    assert!(!kinds.contains(&"thinking".to_string()));
    let r = client.get(format!("{base}/other/v1/models")).bearer_auth("test-token").send().await.unwrap();
    assert_eq!(r.status(), 404);
    let r = client.get(format!("{base}/dd/catalog")).bearer_auth("test-token").send().await.unwrap();
    println!("catalog: {}", String::from_utf8_lossy(&r.bytes().await.unwrap()));

    llama.unload(&info.id).await;
    assert!(llama.list().is_empty());
    let r = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth("test-token")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    println!("events: {:#?}", events.lock().unwrap());
    server.stop();
}
