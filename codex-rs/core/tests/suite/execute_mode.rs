#![allow(clippy::expect_used, clippy::unwrap_used)]

use anyhow::Result;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_apply_patch_custom_tool_call;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::json;

const TASK_COMPLETE_OPEN_TAG: &str = "<task_complete>";
const TASK_COMPLETE_CLOSE_TAG: &str = "</task_complete>";
const AWAIT_USER_INPUT_OPEN_TAG: &str = "<await_user_input>";
const AUTO_CONTINUE_PREFIX: &str = "Continue executing the current task autonomously.";
const NON_STOP_AUTO_CONTINUE_PREFIX: &str = "Continue operating in Non-stop mode.";
const NON_STOP_SUBTASK_PREFIX: &str = "The last subtask is complete.";
const STALL_RECOVERY_PREFIX: &str = "Your last visible update repeated without concrete progress";

fn execute_mode(model: String) -> CollaborationMode {
    CollaborationMode {
        mode: ModeKind::Execute,
        settings: Settings {
            model,
            reasoning_effort: None,
            developer_instructions: None,
        },
    }
}

fn non_stop_mode(model: String) -> CollaborationMode {
    CollaborationMode {
        mode: ModeKind::NonStop,
        settings: Settings {
            model,
            reasoning_effort: None,
            developer_instructions: None,
        },
    }
}

async fn submit_turn(
    test: &TestCodex,
    prompt: &str,
    collaboration_mode: CollaborationMode,
) -> Result<TurnCompleteEvent> {
    let session_model = test.session_configured.model.clone();

    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: prompt.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model.clone(),
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: Some(collaboration_mode),
            personality: None,
        })
        .await?;

    let turn_id = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnStarted(event) => Some(event.turn_id.clone()),
        _ => None,
    })
    .await;

    let completed = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnComplete(event) if event.turn_id == turn_id => Some(event.clone()),
        _ => None,
    })
    .await;

    Ok(completed)
}

async fn submit_execute_turn(test: &TestCodex, prompt: &str) -> Result<TurnCompleteEvent> {
    let session_model = test.session_configured.model.clone();
    submit_turn(test, prompt, execute_mode(session_model)).await
}

