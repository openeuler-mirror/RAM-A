use std::fs;
use std::path::PathBuf;

use memory_mcp::{
    AuthConfig, CaseLibraryConfig, CaseSearchRequest, CaseServiceConfig, IngestMessage,
    IngestRequest, Principal, SearchRequest, ServerConfig, TokenAuthenticator, TokenConfig,
};
use schemars::schema_for;
use serde_json::json;

fn valid_message(id: &str) -> IngestMessage {
    IngestMessage {
        id: id.to_owned(),
        role: "user".to_owned(),
        speaker: Some("Alice".to_owned()),
        text: "I like tea.".to_owned(),
        timestamp: Some("2026-07-22T10:30:00+08:00".to_owned()),
        candidate: true,
    }
}

fn valid_ingest() -> IngestRequest {
    IngestRequest {
        conversation_id: "conversation-1".to_owned(),
        messages: vec![valid_message("message-1")],
    }
}

fn valid_search() -> SearchRequest {
    SearchRequest {
        query: "What does Alice like?".to_owned(),
        top_k: 10,
        memory_types: vec!["preference".to_owned()],
        event_time_from: Some("2026-01-01T00:00:00Z".to_owned()),
        event_time_to: Some("2026-12-31T23:59:59Z".to_owned()),
    }
}

fn valid_case_search() -> CaseSearchRequest {
    CaseSearchRequest {
        query: "Wi-Fi 满格但 DNS 解析失败怎么处理？".to_owned(),
        library: None,
        top_k: 5,
    }
}

#[test]
fn ingest_candidate_defaults_true_and_optional_fields_are_optional() {
    let request: IngestRequest = serde_json::from_value(json!({
        "conversation_id": "conversation-1",
        "messages": [{
            "id": "message-1",
            "role": "user",
            "text": "I like tea."
        }]
    }))
    .unwrap();

    assert!(request.messages[0].candidate);
    assert_eq!(request.messages[0].speaker, None);
    assert_eq!(request.messages[0].timestamp, None);
    assert!(request.validate().is_ok());
}

#[test]
fn tool_inputs_exclude_caller_supplied_identity_fields() {
    let ingest_schema = serde_json::to_string(&schema_for!(IngestRequest)).unwrap();
    let search_schema = serde_json::to_string(&schema_for!(SearchRequest)).unwrap();
    let case_search_schema = serde_json::to_string(&schema_for!(CaseSearchRequest)).unwrap();

    for schema in [&ingest_schema, &search_schema, &case_search_schema] {
        assert!(!schema.contains("tenant_id"));
        assert!(!schema.contains("user_id"));
        assert!(!schema.contains("scope_id"));
        assert!(!schema.contains("dataset_id"));
    }
    assert!(ingest_schema.contains("speaker"));

    assert!(serde_json::from_value::<IngestRequest>(json!({
        "conversation_id": "conversation-1",
        "tenant_id": "attacker-tenant",
        "messages": [{
            "id": "message-1",
            "role": "user",
            "text": "hello"
        }]
    }))
    .is_err());
    assert!(serde_json::from_value::<SearchRequest>(json!({
        "query": "hello",
        "user_id": "attacker-user"
    }))
    .is_err());
    assert!(serde_json::from_value::<CaseSearchRequest>(json!({
        "query": "DNS failure",
        "dataset_id": "attacker-controlled-dataset"
    }))
    .is_err());
}

#[test]
fn ingest_rejects_empty_or_noncanonical_ids() {
    for conversation_id in ["", " conversation-1", "conversation-1 "] {
        let mut request = valid_ingest();
        request.conversation_id = conversation_id.to_owned();
        assert!(request.validate().is_err());
    }
    for message_id in ["", " message-1", "message-1 "] {
        let mut request = valid_ingest();
        request.messages[0].id = message_id.to_owned();
        assert!(request.validate().is_err());
    }
}

#[test]
fn ingest_rejects_an_empty_message_list_or_duplicate_ids() {
    let mut empty = valid_ingest();
    empty.messages.clear();
    let duplicate = IngestRequest {
        conversation_id: "conversation-1".to_owned(),
        messages: vec![valid_message("duplicate"), valid_message("duplicate")],
    };

    assert!(empty.validate().is_err());
    assert!(duplicate.validate().is_err());
}

