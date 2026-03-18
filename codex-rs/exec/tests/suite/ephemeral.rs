#![cfg(not(target_os = "windows"))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use codex_core::NonStopCheckpoint;
use codex_core::NonStopCheckpointControlSignal;
use codex_core::NonStopCheckpointStatus;
use codex_core::non_stop_checkpoint_path;
use codex_utils_cargo_bin::find_resource;
use core_test_support::responses;
use core_test_support::test_codex_exec::test_codex_exec;
use pretty_assertions::assert_eq;
use serde_json::Value;
use walkdir::WalkDir;

fn session_rollout_count(home_path: &std::path::Path) -> usize {
    let sessions_dir = home_path.join("sessions");
    if !sessions_dir.exists() {
        return 0;
    }

    WalkDir::new(sessions_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
        .count()
}

fn session_rollout_contents(home_path: &std::path::Path) -> String {
    let sessions_dir = home_path.join("sessions");
    let session_files: Vec<_> = WalkDir::new(sessions_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
        .collect();
    assert_eq!(
        session_files.len(),
        1,
        "expected exactly one session rollout file"
    );
    std::fs::read_to_string(session_files[0].path()).expect("read session rollout")
}

fn find_session_file_containing_marker(
    sessions_dir: &std::path::Path,
    marker: &str,
) -> Option<std::path::PathBuf> {
    for entry in WalkDir::new(sessions_dir) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() || !entry.file_name().to_string_lossy().ends_with(".jsonl")
        {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for line in content.lines().skip(1) {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(item): Result<Value, _> = serde_json::from_str(line) else {
                continue;
            };
            if item.get("type").and_then(Value::as_str) == Some("response_item")
                && let Some(payload) = item.get("payload")
                && payload.get("type").and_then(Value::as_str) == Some("message")
                && payload
                    .get("content")
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default()
                    .contains(marker)
            {
                return Some(entry.path().to_path_buf());
            }
        }
    }
    None
}

fn extract_conversation_id(path: &std::path::Path) -> String {
    let content = std::fs::read_to_string(path).expect("read rollout");
    let meta_line = content.lines().next().expect("missing meta line");
    let meta: Value = serde_json::from_str(meta_line).expect("invalid meta json");
    meta.get("payload")
        .and_then(|payload| payload.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn persists_rollout_file_by_default() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let fixture = find_resource!("tests/fixtures/cli_responses_fixture.sse")?;

    test.cmd()
        .env("CODEX_RS_SSE_FIXTURE", &fixture)
        .env("OPENAI_BASE_URL", "http://unused.local")
        .arg("--skip-git-repo-check")
        .arg("default persistence behavior")
        .assert()
        .code(0);

    assert_eq!(session_rollout_count(test.home_path()), 1);
    Ok(())
}

#[test]
fn default_exec_sessions_use_execute_collaboration_mode() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let fixture = find_resource!("tests/fixtures/cli_responses_fixture.sse")?;

    test.cmd()
        .env("CODEX_RS_SSE_FIXTURE", &fixture)
        .env("OPENAI_BASE_URL", "http://unused.local")
        .arg("--skip-git-repo-check")
        .arg("default execute mode behavior")
        .assert()
        .code(0);

    let rollout = session_rollout_contents(test.home_path());
    assert!(
        rollout.contains("Collaboration Style: Long-Run"),
        "expected Long-Run collaboration instructions in session rollout, got: {rollout}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_stop_uses_non_stop_defaults_without_rewriting_codex_home() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_assistant_message(
                "msg-1",
                "<await_user_input>I need credentials before continuing.</await_user_input>",
            ),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;
    let source_config = test.home_path().join("config.toml");
    std::fs::write(
        &source_config,
        "model = \"gpt-4.1\"\ninitial_collaboration_mode = \"execute\"\n",
    )?;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--non-stop")
        .arg("non-stop behavior")
        .assert()
        .code(0);

    assert_eq!(session_rollout_count(test.home_path()), 1);
    assert_eq!(
        std::fs::read_to_string(&source_config)?,
        "model = \"gpt-4.1\"\ninitial_collaboration_mode = \"execute\"\n"
    );

    let rollout = session_rollout_contents(test.home_path());
    assert!(
        rollout.contains("Collaboration Style: Non-stop"),
        "expected Non-stop collaboration instructions in session rollout, got: {rollout}"
    );
    assert!(
        rollout.contains("gpt-5.4"),
        "expected Non-stop default model in session rollout, got: {rollout}"
    );

    let thread_id = extract_conversation_id(
        &find_session_file_containing_marker(
            &test.home_path().join("sessions"),
            "non-stop behavior",
        )
        .expect("session file"),
    );
    let thread_id =
        codex_protocol::ThreadId::from_string(&thread_id).expect("parse checkpoint thread id");
    let checkpoint: NonStopCheckpoint = serde_json::from_str(
        &std::fs::read_to_string(non_stop_checkpoint_path(test.home_path(), thread_id))
            .expect("read checkpoint"),
    )
    .expect("parse checkpoint");
    assert_eq!(checkpoint.status, NonStopCheckpointStatus::TurnComplete);
    assert_eq!(checkpoint.goal_prompt.as_deref(), Some("non-stop behavior"));
    assert_eq!(
        checkpoint.last_assistant_control_signal,
        Some(NonStopCheckpointControlSignal::AwaitUserInput)
    );

    Ok(())
}

#[test]
fn does_not_persist_rollout_file_in_ephemeral_mode() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let fixture = find_resource!("tests/fixtures/cli_responses_fixture.sse")?;

    test.cmd()
        .env("CODEX_RS_SSE_FIXTURE", &fixture)
        .env("OPENAI_BASE_URL", "http://unused.local")
        .arg("--skip-git-repo-check")
        .arg("--ephemeral")
        .arg("ephemeral behavior")
        .assert()
        .code(0);

    assert_eq!(session_rollout_count(test.home_path()), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_stop_auto_starts_follow_up_turn_until_blocked() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let requests = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_assistant_message("msg-1", "finished the first subtask"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_assistant_message(
                    "msg-2",
                    "<await_user_input>I need production credentials.</await_user_input>",
                ),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--non-stop")
        .arg("ship the feature")
        .assert()
        .code(0);

    let all_requests = requests.requests();
    assert_eq!(all_requests.len(), 2);
    assert!(
        all_requests[1].body_contains_text("ship the feature"),
        "expected second request to continue the same goal, got: {:?}",
        all_requests[1].body_json()
    );

    let thread_id = extract_conversation_id(
        &find_session_file_containing_marker(
            &test.home_path().join("sessions"),
            "ship the feature",
        )
        .expect("session file"),
    );
    let thread_id =
        codex_protocol::ThreadId::from_string(&thread_id).expect("parse checkpoint thread id");
    let checkpoint: NonStopCheckpoint = serde_json::from_str(
        &std::fs::read_to_string(non_stop_checkpoint_path(test.home_path(), thread_id))
            .expect("read checkpoint"),
    )
    .expect("parse checkpoint");
    assert_eq!(checkpoint.status, NonStopCheckpointStatus::TurnComplete);
    assert_eq!(
        checkpoint.last_assistant_control_signal,
        Some(NonStopCheckpointControlSignal::AwaitUserInput)
    );
    assert_eq!(checkpoint.goal_prompt.as_deref(), Some("ship the feature"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_stop_rejects_unnecessary_await_user_input_and_keeps_running() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let requests = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_assistant_message(
                    "msg-1",
                    "<await_user_input>I need to wait for CI to finish before checking again.</await_user_input>",
                ),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_assistant_message(
                    "msg-2",
                    "<task_complete>Nothing meaningful remains after the wait.</task_complete>",
                ),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--non-stop")
        .arg("keep going after rejecting pointless await")
        .assert()
        .code(0);

    let all_requests = requests.requests();
    assert_eq!(all_requests.len(), 2);
    assert!(
        all_requests[1]
            .message_input_texts("developer")
            .iter()
            .any(
                |text| text.contains("did not establish a clear user-only blocker")
                    && text.contains("turn_sleep")
            ),
        "expected second request to include the invalid await_user_input correction, got: {:?}",
        all_requests[1].body_json()
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_stop_task_complete_stops_cleanly_and_writes_last_message() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let requests = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_assistant_message(
                "msg-1",
                "Checked for the next step.\n<task_complete>Nothing meaningful remains.</task_complete>",
            ),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;
    let last_message_path = test.home_path().join("last-message.txt");

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--non-stop")
        .arg("--output-last-message")
        .arg(&last_message_path)
        .arg("finish the migration")
        .assert()
        .code(0);

    assert_eq!(requests.requests().len(), 1);
    assert_eq!(
        std::fs::read_to_string(&last_message_path)?,
        "Checked for the next step.\nNothing meaningful remains."
    );

    let thread_id = extract_conversation_id(
        &find_session_file_containing_marker(
            &test.home_path().join("sessions"),
            "finish the migration",
        )
        .expect("session file"),
    );
    let thread_id =
        codex_protocol::ThreadId::from_string(&thread_id).expect("parse checkpoint thread id");
    let checkpoint: NonStopCheckpoint = serde_json::from_str(
        &std::fs::read_to_string(non_stop_checkpoint_path(test.home_path(), thread_id))
            .expect("read checkpoint"),
    )
    .expect("parse checkpoint");
    assert_eq!(checkpoint.status, NonStopCheckpointStatus::TurnComplete);
    assert_eq!(
        checkpoint.last_assistant_control_signal,
        Some(NonStopCheckpointControlSignal::TaskComplete)
    );
    assert_eq!(
        checkpoint.last_agent_message.as_deref(),
        Some("Checked for the next step.\nNothing meaningful remains.")
    );
    assert_eq!(
        checkpoint.goal_prompt.as_deref(),
        Some("finish the migration")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_stop_resume_without_prompt_uses_checkpoint_goal_prompt() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let first_run_requests = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_assistant_message(
                "msg-1",
                "<await_user_input>I need credentials before I can continue.</await_user_input>",
            ),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--non-stop")
        .arg("resume target goal")
        .assert()
        .code(0);

    assert_eq!(first_run_requests.requests().len(), 1);

    let resumed_requests = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-2"),
            responses::ev_assistant_message(
                "msg-2",
                "<await_user_input>still blocked in the test fixture.</await_user_input>",
            ),
            responses::ev_completed("resp-2"),
        ]),
    )
    .await;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--non-stop")
        .arg("resume")
        .arg("--last")
        .assert()
        .code(0);

    let resume_request = resumed_requests.single_request();
    assert!(
        resume_request.has_message_with_input_texts("user", |texts| {
            texts
                .iter()
                .any(|text| text.contains("Goal: resume target goal"))
        }),
        "expected resume without prompt to synthesize a non-stop supervisor prompt, got: {:?}",
        resume_request.message_input_text_groups("user")
    );

    Ok(())
}
