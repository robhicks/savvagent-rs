//! Cross-vendor `tool_use_id` compatibility matrix.
//!
//! For every `(sender_provider, receiver_provider)` pair across the three
//! shipping vendors (Anthropic, Gemini, OpenAI), build a synthetic SPP
//! history whose `tool_use_id` is prefixed with `sender_provider` (the
//! shape Phase 3 will introduce via host-side namespacing) and submit a
//! `CompleteRequest` carrying it to `receiver_provider`. The test asserts
//! (a) the call returns `Ok` and (b) the outgoing request body satisfies
//! a vendor-specific structural assertion:
//!
//! - Anthropic / OpenAI: foreign id appears in BOTH canonical fields
//!   (round-trip preservation; see `*_body_has_foreign_id` inspectors).
//! - Gemini: API has no id field, so the receiver-side contract under
//!   test is that the translator's `id_to_name` lookup resolved the
//!   foreign id to the matching tool name (`list_dir`).
//!
//! Tests use the axum fake-vendor servers in `support/mod.rs`; live-vendor
//! twins are marked `#[ignore]` so PR CI does not need credentials.

mod support;

use savvagent_mcp::ProviderHandler;
// Tasks 4-6 will consume the rest of these helpers; until they land,
// `#[allow(unused_imports)]` keeps the import block in one place so the
// follow-up commits only have to append `#[tokio::test]` functions.
#[allow(unused_imports)]
use support::{
    FakeState, anthropic_body_has_foreign_id, anthropic_success_response, build_request,
    gemini_body_has_resolved_function_name, gemini_success_response, history_with_foreign_id,
    openai_body_has_foreign_id, openai_success_response, spawn_fake_anthropic, spawn_fake_gemini,
    spawn_fake_openai,
};

// ===========================================================================
// Anthropic receiver pairs
// ===========================================================================

