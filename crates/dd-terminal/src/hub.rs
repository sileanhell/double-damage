//! The provider hub: one tokio runtime under the synchronous TUI loop, the
//! `dd-providers` clients, and the state the settings menu and `/models`
//! show — login, account info and model catalogs. Background work reports
//! back over a channel the event loop drains between frames.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use dd_providers::{
    AccountInfo, ContentBlock, Effort, Message, MessagesRequest, ModelInfo, Provider, ProviderKind,
    StreamEvent,
};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

/// Something loaded in the background.
#[derive(Clone, Debug, Default)]
pub(crate) enum Load<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

/// A login waiting for the browser redirect.
#[derive(Debug)]
pub(crate) struct LoginWait {
    /// The page the browser was sent to, shown in case it did not open.
    pub(crate) url: String,
    task: Option<JoinHandle<()>>,
}

/// What the panel knows about one provider.
#[derive(Debug)]
pub(crate) struct ProviderEntry {
    pub(crate) kind: ProviderKind,
    pub(crate) authenticated: bool,
    pub(crate) login: Option<LoginWait>,
    pub(crate) login_error: Option<String>,
    pub(crate) account: Load<AccountInfo>,
    /// When the last account fetch finished — Ready or Failed alike: the
    /// [`ACCOUNT_TTL`] cache window `ensure_account` respects.
    account_at: Option<Instant>,
    /// A fetch is in flight. `account` keeps the old data meanwhile:
    /// views show it until the fresh one lands — no «загрузка…» flash.
    account_fetching: bool,
    pub(crate) models: Load<Vec<ModelInfo>>,
}

/// How long account info (limits, balance) stays fresh: every place that
/// shows it calls `ensure_account`, which serves the cached `Load` inside
/// the window instead of refetching.
const ACCOUNT_TTL: Duration = Duration::from_secs(60);

impl ProviderEntry {
    pub(crate) fn new(kind: ProviderKind, authenticated: bool) -> Self {
        Self {
            kind,
            authenticated,
            login: None,
            login_error: None,
            account: Load::Idle,
            account_at: None,
            account_fetching: false,
            models: Load::Idle,
        }
    }

    /// The account data is inside the cache window — `ensure_account`
    /// leaves it alone (a fresh failure counts too: no hammering).
    fn account_fresh(&self) -> bool {
        self.account_at
            .is_some_and(|at| at.elapsed() < ACCOUNT_TTL)
    }

    /// Drop the cache timestamp: the next `ensure_account` refetches.
    /// A finished chat burns quota, so its provider calls this.
    fn invalidate_account(&mut self) {
        self.account_at = None;
    }

    /// The loaded model catalog, empty while it is not ready.
    pub(crate) fn models(&self) -> &[ModelInfo] {
        match &self.models {
            Load::Ready(models) => models,
            _ => &[],
        }
    }
}

/// A model reply: its visible text and the reasoning it streamed first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reply {
    pub(crate) thinking: String,
    pub(crate) text: String,
    /// How long the model reasoned: first reasoning delta to the first
    /// answer delta (rho's «Thought for»). `None` while it still thinks.
    pub(crate) thought: Option<Duration>,
    /// Output tokens generated so far, from mid-stream usage ticks —
    /// the only live signal while a provider thinks without deltas (Devin).
    pub(crate) tokens: u64,
    /// Tokens of the current no-content stretch once it outlives
    /// [`SILENT_TICKS`]: the «Думает…» counter for a later reasoning
    /// block; 0 while content flows or the stretch is still short.
    pub(crate) busy_tokens: u64,
}

/// Generation-rate gates, as rho's `ModelPerformanceAggregate`: shorter
/// calls say more about latency than about decode speed.
const MIN_GENERATION_OUTPUT_TOKENS: u64 = 32;
const MIN_GENERATION_TIME: Duration = Duration::from_millis(500);
/// Usage ticks since the last content frame that mark a new silent
/// stretch (~180 ms at Devin's ~12 ms cadence): long enough not to
/// flicker between text deltas, short enough to catch the next
/// reasoning block.
const SILENT_TICKS: u32 = 15;

/// Token-weighted average generation throughput of one model.
#[derive(Clone, Copy, Debug, Default)]
struct Throughput {
    tokens: u64,
    time: Duration,
}

