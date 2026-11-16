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

#[tokio::test]
async fn litellm_backend_reports_itself_as_unsupported() {
    // The python backend needs `uv sync --extra litellm`; the rust port has no
    // litellm library at all, so the reserved path fails loudly (callers fall
    // back or degrade gracefully, exactly like any backend failure).
    let opts = CompleteOptions {
        model: "litellm:claude-opus-4-8",
        ..Default::default()
    };
    let err = complete(&[ChatMessage::user("hi")], &opts)
        .await
        .expect_err("litellm is not ported");
    assert!(err.to_string().contains("not implemented"), "{err}");
}