#[tokio::test]
async fn anthropic_to_anthropic_control() {
    let state = FakeState::new(anthropic_success_response());
    let base = spawn_fake_anthropic(&state).await;
    let provider = provider_anthropic::provider_for_tests(base);

    let foreign_id = "anthropic:toolu_abc_123";
    let history = history_with_foreign_id("anthropic");
    let req = build_request("claude-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("anthropic accepts anthropic-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake anthropic received a request");
    assert!(
        anthropic_body_has_foreign_id(&body, foreign_id),
        "anthropic body must carry {foreign_id} in BOTH tool_use.id and tool_result.tool_use_id; body was {body:#?}"
    );
}

#[tokio::test]
async fn gemini_to_anthropic() {
    let state = FakeState::new(anthropic_success_response());
    let base = spawn_fake_anthropic(&state).await;
    let provider = provider_anthropic::provider_for_tests(base);

    let foreign_id = "gemini:toolu_abc_123";
    let history = history_with_foreign_id("gemini");
    let req = build_request("claude-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("anthropic accepts gemini-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake anthropic received a request");
    assert!(
        anthropic_body_has_foreign_id(&body, foreign_id),
        "anthropic body must carry {foreign_id} in BOTH tool_use.id and tool_result.tool_use_id; body was {body:#?}"
    );
}

#[tokio::test]
async fn openai_to_anthropic() {
    let state = FakeState::new(anthropic_success_response());
    let base = spawn_fake_anthropic(&state).await;
    let provider = provider_anthropic::provider_for_tests(base);

    let foreign_id = "openai:toolu_abc_123";
    let history = history_with_foreign_id("openai");
    let req = build_request("claude-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("anthropic accepts openai-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake anthropic received a request");
    assert!(
        anthropic_body_has_foreign_id(&body, foreign_id),
        "anthropic body must carry {foreign_id} in BOTH tool_use.id and tool_result.tool_use_id; body was {body:#?}"
    );
}

// ===========================================================================
// Gemini receiver pairs
// ===========================================================================

#[tokio::test]
async fn anthropic_to_gemini() {
    let state = FakeState::new(gemini_success_response());
    let base = spawn_fake_gemini(&state).await;
    let provider = provider_gemini::provider_for_tests(base);

    let history = history_with_foreign_id("anthropic");
    let req = build_request("gemini-test", history);

    // Gemini's API has no id field; the round-trip contract is that the
    // translator resolves the foreign tool_use_id back to the matching
    // ToolUse.name (`list_dir`) via the per-request id_to_name lookup.
    // A regression dropping that lookup would surface `"unknown_tool"`
    // on the wire instead, and this assertion would fail.
    let resp = provider
        .complete(req, None)
        .await
        .expect("gemini accepts anthropic-prefixed tool_use_id in history");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake gemini received a request");
    assert!(
        gemini_body_has_resolved_function_name(&body, "list_dir"),
        "gemini translator must resolve the foreign tool_use_id back to `list_dir` via id_to_name; body was {body:#?}"
    );
}

#[tokio::test]
async fn gemini_to_gemini_control() {
    let state = FakeState::new(gemini_success_response());
    let base = spawn_fake_gemini(&state).await;
    let provider = provider_gemini::provider_for_tests(base);

    let history = history_with_foreign_id("gemini");
    let req = build_request("gemini-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("gemini accepts gemini-prefixed tool_use_id in history");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake gemini received a request");
    assert!(
        gemini_body_has_resolved_function_name(&body, "list_dir"),
        "gemini translator must resolve the foreign tool_use_id back to `list_dir` via id_to_name; body was {body:#?}"
    );
}

#[tokio::test]
async fn openai_to_gemini() {
    let state = FakeState::new(gemini_success_response());
    let base = spawn_fake_gemini(&state).await;
    let provider = provider_gemini::provider_for_tests(base);

    let history = history_with_foreign_id("openai");
    let req = build_request("gemini-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("gemini accepts openai-prefixed tool_use_id in history");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake gemini received a request");
    assert!(
        gemini_body_has_resolved_function_name(&body, "list_dir"),
        "gemini translator must resolve the foreign tool_use_id back to `list_dir` via id_to_name; body was {body:#?}"
    );
}

// ===========================================================================
// OpenAI receiver pairs
// ===========================================================================

#[tokio::test]
async fn anthropic_to_openai() {
    let state = FakeState::new(openai_success_response());
    let base = spawn_fake_openai(&state).await;
    let provider = provider_openai::provider_for_tests(base);

    let foreign_id = "anthropic:toolu_abc_123";
    let history = history_with_foreign_id("anthropic");
    let req = build_request("gpt-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("openai accepts anthropic-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake openai received a request");
    assert!(
        openai_body_has_foreign_id(&body, foreign_id),
        "openai body must carry {foreign_id} in BOTH assistant.tool_calls[].id and tool-role.tool_call_id; body was {body:#?}"
    );
}

#[tokio::test]
async fn gemini_to_openai() {
    let state = FakeState::new(openai_success_response());
    let base = spawn_fake_openai(&state).await;
    let provider = provider_openai::provider_for_tests(base);

    let foreign_id = "gemini:toolu_abc_123";
    let history = history_with_foreign_id("gemini");
    let req = build_request("gpt-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("openai accepts gemini-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake openai received a request");
    assert!(
        openai_body_has_foreign_id(&body, foreign_id),
        "openai body must carry {foreign_id} in BOTH assistant.tool_calls[].id and tool-role.tool_call_id; body was {body:#?}"
    );
}

#[tokio::test]
async fn openai_to_openai_control() {
    let state = FakeState::new(openai_success_response());
    let base = spawn_fake_openai(&state).await;
    let provider = provider_openai::provider_for_tests(base);

    let foreign_id = "openai:toolu_abc_123";
    let history = history_with_foreign_id("openai");
    let req = build_request("gpt-test", history);

    let resp = provider
        .complete(req, None)
        .await
        .expect("openai accepts openai-prefixed tool_use_id");
    assert!(matches!(
        resp.stop_reason,
        savvagent_protocol::StopReason::EndTurn
    ));

    let body = state
        .captured_body()
        .await
        .expect("fake openai received a request");
    assert!(
        openai_body_has_foreign_id(&body, foreign_id),
        "openai body must carry {foreign_id} in BOTH assistant.tool_calls[].id and tool-role.tool_call_id; body was {body:#?}"
    );
}
