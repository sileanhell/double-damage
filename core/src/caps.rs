//! What a model can do, judged from its chat template before it runs and corrected by what
//! llama-server reports (`chat_template_caps`) once it is loaded.

use serde::Serialize;

/// How the model reasons and how the app can steer it.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Reasoning {
    /// No reasoning.
    None,
    /// Thinking can be switched on and off (`enable_thinking`: Qwen3, GLM, …).
    Toggle,
    /// Reasoning levels (`reasoning_effort`: GPT-OSS, …).
    Effort,
    /// Always thinks, nothing to choose (DeepSeek-R1, QwQ, …).
    Always,
}

#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub tools: bool,
    pub reasoning: Reasoning,
    /// For `Reasoning::Effort`: the levels, weakest first.
    pub effort_levels: Vec<String>,
    /// Confirmed by the running server rather than guessed from the template.
    pub confirmed: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self { tools: false, reasoning: Reasoning::None, effort_levels: Vec::new(), confirmed: false }
    }
}

const DEFAULT_EFFORT: [&str; 3] = ["low", "medium", "high"];

/// Guess from the Jinja chat template stored in the GGUF file.
pub fn from_template(template: Option<&str>) -> Capabilities {
    let Some(t) = template else { return Capabilities::default() };
    let tools = t.contains("tools") || t.contains("tool_call");
    let reasoning = if t.contains("reasoning_effort") {
        Reasoning::Effort
    } else if t.contains("enable_thinking") {
        Reasoning::Toggle
    } else if t.contains("<think>") || t.contains("reasoning_content") || t.contains("<|channel|>analysis") {
        Reasoning::Always
    } else {
        Reasoning::None
    };
    let effort_levels = if reasoning == Reasoning::Effort {
        DEFAULT_EFFORT.iter().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    };
    Capabilities { tools, reasoning, effort_levels, confirmed: false }
}

/// Folds in `chat_template_caps` from a running llama-server.
pub fn with_runtime(mut caps: Capabilities, runtime: Option<&serde_json::Value>) -> Capabilities {
    let Some(rt) = runtime else { return caps };
    if let Some(tools) = rt.get("supports_tools").and_then(|v| v.as_bool()) {
        caps.tools = tools;
    }
    let effort = rt.get("supports_reasoning_effort").and_then(|v| v.as_bool());
    if effort == Some(true) {
        caps.reasoning = Reasoning::Effort;
        let levels: Vec<String> = rt
            .get("reasoning_effort_levels")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        caps.effort_levels = if levels.is_empty() { DEFAULT_EFFORT.iter().map(|s| s.to_string()).collect() } else { levels };
    } else if effort == Some(false) && caps.reasoning == Reasoning::Effort {
        // The server says it can't take levels after all (only when it says so: older builds
        // don't report the key); thinking still happens.
        caps.reasoning = Reasoning::Always;
        caps.effort_levels.clear();
    }
    caps.confirmed = true;
    caps
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn template_guesses() {
        assert_eq!(from_template(None), Capabilities::default());
        assert_eq!(from_template(Some("{% if enable_thinking %}<think>{% endif %}")).reasoning, Reasoning::Toggle);
        let effort = from_template(Some("{{ reasoning_effort }} {% for tool in tools %}{% endfor %}"));
        assert_eq!(effort.reasoning, Reasoning::Effort);
        assert!(effort.tools);
        assert_eq!(from_template(Some("<think>\n")).reasoning, Reasoning::Always);
        assert_eq!(from_template(Some("plain")).reasoning, Reasoning::None);
    }

    #[test]
    fn runtime_corrects_the_guess() {
        let guess = from_template(Some("reasoning_effort"));
        // Older builds don't report the key: keep the guess.
        assert_eq!(with_runtime(guess.clone(), Some(&json!({ "supports_tools": true }))).reasoning, Reasoning::Effort);
        let no = with_runtime(guess.clone(), Some(&json!({ "supports_reasoning_effort": false })));
        assert_eq!(no.reasoning, Reasoning::Always);
        assert!(no.confirmed);
        let levels = with_runtime(
            guess,
            Some(&json!({ "supports_reasoning_effort": true, "reasoning_effort_levels": ["low", "high"] })),
        );
        assert_eq!(levels.effort_levels, vec!["low", "high"]);
        let toggle = with_runtime(from_template(Some("enable_thinking")), Some(&json!({ "supports_tools": false })));
        assert_eq!(toggle.reasoning, Reasoning::Toggle);
        assert!(!toggle.tools);
    }
}
