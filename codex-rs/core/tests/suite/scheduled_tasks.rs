#![allow(clippy::unwrap_used)]

use codex_state::ScheduledPromptKind;
use codex_state::ScheduledPromptStatus;
use codex_state::StateRuntime;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn natural_language_prompt_can_create_scheduled_task_via_schedule_tool() -> anyhow::Result<()>
{
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(
            "call-schedule-1",
            "schedule",
            r#"{"action":"create_task","run_at":"tomorrow 18:00","prompt":"At 18:00 local time tomorrow, inspect the target log, summarize any errors, and email the user if an issue is detected."}"#,
        ),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_response_created("resp-2"),
        ev_assistant_message(
            "msg-1",
            "Understood. I scheduled a task for tomorrow at 18:00 local time and will inspect the log then.",
        ),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    test.submit_turn(
        "Please check the deploy log tomorrow at 6pm, and if there is a problem send me an email.",
    )
    .await?;

    let state_db = StateRuntime::init(
        test.config.codex_home.clone(),
        test.config.model_provider_id.clone(),
        None,
    )
    .await?;
    let schedules = state_db
        .list_scheduled_prompts(Some(&test.session_configured.session_id))
        .await?;
    assert_eq!(schedules.len(), 1);
    let schedule = &schedules[0];
    assert_eq!(schedule.kind, ScheduledPromptKind::Task);
    assert_eq!(schedule.status, ScheduledPromptStatus::Active);
    assert_eq!(schedule.interval_seconds, 0);
    assert!(schedule.prompt.contains("inspect the target log"));

    let output = second_mock
        .single_request()
        .function_call_output("call-schedule-1");
    assert_eq!(
        output.get("call_id").and_then(serde_json::Value::as_str),
        Some("call-schedule-1")
    );
    let output_text = second_mock
        .single_request()
        .function_call_output_content_and_success("call-schedule-1")
        .and_then(|(content, _success)| content)
        .unwrap();
    assert!(output_text.contains("Scheduled task"));

    Ok(())
}
