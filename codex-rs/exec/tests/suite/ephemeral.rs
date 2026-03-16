#![cfg(not(target_os = "windows"))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use codex_core::NonStopCheckpoint;
use codex_core::NonStopCheckpointStatus;
use codex_core::non_stop_checkpoint_path;
use codex_utils_cargo_bin::find_resource;
use core_test_support::test_codex_exec::test_codex_exec;
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
        rollout.contains("Collaboration Style: Execute"),
        "expected Execute collaboration instructions in session rollout, got: {rollout}"
    );
    Ok(())
}

#[test]
fn non_stop_uses_non_stop_defaults_without_rewriting_codex_home() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let fixture = find_resource!("tests/fixtures/cli_responses_fixture.sse")?;
    let source_config = test.home_path().join("config.toml");
    std::fs::write(
        &source_config,
        "model = \"gpt-4.1\"\ninitial_collaboration_mode = \"execute\"\n",
    )?;

    test.cmd()
        .env("CODEX_RS_SSE_FIXTURE", &fixture)
        .env("OPENAI_BASE_URL", "http://unused.local")
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
