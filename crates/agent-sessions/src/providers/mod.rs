pub mod claude;
pub mod codex;
pub mod factory;
pub mod gemini;
pub mod goose;
pub mod opencode;
pub mod qwen;

use crate::provider::AgentProvider;

/// Every provider we know how to read, in display order.
pub fn all_providers() -> Vec<Box<dyn AgentProvider>> {
    vec![
        Box::new(claude::ClaudeProvider::new()),
        Box::new(codex::CodexProvider::new()),
        Box::new(opencode::OpencodeProvider::new()),
        Box::new(gemini::GeminiProvider::new()),
        Box::new(qwen::QwenProvider::new()),
        Box::new(goose::GooseProvider::new()),
        Box::new(factory::FactoryProvider::new()),
    ]
}
