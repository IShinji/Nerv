//! Tests ported from `brain/tests/test_router.py` and
//! `brain/tests/test_eval_routing.py` (the golden eval).

use super::*;

fn router() -> Router {
    Router::with_parts(
        "claude-cli:opus",
        include_str!("../../prompts/router_system.md"),
        include_str!("../../prompts/router_user_template.txt"),
    )
}

fn route_result(
    intent: &str,
    complexity: &str,
    model_tier: i64,
    agent_type: &str,
    reply: &str,
) -> RouteResult {
    RouteResult {
        intent: intent.to_string(),
        complexity: complexity.to_string(),
        model_tier,
        agent_type: agent_type.to_string(),
        needs_capabilities: Vec::new(),
        preferred_execution_mode: "auto".to_string(),
        reply: reply.to_string(),
    }
}

// --- TestRouterFallback ---

#[test]
fn fallback_code_intent_detected() {
    let result = router().fallback_classify("help me write a Python script");
    assert_eq!(result.intent, "code_generation");
    assert_eq!(result.agent_type, "coder");
}

#[test]
fn fallback_research_intent_detected() {
    let result = router().fallback_classify("what is quantum computing");
    assert_eq!(result.intent, "research");
    assert_eq!(result.agent_type, "researcher");
}

#[test]
fn fallback_writing_intent_detected() {
    let result = router().fallback_classify("write me an essay about AI");
    assert_eq!(result.intent, "writing");
    assert_eq!(result.agent_type, "writer");
}

#[test]
fn fallback_sysadmin_intent_detected() {
    let result = router().fallback_classify("deploy this to the server");
    assert_eq!(result.intent, "sysadmin");
    assert_eq!(result.agent_type, "sysadmin");
}

#[test]
fn fallback_general_intent() {
    let result = router().fallback_classify("hello there!");
    assert_eq!(result.intent, "general");
    assert_eq!(result.agent_type, "general");
}

#[test]
fn fallback_empty_message_classified() {
    let result = router().fallback_classify("");
    assert_eq!(result.intent, "general");
}

// --- TestRouterParsing ---

#[test]
fn valid_json_parsed() {
    let content = r#"{"intent": "code_generation", "complexity": "medium", "model_tier": 1, "agent_type": "coder", "reply": ""}"#;
    let result = router().parse_response(content);
    assert_eq!(result.intent, "code_generation");
    assert_eq!(result.model_tier, 1);
}

#[test]
fn json_with_extra_text_extracted() {
    let content = r#"Here is the result: {"intent": "general", "complexity": "low", "model_tier": 0, "agent_type": "general", "reply": "hello"}"#;
    let result = router().parse_response(content);
    assert_eq!(result.intent, "general");
}

#[test]
fn invalid_json_returns_fallback() {
    let content = "This is not JSON at all";
    let result = router().parse_response(content);
    assert_eq!(result.intent, "general");
}

#[test]
fn general_intent_agent_type_is_normalized() {
    let result = route_result("general", "low", 0, "personal AI assistant", "Hello!");
    let sanitized = router().sanitize_route_result("hello", result);
    assert_eq!(sanitized.agent_type, "general");
}

#[test]
fn question_reply_is_suppressed() {
    let result = route_result("general", "low", 0, "search", "现在几点？");
    let sanitized = router().sanitize_route_result("现在几点", result);
    assert_eq!(sanitized.reply, "");
    assert_eq!(sanitized.agent_type, "general");
}

#[test]
fn trivial_greeting_can_keep_direct_reply() {
    let result = route_result("general", "low", 0, "general", "你好，有什么可以帮你？");
    let sanitized = router().sanitize_route_result("你好", result);
    assert_eq!(sanitized.reply, "你好，有什么可以帮你？");
}

// --- TestRouteResult ---