#[test]
fn ingest_rejects_too_many_messages() {
    let request = IngestRequest {
        conversation_id: "conversation-1".to_owned(),
        messages: (0..101)
            .map(|index| valid_message(&format!("message-{index}")))
            .collect(),
    };

    assert!(request.validate().is_err());
}

#[test]
fn ingest_validates_text_by_unicode_character_count() {
    let mut exact_limit = valid_ingest();
    exact_limit.messages[0].text = "界".repeat(32_000);
    let mut over_limit = valid_ingest();
    over_limit.messages[0].text = "界".repeat(32_001);
    let mut blank = valid_ingest();
    blank.messages[0].text = " \n\t".to_owned();

    assert!(exact_limit.validate().is_ok());
    assert!(over_limit.validate().is_err());
    assert!(blank.validate().is_err());
}

#[test]
fn ingest_accepts_only_supported_roles() {
    for role in ["user", "assistant", "system", "tool"] {
        let mut request = valid_ingest();
        request.messages[0].role = role.to_owned();
        assert!(request.validate().is_ok());
    }

    let mut request = valid_ingest();
    request.messages[0].role = "other".to_owned();
    assert!(request.validate().is_err());
}

#[test]
fn ingest_validates_optional_timestamp_as_rfc3339() {
    let mut request = valid_ingest();
    request.messages[0].timestamp = Some("July 22, 2026".to_owned());

    assert!(request.validate().is_err());
}

#[test]
fn search_defaults_top_k_and_memory_types() {
    let request: SearchRequest = serde_json::from_value(json!({"query": "tea"})).unwrap();

    assert_eq!(request.top_k, 10);
    assert!(request.memory_types.is_empty());
    assert!(request.validate().is_ok());
}

#[test]
fn search_validates_query_by_unicode_character_count() {
    let mut exact_limit = valid_search();
    exact_limit.query = "界".repeat(32_000);
    let mut over_limit = valid_search();
    over_limit.query = "界".repeat(32_001);
    let mut blank = valid_search();
    blank.query = " \n\t".to_owned();

    assert!(exact_limit.validate().is_ok());
    assert!(over_limit.validate().is_err());
    assert!(blank.validate().is_err());
}

#[test]
fn search_requires_top_k_between_one_and_one_hundred() {
    for top_k in [0, 101] {
        let mut request = valid_search();
        request.top_k = top_k;
        assert!(request.validate().is_err());
    }
    for top_k in [1, 100] {
        let mut request = valid_search();
        request.top_k = top_k;
        assert!(request.validate().is_ok());
    }
}