/// One finished chat reply.
pub(crate) type ChatResult = Result<Reply, String>;

enum HubEvent {
    Login(ProviderKind, Result<(), String>),
    Account(ProviderKind, Result<AccountInfo, String>),
    Models(ProviderKind, Result<Vec<ModelInfo>, String>),
    /// `generation` tells a cancelled request's late events apart.
    /// `at` is when the stream produced it, not when the loop drained it.
    ChatDelta {
        generation: u64,
        text: String,
        at: Instant,
    },
    ChatThinking {
        generation: u64,
        text: String,
        at: Instant,
    },
    /// A mid-stream usage report (Devin ticks while it reasons).
    ChatUsage {
        generation: u64,
        tokens: u64,
        at: Instant,
    },
    ChatDone {
        generation: u64,
        /// The reply and its provider-reported output tokens (0: unknown).
        result: Result<(Reply, u64), String>,
    },
}

/// The reply being streamed.
struct ChatTask {
    generation: u64,
    reply: Reply,
    handle: JoinHandle<()>,
    /// Whose account the request burns: refreshed when the reply lands.
    kind: ProviderKind,
    /// `provider:model`, the throughput key.
    model_key: String,
    thinking_at: Option<Instant>,
    /// First and last generated delta: the generation window.
    first_at: Option<Instant>,
    last_at: Option<Instant>,
    /// Consecutive usage ticks without a content delta: the measure of
    /// the current silent stretch.
    ticks: u32,
    /// `reply.tokens` when the last content landed — the stretch base.
    stretch_base: u64,
}

impl ChatTask {
    fn observe(&mut self, at: Instant) {
        self.first_at.get_or_insert(at);
        self.last_at = Some(at);
    }

    /// Close the reasoning stretch at `at` if it is open.
    fn finish_thinking(&mut self, at: Instant) {
        if self.reply.thought.is_none()
            && let Some(started) = self.thinking_at
        {
            self.reply.thought = Some(at.saturating_duration_since(started));
        }
    }

    /// A content delta landed: the silent stretch, if any, is over.
    fn end_stretch(&mut self) {
        self.ticks = 0;
        self.stretch_base = self.reply.tokens;
        self.reply.busy_tokens = 0;
    }
}

pub(crate) struct Hub {
    runtime: Runtime,
    clients: Vec<Arc<dyn Provider>>,
    pub(crate) providers: Vec<ProviderEntry>,
    tx: Sender<HubEvent>,
    rx: Receiver<HubEvent>,
    inbox: VecDeque<HubEvent>,
    chat: Option<ChatTask>,
    generation: u64,
    finished: Vec<ChatResult>,
    throughput: HashMap<String, Throughput>,
}

