use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ScheduleCancelParams;
use codex_app_server_protocol::ScheduleCancelResponse;
use codex_app_server_protocol::ScheduleCreateParams;
use codex_app_server_protocol::ScheduleCreateResponse;
use codex_app_server_protocol::ScheduleListParams;
use codex_app_server_protocol::ScheduleListResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn schedule_create_list_and_cancel_round_trip_for_persisted_thread() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;

    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "seed persisted rollout".to_string(),
                text_elements: Vec::new(),
            }],
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = to_response(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
        )
        .await??,
    )?;
    let _ = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let create_id = mcp
        .send_schedule_create_request(ScheduleCreateParams {
            thread_id: thread.id.clone(),
            prompt: "check deployment logs".to_string(),
            interval_seconds: 600,
        })
        .await?;
    let create_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(create_id)),
    )
    .await??;
    let ScheduleCreateResponse { schedule } = to_response::<ScheduleCreateResponse>(create_resp)?;
    assert_eq!(schedule.thread_id, thread.id);
    assert_eq!(schedule.kind, "loop");
    assert_eq!(schedule.status, "active");
    assert_eq!(schedule.interval_seconds, 600);
    assert_eq!(schedule.paused_until, None);
    assert_eq!(schedule.completed_at, None);

    let list_id = mcp
        .send_schedule_list_request(ScheduleListParams {
            thread_id: Some(thread.id.clone()),
        })
        .await?;
    let list_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(list_id)),
    )
    .await??;
    let ScheduleListResponse { data } = to_response::<ScheduleListResponse>(list_resp)?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0], schedule);

    let cancel_id = mcp
        .send_schedule_cancel_request(ScheduleCancelParams {
            schedule_id: schedule.id.clone(),
        })
        .await?;
    let cancel_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(cancel_id)),
    )
    .await??;
    let ScheduleCancelResponse { cancelled } = to_response::<ScheduleCancelResponse>(cancel_resp)?;
    assert!(cancelled);

    let list_after_cancel_id = mcp
        .send_schedule_list_request(ScheduleListParams {
            thread_id: Some(thread.id.clone()),
        })
        .await?;
    let list_after_cancel_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(list_after_cancel_id)),
    )
    .await??;
    let ScheduleListResponse { data } =
        to_response::<ScheduleListResponse>(list_after_cancel_resp)?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].status, "cancelled");

    Ok(())
}

fn create_config_toml(codex_home: &std::path::Path, server_uri: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"
model = "gpt-5.4"
approval_policy = "never"
sandbox_mode = "danger-full-access"

[model_providers.mock_provider]
name = "Mock provider"
base_url = "{server_uri}"
wire_api = "responses"
http_auth_token = "dummy"
"#
        ),
    )
}