#[test]
fn search_accepts_only_pipeline_memory_types() {
    let mut request = valid_search();
    request.memory_types = [
        "fact",
        "preference",
        "relationship",
        "event",
        "state",
        "procedure",
        "other",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert!(request.validate().is_ok());

    request.memory_types.push("planned".to_owned());
    assert!(request.validate().is_err());
}

#[test]
fn search_validates_optional_event_times_as_rfc3339() {
    let mut invalid_from = valid_search();
    invalid_from.event_time_from = Some("2026-01-01".to_owned());
    let mut invalid_to = valid_search();
    invalid_to.event_time_to = Some("tomorrow".to_owned());

    assert!(invalid_from.validate().is_err());
    assert!(invalid_to.validate().is_err());
}

#[test]
fn case_search_defaults_to_server_selected_library_and_bounded_top_k() {
    let request: CaseSearchRequest =
        serde_json::from_value(json!({"query": "DNS failure"})).unwrap();

    assert_eq!(request.library, None);
    assert_eq!(request.top_k, 5);
    assert!(request.validate().is_ok());
}

#[test]
fn case_search_rejects_blank_or_oversized_queries_and_unbounded_results() {
    let mut blank = valid_case_search();
    blank.query = " \n\t".to_owned();
    let mut oversized = valid_case_search();
    oversized.query = "界".repeat(32_001);
    let mut zero = valid_case_search();
    zero.top_k = 0;
    let mut too_many = valid_case_search();
    too_many.top_k = 21;

    assert!(blank.validate().is_err());
    assert!(oversized.validate().is_err());
    assert!(zero.validate().is_err());
    assert!(too_many.validate().is_err());
}

#[test]
fn case_service_config_requires_unique_names_private_dataset_mapping_and_default() {
    let valid = CaseServiceConfig {
        base_url: "http://127.0.0.1:18082".to_owned(),
        bearer_token_env: "RAM_A_CASE_SERVICE_TOKEN".to_owned(),
        timeout_seconds: 5,
        max_response_bytes: 262_144,
        default_library: "ops".to_owned(),
        libraries: vec![CaseLibraryConfig {
            name: "ops".to_owned(),
            dataset_id: "openeuler-ops-cases".to_owned(),
            tenant_ids: vec!["tenant-a".to_owned()],
        }],
    };
    assert!(valid.validate().is_ok());

    let mut duplicate = valid.clone();
    duplicate.libraries.push(duplicate.libraries[0].clone());
    assert!(duplicate.validate().is_err());

    let mut missing_default = valid.clone();
    missing_default.default_library = "unknown".to_owned();
    assert!(missing_default.validate().is_err());

    let mut public_bind_without_http = valid;
    public_bind_without_http.base_url = "file:///var/lib/ram-a/cases".to_owned();
    assert!(public_bind_without_http.validate().is_err());
}

fn token_config(token_env: &str, tenant_id: &str, user_id: &str, agent_id: &str) -> TokenConfig {
    TokenConfig {
        token_env: token_env.to_owned(),
        tenant_id: tenant_id.to_owned(),
        user_id: user_id.to_owned(),
        agent_id: agent_id.to_owned(),
        permissions: vec!["memory:read".to_owned(), "memory:write".to_owned()],
    }
}

fn config_for(token_env: &str, tenant_id: &str, user_id: &str, agent_id: &str) -> AuthConfig {
    AuthConfig {
        tokens: vec![token_config(token_env, tenant_id, user_id, agent_id)],
    }
}

fn write_config_fixture(label: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "ram-a-memory-mcp-{label}-{}.json",
        std::process::id()
    ));
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn server_config_loads_json_that_contains_only_token_environment_names() {
    std::env::set_var("RAM_A_TEST_TOKEN_CONFIG", "must-not-appear-in-config");
    let path = write_config_fixture(
        "valid-config",
        r#"{
            "auth": {
                "tokens": [{
                    "token_env": "RAM_A_TEST_TOKEN_CONFIG",
                    "tenant_id": "tenant-a",
                    "user_id": "alice",
                    "agent_id": "xiaoo",
                    "permissions": ["memory:read"]
                }]
            }
        }"#,
    );

    let config = ServerConfig::load(&path).unwrap();

    assert_eq!(config.auth.tokens[0].token_env, "RAM_A_TEST_TOKEN_CONFIG");
    assert!(!fs::read_to_string(&path)
        .unwrap()
        .contains("must-not-appear-in-config"));
    fs::remove_file(path).unwrap();
}

#[test]
fn server_config_rejects_unknown_json_fields() {
    let path = write_config_fixture(
        "unknown-field",
        r#"{
            "auth": {
                "tokens": [],
                "raw_token": "must-never-be-accepted"
            }
        }"#,
    );

    let result = ServerConfig::load(&path);

    fs::remove_file(path).unwrap();
    assert!(result.is_err());
}

#[test]
fn tokens_map_to_opaque_user_scope_without_exposing_raw_ids() {
    std::env::set_var("RAM_A_TEST_TOKEN_ALICE", "secret-a");
    let auth = TokenAuthenticator::from_config(&config_for(
        "RAM_A_TEST_TOKEN_ALICE",
        "tenant-a",
        "alice",
        "xiaoo",
    ))
    .unwrap();

    let principal = auth.authenticate("secret-a").unwrap();

    assert_eq!(principal.tenant_id, "tenant-a");
    assert_eq!(principal.user_id, "alice");
    assert_eq!(principal.agent_id, "xiaoo");
    assert_eq!(principal.permissions, vec!["memory:read", "memory:write"]);
    assert!(principal.scope_id().starts_with("scope-"));
    assert!(!principal.scope_id().contains("tenant-a"));
    assert!(!principal.scope_id().contains("alice"));
    assert_eq!(
        principal.scope_id(),
        "scope-4605b6544afec1f1d0a51be6bff6e7be8da3400294a95dcf6387d1e2f466297b"
    );
}