async fn submit_non_stop_turn(test: &TestCodex, prompt: &str) -> Result<TurnCompleteEvent> {
    let session_model = test.session_configured.model.clone();
    submit_turn(test, prompt, non_stop_mode(session_model)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_auto_continues_without_user_input_until_completion() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_assistant_message("msg-1", "Still working; next I will inspect the files."),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message(
                    "msg-2",
                    &format!("{TASK_COMPLETE_OPEN_TAG}done{TASK_COMPLETE_CLOSE_TAG}"),
                ),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let completed = submit_execute_turn(&test, "finish the task autonomously").await?;

    assert_eq!(completed.last_agent_message.as_deref(), Some("done"));

    let all_requests = requests.requests();
    assert_eq!(
        all_requests.len(),
        2,
        "expected one auto-continuation round"
    );
    assert!(
        all_requests[1]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(AUTO_CONTINUE_PREFIX)),
        "second request should include execute-mode continuation developer message"
    );
    assert!(
        !all_requests[0]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(AUTO_CONTINUE_PREFIX)),
        "first request should not already contain execute-mode continuation developer message"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_ignores_plain_text_question_and_keeps_going() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_assistant_message(
                    "msg-1",
                    "I could run one more verification step. Do you want me to continue?",
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message(
                    "msg-2",
                    &format!("{TASK_COMPLETE_OPEN_TAG}verified without waiting for user confirmation{TASK_COMPLETE_CLOSE_TAG}"),
                ),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let completed =
        submit_execute_turn(&test, "keep moving without asking me optional questions").await?;

    assert_eq!(
        completed.last_agent_message.as_deref(),
        Some("verified without waiting for user confirmation")
    );

    let all_requests = requests.requests();
    assert_eq!(all_requests.len(), 2);
    assert!(
        all_requests[1]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(AUTO_CONTINUE_PREFIX)),
        "plain-text questions should not pause execute mode; continuation should be injected"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_stops_immediately_when_task_is_marked_complete() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let requests = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message(
                "msg-1",
                &format!("{TASK_COMPLETE_OPEN_TAG}done{TASK_COMPLETE_CLOSE_TAG}"),
            ),
            ev_completed("resp-1"),
        ])],
    )
    .await;

    let completed = submit_execute_turn(&test, "complete and stop at the right time").await?;

    assert_eq!(completed.last_agent_message.as_deref(), Some("done"));

    let all_requests = requests.requests();
    assert_eq!(all_requests.len(), 1, "should stop without auto-continuing");
    assert!(
        !all_requests[0].body_contains_text(AUTO_CONTINUE_PREFIX),
        "completion-marked response should not trigger execute-mode continuation"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_does_not_stop_on_unmarked_completion_claim() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_assistant_message(
                    "msg-1",
                    "Everything looks done to me; I think the task is complete.",
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message(
                    "msg-2",
                    &format!("{TASK_COMPLETE_OPEN_TAG}done after explicit completion marker{TASK_COMPLETE_CLOSE_TAG}"),
                ),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let completed = submit_execute_turn(&test, "only stop when you are explicitly done").await?;

    assert_eq!(
        completed.last_agent_message.as_deref(),
        Some("done after explicit completion marker")
    );

    let all_requests = requests.requests();
    assert_eq!(all_requests.len(), 2);
    assert!(
        all_requests[1]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(AUTO_CONTINUE_PREFIX)),
        "unmarked completion language should still trigger execute-mode continuation"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_stop_mode_continues_after_completed_subtask() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_assistant_message(
                    "msg-1",
                    &format!(
                        "{TASK_COMPLETE_OPEN_TAG}initial fix verified{TASK_COMPLETE_CLOSE_TAG}"
                    ),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-2", "Starting the next validation pass."),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_assistant_message(
                    "msg-3",
                    &format!(
                        "{AWAIT_USER_INPUT_OPEN_TAG}I need production credentials.</await_user_input>"
                    ),
                ),
                ev_completed("resp-3"),
            ]),
        ],
    )
    .await;

    let completed = submit_non_stop_turn(&test, "keep working until I explicitly stop you").await?;

    assert_eq!(
        completed.last_agent_message.as_deref(),
        Some("I need production credentials.")
    );

    let all_requests = requests.requests();
    assert_eq!(all_requests.len(), 3);
    assert!(
        all_requests[1]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(NON_STOP_SUBTASK_PREFIX)),
        "completed subtasks should trigger the non-stop subtask continuation prompt"
    );
    assert!(
        all_requests[2]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(NON_STOP_AUTO_CONTINUE_PREFIX)),
        "follow-up status updates should keep non-stop mode moving"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_stops_for_explicit_user_blocker() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let requests = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message(
                "msg-1",
                &format!(
                    "{AWAIT_USER_INPUT_OPEN_TAG}I need a production API key.</await_user_input>"
                ),
            ),
            ev_completed("resp-1"),
        ])],
    )
    .await;

    let completed = submit_execute_turn(&test, "keep going unless blocked").await?;

    assert_eq!(
        completed.last_agent_message.as_deref(),
        Some("I need a production API key.")
    );
    assert_eq!(
        requests.requests().len(),
        1,
        "explicit blocker should stop the turn"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_recovers_from_tool_failure_without_user_input() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let failing_call_id = "call-1";
    let recovery_call_id = "call-2";
    let recovered_path = test.workspace_path("recovered.txt");
    let failing_args = serde_json::to_string(&json!({
        "command": "definitely-not-a-real-command-12345",
        "login": false,
        "timeout_ms": 1000,
    }))?;
    let recovery_command = format!("printf fixed > {recovered_path:?} && cat {recovered_path:?}");
    let recovery_args = serde_json::to_string(&json!({
        "command": recovery_command,
        "login": false,
        "timeout_ms": 1000,
    }))?;

    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(failing_call_id, "shell_command", &failing_args),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message(
                    "msg-2",
                    "The command failed, so I will create the file directly instead.",
                ),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_function_call(recovery_call_id, "shell_command", &recovery_args),
                ev_completed("resp-3"),
            ]),
            sse(vec![
                ev_response_created("resp-4"),
                ev_assistant_message(
                    "msg-4",
                    &format!("{TASK_COMPLETE_OPEN_TAG}recovered{TASK_COMPLETE_CLOSE_TAG}"),
                ),
                ev_completed("resp-4"),
            ]),
        ],
    )
    .await;

    let completed = submit_execute_turn(&test, "recover from failures autonomously").await?;

    assert_eq!(completed.last_agent_message.as_deref(), Some("recovered"));
    assert_eq!(std::fs::read_to_string(&recovered_path)?, "fixed");

    let all_requests = requests.requests();
    assert_eq!(
        all_requests.len(),
        4,
        "expected failure, continuation, recovery, completion rounds"
    );
    assert!(
        all_requests[1]
            .function_call_output_text(failing_call_id)
            .is_some(),
        "second request should include the failed tool output"
    );
    assert!(
        all_requests[2]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(AUTO_CONTINUE_PREFIX)),
        "third request should include execute-mode continuation after a status-only recovery plan"
    );
    assert!(
        all_requests[3]
            .function_call_output_text(recovery_call_id)
            .is_some(),
        "fourth request should include the successful recovery tool output"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_injects_stall_recovery_after_repeated_status_update() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_assistant_message("msg-1", "Still working on the same verification step."),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-2", "Still working on the same verification step."),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_assistant_message(
                    "msg-3",
                    &format!(
                        "{TASK_COMPLETE_OPEN_TAG}finished after the stall recovery nudge{TASK_COMPLETE_CLOSE_TAG}"
                    ),
                ),
                ev_completed("resp-3"),
            ]),
        ],
    )
    .await;

    let completed =
        submit_execute_turn(&test, "keep executing instead of repeating status").await?;

    assert_eq!(
        completed.last_agent_message.as_deref(),
        Some("finished after the stall recovery nudge")
    );

    let all_requests = requests.requests();
    assert_eq!(all_requests.len(), 3);
    assert!(
        all_requests[1]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(AUTO_CONTINUE_PREFIX)),
        "first continuation should still use the generic execute-mode continuation"
    );
    assert!(
        all_requests[2]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(STALL_RECOVERY_PREFIX)),
        "repeated status update should trigger a stall recovery developer message"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_warns_when_stall_limit_is_hit() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let requests = mount_sse_sequence(
        &server,
        (1..=5)
            .map(|index| {
                sse(vec![
                    ev_response_created(&format!("resp-{index}")),
                    ev_assistant_message(
                        &format!("msg-{index}"),
                        "Still working on the same verification step.",
                    ),
                    ev_completed(&format!("resp-{index}")),
                ])
            })
            .collect(),
    )
    .await;

    let session_model = test.session_configured.model.clone();
    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "keep going unless you are actually making no progress".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model.clone(),
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: Some(execute_mode(session_model)),
            personality: None,
        })
        .await?;

    let turn_id = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnStarted(event) => Some(event.turn_id.clone()),
        _ => None,
    })
    .await;

    let warning = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::Warning(event)
            if event
                .message
                .contains("consecutive status-only responses without concrete progress") =>
        {
            Some(event.message.clone())
        }
        _ => None,
    })
    .await;
    let completed = wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(event) => event.turn_id == turn_id,
        _ => false,
    })
    .await;

    assert!(warning.contains("without concrete progress"));
    let EventMsg::TurnComplete(completed) = completed else {
        panic!("expected turn complete after warning");
    };
    assert_eq!(
        completed.last_agent_message.as_deref(),
        Some("Still working on the same verification step.")
    );
    assert_eq!(
        requests.requests().len(),
        5,
        "expected the turn to stop on the fifth repeated status response"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_warns_when_auto_continuation_limit_is_hit() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let requests = mount_sse_sequence(
        &server,
        (1..=17)
            .map(|index| {
                sse(vec![
                    ev_response_created(&format!("resp-{index}")),
                    ev_assistant_message(
                        &format!("msg-{index}"),
                        &format!("Still working on step {index}."),
                    ),
                    ev_completed(&format!("resp-{index}")),
                ])
            })
            .collect(),
    )
    .await;

    let session_model = test.session_configured.model.clone();
    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "keep working until forced to stop".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model.clone(),
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: Some(execute_mode(session_model)),
            personality: None,
        })
        .await?;

    let turn_id = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnStarted(event) => Some(event.turn_id.clone()),
        _ => None,
    })
    .await;

    let warning = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::Warning(event)
            if event
                .message
                .contains("Execute mode reached the auto-continuation limit") =>
        {
            Some(event.message.clone())
        }
        _ => None,
    })
    .await;
    let completed = wait_for_event(&test.codex, |event| match event {
        EventMsg::TurnComplete(event) => event.turn_id == turn_id,
        _ => false,
    })
    .await;

    assert!(warning.contains("auto-continuation limit"));
    let EventMsg::TurnComplete(completed) = completed else {
        panic!("expected turn complete after warning");
    };
    assert_eq!(
        completed.last_agent_message.as_deref(),
        Some("Still working on step 17.")
    );
    assert_eq!(
        requests.requests().len(),
        17,
        "expected the turn to stop on the 17th status-only response"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_mode_can_finish_a_multi_tool_task_without_user_input() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;

    let notes_path = test.workspace_path("notes.txt");
    let create_call_id = "call-create";
    let patch_call_id = "call-patch";
    let verify_call_id = "call-verify";
    let create_command = format!("printf before > {notes_path:?} && cat {notes_path:?}");
    let create_args = serde_json::to_string(&json!({
        "command": create_command,
        "login": false,
        "timeout_ms": 1000,
    }))?;
    let verify_command = format!("cat {notes_path:?}");
    let verify_args = serde_json::to_string(&json!({
        "command": verify_command,
        "login": false,
        "timeout_ms": 1000,
    }))?;
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-before\n+after\n*** End Patch",
        notes_path.display()
    );

    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(create_call_id, "shell_command", &create_args),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message(
                    "msg-2",
                    "The file exists now; next I will update it and verify the final contents.",
                ),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_apply_patch_custom_tool_call(patch_call_id, &patch),
                ev_completed("resp-3"),
            ]),
            sse(vec![
                ev_response_created("resp-4"),
                ev_function_call(verify_call_id, "shell_command", &verify_args),
                ev_completed("resp-4"),
            ]),
            sse(vec![
                ev_response_created("resp-5"),
                ev_assistant_message(
                    "msg-5",
                    &format!("{TASK_COMPLETE_OPEN_TAG}completed multi-tool workflow{TASK_COMPLETE_CLOSE_TAG}"),
                ),
                ev_completed("resp-5"),
            ]),
        ],
    )
    .await;

    let completed = submit_execute_turn(&test, "create, modify, verify, then finish").await?;

    assert_eq!(
        completed.last_agent_message.as_deref(),
        Some("completed multi-tool workflow")
    );
    assert_eq!(std::fs::read_to_string(&notes_path)?, "after\n");

    let all_requests = requests.requests();
    assert_eq!(all_requests.len(), 5);
    assert!(
        all_requests[1]
            .function_call_output_text(create_call_id)
            .is_some()
    );
    assert!(
        all_requests[2]
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains(AUTO_CONTINUE_PREFIX)),
        "status update after the first tool call should trigger execute-mode continuation"
    );
    assert!(
        all_requests[3]
            .custom_tool_call_output(patch_call_id)
            .is_object()
    );
    assert!(
        all_requests[4]
            .function_call_output_text(verify_call_id)
            .is_some()
    );

    Ok(())
}
