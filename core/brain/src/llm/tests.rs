//! Tests ported from `brain/tests/test_llm_client.py` (the dispatch layer).

use super::*;

#[test]
fn parse_model_recognizes_provider_prefixes() {
    assert_eq!(parse_model("claude-cli:opus"), ("claude-cli", "opus"));
    assert_eq!(parse_model("local:qwen2.5:1.5b"), ("local", "qwen2.5:1.5b"));
    assert_eq!(
        parse_model("litellm:claude-opus-4-8"),
        ("litellm", "claude-opus-4-8")
    );
    // Bare cloud names route through litellm for the reserved API path.
    assert_eq!(
        parse_model("claude-opus-4-8"),
        ("litellm", "claude-opus-4-8")
    );
}