#[test]
fn duplicate_or_empty_token_values_are_rejected() {
    std::env::set_var("RAM_A_TEST_TOKEN_DUPLICATE_A", "same");
    std::env::set_var("RAM_A_TEST_TOKEN_DUPLICATE_B", "same");
    std::env::set_var("RAM_A_TEST_TOKEN_EMPTY", "");

    let duplicate = AuthConfig {
        tokens: vec![
            token_config("RAM_A_TEST_TOKEN_DUPLICATE_A", "tenant-a", "alice", "xiaoo"),
            token_config("RAM_A_TEST_TOKEN_DUPLICATE_B", "tenant-b", "bob", "other"),
        ],
    };
    let empty = config_for("RAM_A_TEST_TOKEN_EMPTY", "tenant-a", "alice", "xiaoo");

    assert!(TokenAuthenticator::from_config(&duplicate).is_err());
    assert!(TokenAuthenticator::from_config(&empty).is_err());
}

#[test]
fn missing_token_environment_variables_are_rejected() {
    let token_env = "RAM_A_TEST_TOKEN_DOES_NOT_EXIST";
    std::env::remove_var(token_env);

    assert!(
        TokenAuthenticator::from_config(&config_for(token_env, "tenant-a", "alice", "xiaoo",))
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_token_errors_never_expose_secret_bytes() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let token_env = "RAM_A_TEST_TOKEN_NON_UTF8";
    let secret = OsString::from_vec(b"visible-secret-prefix-\xff-visible-secret-suffix".to_vec());
    std::env::set_var(token_env, secret);

    let error =
        TokenAuthenticator::from_config(&config_for(token_env, "tenant-a", "alice", "xiaoo"))
            .unwrap_err();
    std::env::remove_var(token_env);
    let formatted = format!("{error:#}\n{error:?}");

    assert!(formatted.contains(token_env));
    assert!(!formatted.contains("visible-secret-prefix"));
    assert!(!formatted.contains("visible-secret-suffix"));
    assert!(formatted.contains("not valid Unicode"));
}

#[test]
fn authentication_rejects_unknown_tokens_and_agent_spoofing() {
    std::env::set_var("RAM_A_TEST_TOKEN_BOUND_AGENT", "secret-bound");
    let auth = TokenAuthenticator::from_config(&config_for(
        "RAM_A_TEST_TOKEN_BOUND_AGENT",
        "tenant-a",
        "alice",
        "xiaoo",
    ))
    .unwrap();

    assert!(auth.authenticate("wrong-secret").is_err());
    assert!(auth
        .authenticate_with_agent("secret-bound", Some("impostor"))
        .is_err());
    assert!(auth
        .authenticate_with_agent("secret-bound", Some("xiaoo"))
        .is_ok());
    assert!(auth.authenticate_with_agent("secret-bound", None).is_ok());
}

#[test]
fn authenticator_debug_output_never_contains_token_values() {
    std::env::set_var("RAM_A_TEST_TOKEN_DEBUG", "super-secret-debug-value");
    let auth = TokenAuthenticator::from_config(&config_for(
        "RAM_A_TEST_TOKEN_DEBUG",
        "tenant-a",
        "alice",
        "xiaoo",
    ))
    .unwrap();

    let debug = format!("{auth:?}");

    assert!(!debug.contains("super-secret-debug-value"));
}

#[test]
fn scope_tuple_is_canonical_and_unambiguous() {
    let left = Principal {
        tenant_id: "tenant:a".to_owned(),
        user_id: "alice".to_owned(),
        agent_id: "xiaoo".to_owned(),
        permissions: vec![],
    };
    let right = Principal {
        tenant_id: "tenant".to_owned(),
        user_id: "a:alice".to_owned(),
        agent_id: "xiaoo".to_owned(),
        permissions: vec![],
    };
    let same_scope_different_agent = Principal {
        tenant_id: left.tenant_id.clone(),
        user_id: left.user_id.clone(),
        agent_id: "another-agent".to_owned(),
        permissions: vec!["different:permission".to_owned()],
    };

    assert_ne!(left.scope_id(), right.scope_id());
    assert_eq!(left.scope_id(), same_scope_different_agent.scope_id());
}
