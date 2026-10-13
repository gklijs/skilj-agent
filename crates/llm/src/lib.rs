//! The model, behind rig (spec: surface Model). Ollama by default; the
//! provider is configuration. A conversation is one narrow task: a
//! system prompt chosen by the task, and the task's own messages, so a
//! retry can show the model what went wrong with its last answer.

use rig_core::client::CompletionClient;
use rig_core::completion::Message;
use rig_core::providers::ollama;
use std::time::{Duration, Instant};

pub const DEFAULT_MODEL: &str = "qwen3:14b";
pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
/// Ollama's own default context is a few thousand tokens, too small for
/// a type's code, an example test and the testing guide together.
pub const DEFAULT_CONTEXT_TOKENS: u64 = 16_384;
/// The most an answer may take, reasoning included. A thinking model
/// can otherwise reason without end; qwen3:14b's good answers to a
/// write-test task stay well under this.
pub const DEFAULT_MAX_ANSWER_TOKENS: u64 = 8_192;
pub const DEFAULT_ANSWER_TIMEOUT: Duration = Duration::from_secs(6 * 60);

#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub model: String,
    pub ollama_url: String,
    pub context_tokens: u64,
    pub max_answer_tokens: u64,
    pub answer_timeout: Duration,
    /// When a thinking model (qwen3) reasons before answering: slower,
    /// usually better code.
    pub thinking: Thinking,
    pub temperature: f64,
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig {
            model: DEFAULT_MODEL.into(),
            ollama_url: DEFAULT_OLLAMA_URL.into(),
            context_tokens: DEFAULT_CONTEXT_TOKENS,
            max_answer_tokens: DEFAULT_MAX_ANSWER_TOKENS,
            answer_timeout: DEFAULT_ANSWER_TIMEOUT,
            thinking: Thinking::Fallback,
            temperature: 0.2,
        }
    }
}

/// When the model reasons before it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    Always,
    Never,
    /// The first attempt without: fast, and enough for simple cases.
    /// Every retry with.
    Fallback,
}

impl Thinking {
    /// Whether attempt `attempt` (from 1) reasons first.
    pub fn on_attempt(self, attempt: usize) -> bool {
        match self {
            Thinking::Always => true,
            Thinking::Never => false,
            Thinking::Fallback => attempt > 1,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Thinking::Always => "thinking",
            Thinking::Never => "without thinking",
            Thinking::Fallback => "first without thinking, then with",
        }
    }
}

impl ModelConfig {
    /// The defaults, overridden by `SKILJ_AGENT_MODEL` and `OLLAMA_HOST`.
    pub fn from_env() -> Self {
        let mut config = ModelConfig::default();
        if let Ok(model) = std::env::var("SKILJ_AGENT_MODEL")
            && !model.is_empty()
        {
            config.model = model;
        }
        if let Ok(host) = std::env::var("OLLAMA_HOST")
            && !host.is_empty()
        {
            config.ollama_url = if host.starts_with("http") { host } else { format!("http://{host}") };
        }
        config
    }

    pub fn describe(&self) -> String {
        format!("{} on Ollama at {}", self.model, self.ollama_url)
    }
}

pub struct Conversation {
    client: ollama::Client,
    config: ModelConfig,
    system: String,
    history: Vec<Message>,
}

#[derive(Debug)]
pub struct Reply {
    pub text: String,
    pub took: Duration,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The answer hit the token or time limit before it was complete.
    pub cut_off: bool,
}

impl Conversation {
    pub fn new(config: &ModelConfig, system: &str) -> anyhow::Result<Conversation> {
        let client = ollama::Client::builder()
            .api_key(ollama::OllamaApiKey::default())
            .base_url(&config.ollama_url)
            .build()?;
        Ok(Conversation { client, config: config.clone(), system: system.to_string(), history: Vec::new() })
    }

    /// Sends a message and returns the answer, without any reasoning
    /// the model did first. `think`: whether it reasons first; the
    /// conversation so far is kept either way.
    pub async fn ask(&mut self, message: &str, think: bool) -> anyhow::Result<Reply> {
        use rig_core::completion::Prompt as _;
        use std::future::IntoFuture as _;
        let agent = self
            .client
            .agent(&self.config.model)
            .preamble(&self.system)
            .temperature(self.config.temperature)
            .additional_params(serde_json::json!({
                "think": think,
                "num_ctx": self.config.context_tokens,
                "num_predict": self.config.max_answer_tokens,
            }))
            .build();
        let started = Instant::now();
        let request = agent.prompt(message).history(self.history.clone()).extended_details();
        let Ok(response) = tokio::time::timeout(self.config.answer_timeout, request.into_future()).await else {
            return Ok(Reply { text: String::new(), took: started.elapsed(), input_tokens: 0, output_tokens: 0, cut_off: true });
        };
        let response = response.map_err(|e| anyhow::anyhow!("the model didn't answer: {e}"))?;
        if let Some(messages) = response.messages {
            self.history.extend(messages);
        }
        let text = strip_thinking(&response.output);
        // Out of tokens: the answer is empty (all reasoning) or has an
        // unclosed code block.
        let cut_off = response.usage.output_tokens >= self.config.max_answer_tokens
            || text.is_empty()
            || text.matches("```").count() % 2 == 1;
        Ok(Reply {
            text,
            took: started.elapsed(),
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            cut_off,
        })
    }
}

/// Some models put their reasoning in the answer itself.
fn strip_thinking(text: &str) -> String {
    match (text.find("<think>"), text.find("</think>")) {
        (Some(start), Some(end)) if start < end => {
            format!("{}{}", &text[..start], &text[end + "</think>".len()..]).trim().to_string()
        }
        _ => text.trim().to_string(),
    }
}

/// The last fenced code block in `text` (preferring `rust` ones), or
/// the whole text when there is none.
pub fn code_block(text: &str) -> String {
    let mut blocks: Vec<(bool, String)> = Vec::new();
    let mut current: Option<(bool, Vec<&str>)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(lang) = trimmed.strip_prefix("```") {
            match current.take() {
                Some((rust, lines)) => blocks.push((rust, lines.join("\n"))),
                None => current = Some((lang.trim().is_empty() || lang.trim() == "rust", Vec::new())),
            }
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    blocks
        .iter()
        .rev()
        .find(|(rust, _)| *rust)
        .or(blocks.last())
        .map(|(_, code)| code.clone())
        .unwrap_or_else(|| text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_last_rust_block() {
        let text = "Here:\n```toml\na = 1\n```\n```rust\n#[test]\nfn a() {}\n```\nDone.";
        assert_eq!(code_block(text), "#[test]\nfn a() {}");
        assert_eq!(code_block("fn a() {}"), "fn a() {}");
    }

    #[test]
    fn fallback_thinks_from_the_second_attempt() {
        let on = |t: Thinking| (1..=4).map(|a| t.on_attempt(a)).collect::<Vec<_>>();
        assert_eq!(on(Thinking::Fallback), [false, true, true, true]);
        assert_eq!(on(Thinking::Always), [true; 4]);
        assert_eq!(on(Thinking::Never), [false; 4]);
    }

    #[test]
    fn strips_inline_thinking() {
        assert_eq!(strip_thinking("<think>hmm</think>\n```rust\n```"), "```rust\n```");
    }
}