#[test]
fn route_result_default_values() {
    let result: RouteResult = serde_json::from_str(r#"{"intent": "general"}"#).unwrap();
    assert_eq!(result.complexity, "low");
    assert_eq!(result.model_tier, 0);
    assert_eq!(result.agent_type, "general");
    assert!(result.needs_capabilities.is_empty());
    assert_eq!(result.preferred_execution_mode, "auto");
    assert_eq!(result.reply, "");
}

#[test]
fn route_result_serialization() {
    let result = RouteResult {
        intent: "code_generation".to_string(),
        complexity: "high".to_string(),
        model_tier: 2,
        agent_type: "coder".to_string(),
        needs_capabilities: vec!["filesystem.read".to_string(), "local.exec".to_string()],
        preferred_execution_mode: "auto".to_string(),
        reply: "I can help with that".to_string(),
    };
    let data = serde_json::to_value(&result).unwrap();
    assert_eq!(data["intent"], "code_generation");
    assert_eq!(data["model_tier"], 2);
    assert_eq!(
        data["needs_capabilities"],
        serde_json::json!(["filesystem.read", "local.exec"])
    );
    assert_eq!(data["preferred_execution_mode"], "auto");
}

// --- Golden eval (port of eval.py offline mode / test_eval_routing.py) ---

struct RoutingCase {
    message: &'static str,
    raw_model_json: &'static str,
    expect_agent: &'static str,
    expect_reply_suppressed: bool,
}

/// Port of `GOLDEN_ROUTING`.
fn golden_routing() -> Vec<RoutingCase> {
    vec![
        RoutingCase {
            message: "help me debug this python function",
            raw_model_json: r#"{"intent":"code_generation","complexity":"medium","model_tier":1,"agent_type":"developer","reply":""}"#,
            expect_agent: "coder",
            expect_reply_suppressed: false,
        },
        RoutingCase {
            message: "compare postgres and mysql for analytics",
            raw_model_json: r#"{"intent":"research","complexity":"high","model_tier":2,"agent_type":"analyst","reply":""}"#,
            expect_agent: "researcher",
            expect_reply_suppressed: false,
        },
        RoutingCase {
            message: "translate this paragraph to french",
            raw_model_json: r#"{"intent":"writing","complexity":"low","model_tier":1,"agent_type":"translator","reply":""}"#,
            expect_agent: "writer",
            expect_reply_suppressed: false,
        },
        RoutingCase {
            message: "restart nginx on the server",
            raw_model_json: r#"{"intent":"sysadmin","complexity":"medium","model_tier":1,"agent_type":"devops","reply":""}"#,
            expect_agent: "sysadmin",
            expect_reply_suppressed: false,
        },
        RoutingCase {
            message: "你好",
            raw_model_json: r#"{"intent":"general","complexity":"low","model_tier":0,"agent_type":"personal AI assistant","reply":"你好，有什么可以帮你？"}"#,
            expect_agent: "general",
            expect_reply_suppressed: false,
        },
        RoutingCase {
            message: "现在几点",
            raw_model_json: r#"{"intent":"general","complexity":"low","model_tier":0,"agent_type":"assistant","reply":"现在几点？"}"#,
            expect_agent: "general",
            expect_reply_suppressed: true,
        },
    ]
}

#[test]
fn golden_routing_pipeline_is_fully_correct() {
    let r = router();
    let cases = golden_routing();
    assert!(cases.len() >= 5);
    for case in &cases {
        let parsed = r.parse_response(case.raw_model_json);
        let result = r.sanitize_route_result(case.message, parsed);

        let mut problems = Vec::new();
        if result.agent_type != case.expect_agent {
            problems.push(format!(
                "agent {:?}!={:?}",
                result.agent_type, case.expect_agent
            ));
        }
        if case.expect_reply_suppressed && !result.reply.is_empty() {
            problems.push("reply not suppressed".to_string());
        }
        assert!(
            problems.is_empty(),
            "{:?}: {}",
            case.message,
            problems.join(", ")
        );
    }
}

#[test]
fn golden_eval_reports_failures_for_wrong_expectations() {
    // Port of test_eval_report_tracks_failures: a deliberately wrong
    // expectation must be caught, not silently passed.
    let r = router();
    let parsed =
        r.parse_response(r#"{"intent":"code_generation","agent_type":"developer","reply":""}"#);
    let result = r.sanitize_route_result("x", parsed);
    assert_ne!(result.agent_type, "researcher");
}

// --- classify entry points ---

#[tokio::test]
async fn empty_message_gets_the_direct_help_reply() {
    let result = router().classify("").await;
    assert_eq!(result.intent, "general");
    assert_eq!(result.agent_type, "general");
    assert_eq!(
        result.reply,
        "I received an empty message. How can I help you?"
    );
}

#[tokio::test]
async fn whitespace_only_message_gets_the_direct_help_reply() {
    let result = router().classify("   \n\t").await;
    assert_eq!(
        result.reply,
        "I received an empty message. How can I help you?"
    );
}

#[tokio::test]
async fn unsupported_backend_falls_back_to_keywords() {
    // The litellm backend is not implemented in the Rust brain yet; the
    // Router must treat that like any backend failure.
    let r = Router::with_parts(
        "litellm:claude-opus-4-8",
        include_str!("../../prompts/router_system.md"),
        include_str!("../../prompts/router_user_template.txt"),
    );
    let result = r.classify("help me debug this python function").await;
    assert_eq!(result.intent, "code_generation");
    assert_eq!(result.agent_type, "coder");
}

#[test]
fn user_prompt_renders_like_python_str_format() {
    let template = include_str!("../../prompts/router_user_template.txt");
    let rendered = render_user_prompt(template, "你好");
    assert!(rendered.starts_with("Classify this message and return JSON:"));
    assert!(rendered.contains("\n\nMessage: 你好\n\n"));
    assert!(rendered.ends_with(r#"Return format: {"intent": "...", "complexity": "...", "model_tier": N, "agent_type": "...", "needs_capabilities": ["..."], "preferred_execution_mode": "...", "reply": "..."}"#));
    assert!(!rendered.contains("{{"));
}

/// Live spot-check against the configured backend: `cargo test -p
/// nerv-brain -- --ignored`. Port of `python -m nerv.eval`.
#[tokio::test]
#[ignore = "requires the configured router backend"]
async fn live_routing_eval() {
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let r = Router::new(&project_root).expect("router loads from repo layout");
    for case in golden_routing() {
        let result = r.classify(case.message).await;
        assert_eq!(
            result.agent_type, case.expect_agent,
            "message {:?}",
            case.message
        );
    }
}