impl Hub {
    pub(crate) fn new() -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let clients: Vec<Arc<dyn Provider>> = ProviderKind::ALL
            .iter()
            .map(|&kind| Arc::from(dd_providers::provider(kind)))
            .collect();
        let providers = clients
            .iter()
            .map(|client| ProviderEntry::new(client.kind(), client.is_authenticated()))
            .collect();
        let (tx, rx) = channel();
        let mut hub = Self {
            runtime,
            clients,
            providers,
            tx,
            rx,
            inbox: VecDeque::new(),
            chat: None,
            generation: 0,
            finished: Vec::new(),
            throughput: HashMap::new(),
        };
        // `/models` needs the catalogs: fetch them for every signed-in
        // provider right away.
        for kind in ProviderKind::ALL {
            hub.ensure_models(kind);
        }
        Ok(hub)
    }

    fn index(kind: ProviderKind) -> usize {
        ProviderKind::ALL
            .iter()
            .position(|&k| k == kind)
            .expect("known provider")
    }

    pub(crate) fn entry(&self, kind: ProviderKind) -> &ProviderEntry {
        &self.providers[Self::index(kind)]
    }

    fn entry_mut(&mut self, kind: ProviderKind) -> &mut ProviderEntry {
        &mut self.providers[Self::index(kind)]
    }

    fn client(&self, kind: ProviderKind) -> Arc<dyn Provider> {
        Arc::clone(&self.clients[Self::index(kind)])
    }

    /// Whether background results wait to be applied; peeks the channel.
    pub(crate) fn has_events(&mut self) -> bool {
        while let Ok(event) = self.rx.try_recv() {
            self.inbox.push_back(event);
        }
        !self.inbox.is_empty()
    }

    /// Something is running that the screen animates: a reply or a login.
    pub(crate) fn busy(&self) -> bool {
        self.chat.is_some()
            || self.providers.iter().any(|entry| {
                entry.login.is_some()
                    || entry.account_fetching
                    || matches!(entry.models, Load::Loading)
            })
    }

    /// Apply every background result that arrived; true when any did.
    pub(crate) fn poll(&mut self) -> bool {
        let changed = self.has_events();
        while let Some(event) = self.inbox.pop_front() {
            match event {
                HubEvent::Login(kind, result) => {
                    let entry = self.entry_mut(kind);
                    entry.login = None;
                    match result {
                        Ok(()) => {
                            entry.authenticated = true;
                            entry.login_error = None;
                            entry.account = Load::Idle;
                            entry.account_at = None;
                            entry.account_fetching = false;
                            entry.models = Load::Idle;
                            self.ensure_account(kind);
                            self.ensure_models(kind);
                        }
                        Err(error) => entry.login_error = Some(error),
                    }
                }
                HubEvent::Account(kind, result) => {
                    let entry = self.entry_mut(kind);
                    entry.account = match result {
                        Ok(info) => Load::Ready(info),
                        Err(error) => Load::Failed(error),
                    };
                    entry.account_at = Some(Instant::now());
                    entry.account_fetching = false;
                }
                HubEvent::Models(kind, result) => {
                    self.entry_mut(kind).models = match result {
                        Ok(mut models) => {
                            models.sort_by_key(|m| m.display_name.to_lowercase());
                            Load::Ready(models)
                        }
                        Err(error) => Load::Failed(error),
                    };
                }
                HubEvent::ChatDelta {
                    generation,
                    text,
                    at,
                } => {
                    if let Some(chat) = self.chat.as_mut().filter(|c| c.generation == generation) {
                        chat.observe(at);
                        chat.finish_thinking(at);
                        chat.end_stretch();
                        chat.reply.text.push_str(&text);
                    }
                }
                HubEvent::ChatThinking {
                    generation,
                    text,
                    at,
                } => {
                    if let Some(chat) = self.chat.as_mut().filter(|c| c.generation == generation) {
                        chat.observe(at);
                        chat.thinking_at.get_or_insert(at);
                        chat.end_stretch();
                        chat.reply.thinking.push_str(&text);
                    }
                }
                HubEvent::ChatUsage {
                    generation,
                    tokens,
                    at,
                } => {
                    if let Some(chat) = self.chat.as_mut().filter(|c| c.generation == generation) {
                        chat.observe(at);
                        chat.ticks += 1;
                        // Ticks before the first text are the reasoning
                        // stretch: they time it even when no reasoning
                        // delta ever comes (Devin's summary arrives late).
                        if chat.reply.text.is_empty() {
                            chat.thinking_at.get_or_insert(at);
                        }
                        chat.reply.tokens = chat.reply.tokens.max(tokens);
                        let stretch = chat.reply.tokens.saturating_sub(chat.stretch_base);
                        chat.reply.busy_tokens = if chat.ticks >= SILENT_TICKS {
                            stretch
                        } else {
                            0
                        };
                    }
                }
                HubEvent::ChatDone { generation, result } => {
                    if let Some(mut chat) = self.chat.take_if(|c| c.generation == generation) {
                        if let Some(last) = chat.last_at {
                            chat.finish_thinking(last);
                        }
                        let result = result.map(|(reply, output_tokens)| {
                            // Prefer the assembled reply; fall back to what
                            // streamed in when the provider returned no text.
                            let streamed = std::mem::take(&mut chat.reply);
                            let reply = Reply {
                                thinking: if reply.thinking.is_empty() {
                                    streamed.thinking
                                } else {
                                    reply.thinking
                                },
                                text: if reply.text.is_empty() {
                                    streamed.text
                                } else {
                                    reply.text
                                },
                                thought: streamed.thought,
                                tokens: streamed.tokens.max(output_tokens),
                                busy_tokens: 0,
                            };
                            self.record_throughput(&chat, &reply, output_tokens);
                            reply
                        });
                        self.finished.push(result);
                        // The reply burned quota: refresh this provider's
                        // account in the background — the cached data stays
                        // on screen until the new one lands.
                        self.entry_mut(chat.kind).invalidate_account();
                        self.ensure_account(chat.kind);
                    }
                }
            }
        }
        changed
    }

    /// Fold a finished call into its model's average, rho-style: output
    /// tokens over the window from the first generated delta to the last.
    /// Without a provider count the text is estimated at 4 chars a token.
    fn record_throughput(&mut self, chat: &ChatTask, reply: &Reply, output_tokens: u64) {
        let (Some(first), Some(last)) = (chat.first_at, chat.last_at) else {
            return;
        };
        let time = last.saturating_duration_since(first);
        let tokens = if output_tokens > 0 {
            output_tokens
        } else {
            ((reply.thinking.chars().count() + reply.text.chars().count()) / 4) as u64
        };
        if tokens < MIN_GENERATION_OUTPUT_TOKENS || time < MIN_GENERATION_TIME {
            return;
        }
        let entry = self.throughput.entry(chat.model_key.clone()).or_default();
        entry.tokens += tokens;
        entry.time += time;
    }

    /// Average generation rate of `model`, rounded, for the `N tok/s`
    /// status field; `None` until one call passed the gates.
    pub(crate) fn generation_rate(&self, kind: ProviderKind, model: &str) -> Option<u64> {
        let entry = self.throughput.get(&format!("{}:{model}", kind.as_str()))?;
        (!entry.time.is_zero())
            .then(|| (entry.tokens as f64 / entry.time.as_secs_f64()).round() as u64)
    }

    /// Replies that finished since the last call.
    pub(crate) fn take_finished(&mut self) -> Vec<ChatResult> {
        std::mem::take(&mut self.finished)
    }

    // --- login ------------------------------------------------------------

    /// Open the provider's OAuth page and wait for the redirect.
    pub(crate) fn login(&mut self, kind: ProviderKind) {
        let client = self.client(kind);
        let pending = match client.begin_login() {
            Ok(pending) => pending,
            Err(error) => {
                self.entry_mut(kind).login_error = Some(error.to_string());
                return;
            }
        };
        let url = pending.authorize_url.clone();
        let _ = dd_providers::oauth::open_browser(&url);
        let tx = self.tx.clone();
        let task = self.runtime.spawn(async move {
            let result = client.finish_login(pending, None).await;
            let _ = tx.send(HubEvent::Login(kind, result.map_err(|e| e.to_string())));
        });
        let entry = self.entry_mut(kind);
        entry.login_error = None;
        entry.login = Some(LoginWait {
            url,
            task: Some(task),
        });
    }

    /// Forget the provider's saved sign-in.
    pub(crate) fn logout(&mut self, kind: ProviderKind) -> Result<(), String> {
        self.cancel_login(kind);
        self.client(kind).logout().map_err(|e| e.to_string())?;
        let entry = self.entry_mut(kind);
        entry.authenticated = false;
        entry.login_error = None;
        entry.account = Load::Idle;
        entry.account_at = None;
        entry.account_fetching = false;
        entry.models = Load::Idle;
        Ok(())
    }

    pub(crate) fn cancel_login(&mut self, kind: ProviderKind) {
        if let Some(mut wait) = self.entry_mut(kind).login.take()
            && let Some(task) = wait.task.take()
        {
            task.abort();
        }
    }

    // --- account / models ----------------------------------------------------

    /// Fetch account info — the one place every view goes through.
    /// Skips the fetch while a request is in flight or the data is fresh
    /// ([`ACCOUNT_TTL`]); a stale `Ready`/`Failed` stays on screen until
    /// the new result lands — the old data is shown, not «загрузка…».
    pub(crate) fn ensure_account(&mut self, kind: ProviderKind) {
        let entry = self.entry(kind);
        if !entry.authenticated || entry.account_fetching || entry.account_fresh() {
            return;
        }
        self.entry_mut(kind).account_fetching = true;
        let (client, tx) = (self.client(kind), self.tx.clone());
        self.runtime.spawn(async move {
            let result = client.account_info().await.map_err(|e| e.to_string());
            let _ = tx.send(HubEvent::Account(kind, result));
        });
    }

    /// Fetch the model catalog unless it is loaded or loading.
    pub(crate) fn ensure_models(&mut self, kind: ProviderKind) {
        let entry = self.entry(kind);
        if !entry.authenticated || !matches!(entry.models, Load::Idle) {
            return;
        }
        self.entry_mut(kind).models = Load::Loading;
        let (client, tx) = (self.client(kind), self.tx.clone());
        self.runtime.spawn(async move {
            let result = client.list_models().await.map_err(|e| e.to_string());
            let _ = tx.send(HubEvent::Models(kind, result));
        });
    }

    // --- chat --------------------------------------------------------------

    /// The reply streaming in, if any.
    pub(crate) fn streaming(&self) -> Option<&Reply> {
        self.chat.as_ref().map(|chat| &chat.reply)
    }

    /// Send `messages` to `model` and stream the reply.
    pub(crate) fn send_chat(
        &mut self,
        kind: ProviderKind,
        model: String,
        effort: Option<Effort>,
        system: &str,
        messages: Vec<Message>,
    ) {
        self.cancel_chat();
        self.generation += 1;
        let generation = self.generation;
        let (client, tx) = (self.client(kind), self.tx.clone());
        let model_key = format!("{}:{model}", kind.as_str());
        let system = system.trim();
        let request = MessagesRequest {
            model,
            messages,
            effort,
            stream: true,
            system: (!system.is_empty()).then(|| system.to_string()),
            ..Default::default()
        };
        let handle = self.runtime.spawn(async move {
            let (sink, mut events) = tokio::sync::mpsc::unbounded_channel();
            let deltas = tx.clone();
            let forward = tokio::spawn(async move {
                while let Some(event) = events.recv().await {
                    // Stamped here, where the stream produced it: the loop
                    // drains the channel up to a frame later.
                    let at = Instant::now();
                    let event = match event {
                        StreamEvent::TextDelta { text } if !text.is_empty() => {
                            HubEvent::ChatDelta {
                                generation,
                                text,
                                at,
                            }
                        }
                        StreamEvent::ThinkingDelta { thinking } if !thinking.is_empty() => {
                            HubEvent::ChatThinking {
                                generation,
                                text: thinking,
                                at,
                            }
                        }
                        StreamEvent::MessageDelta {
                            stop_reason: None,
                            usage: Some(usage),
                        } => HubEvent::ChatUsage {
                            generation,
                            tokens: usage.output_tokens,
                            at,
                        },
                        _ => continue,
                    };
                    let _ = deltas.send(event);
                }
            });
            let result = client.stream_messages(&request, sink).await;
            let _ = forward.await;
            let result = result
                .map(|reply| {
                    let thinking = reply
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Thinking { thinking } => Some(thinking.as_str()),
                            _ => None,
                        })
                        .collect();
                    let text = Reply {
                        thinking,
                        text: reply.text(),
                        tokens: reply.usage.output_tokens,
                        ..Reply::default()
                    };
                    (text, reply.usage.output_tokens)
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(HubEvent::ChatDone { generation, result });
        });
        self.chat = Some(ChatTask {
            generation,
            reply: Reply::default(),
            handle,
            kind,
            model_key,
            thinking_at: None,
            first_at: None,
            last_at: None,
            ticks: 0,
            stretch_base: 0,
        });
    }

    /// Stop the reply in flight; returns what had streamed in.
    pub(crate) fn cancel_chat(&mut self) -> Option<Reply> {
        let mut chat = self.chat.take()?;
        chat.handle.abort();
        chat.finish_thinking(Instant::now());
        // A cancelled request still burned quota: refresh the account too.
        self.entry_mut(chat.kind).invalidate_account();
        self.ensure_account(chat.kind);
        Some(chat.reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_cache_is_fresh_for_a_minute() {
        let mut entry = ProviderEntry::new(ProviderKind::Devin, true);
        // Never fetched: stale — `ensure_account` would fire.
        assert!(!entry.account_fresh());
        entry.account_at = Some(Instant::now());
        assert!(entry.account_fresh());
        // A second past the window: stale again, the next ensure refetches.
        entry.account_at = Some(Instant::now() - ACCOUNT_TTL - Duration::from_secs(1));
        assert!(!entry.account_fresh());
    }

    #[test]
    fn a_finished_chat_invalidates_its_providers_cache() {
        let mut entry = ProviderEntry::new(ProviderKind::Claude, true);
        entry.account_at = Some(Instant::now());
        entry.invalidate_account();
        assert!(!entry.account_fresh());
    }
}
