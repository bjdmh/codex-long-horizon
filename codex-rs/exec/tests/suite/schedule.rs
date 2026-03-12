#![allow(clippy::expect_used, clippy::unwrap_used)]

use anyhow::Context;
use core_test_support::test_codex_exec::test_codex_exec;
use pretty_assertions::assert_eq;
use serde_json::Value;
use walkdir::WalkDir;

fn session_file(home_path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
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
        "expected exactly one session rollout"
    );
    Ok(session_files[0].path().to_path_buf())
}

fn extract_conversation_id(path: &std::path::Path) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read session file {}", path.display()))?;
    let meta_line = content
        .lines()
        .next()
        .context("missing session meta line")?;
    let meta: Value = serde_json::from_str(meta_line).context("parse session meta")?;
    meta.get("payload")
        .and_then(|payload| payload.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .context("missing conversation id in session meta")
}

#[test]
fn exec_schedule_create_list_and_cancel_round_trip() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let fixture =
        codex_utils_cargo_bin::find_resource!("tests/fixtures/cli_responses_fixture.sse")?;

    std::fs::write(
        test.home_path().join("config.toml"),
        "model = \"gpt-5.4\"\n",
    )?;

    test.cmd()
        .env("CODEX_RS_SSE_FIXTURE", &fixture)
        .env("OPENAI_BASE_URL", "http://unused.local")
        .arg("--skip-git-repo-check")
        .arg("seed persisted session for schedule testing")
        .assert()
        .success();

    let session_path = session_file(test.home_path())?;
    let session_id = extract_conversation_id(&session_path)?;

    test.cmd()
        .arg("--skip-git-repo-check")
        .arg("schedule")
        .arg("create")
        .arg("--session-id")
        .arg(&session_id)
        .arg("--every")
        .arg("10m")
        .arg("check deployment logs")
        .assert()
        .success();

    let list_output = test
        .cmd()
        .arg("--skip-git-repo-check")
        .arg("--json")
        .arg("schedule")
        .arg("list")
        .arg("--session-id")
        .arg(&session_id)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let schedules: Vec<Value> = serde_json::from_slice(&list_output)?;
    assert_eq!(schedules.len(), 1);
    let schedule = &schedules[0];
    assert_eq!(schedule.get("kind").and_then(Value::as_str), Some("loop"));
    assert_eq!(
        schedule.get("status").and_then(Value::as_str),
        Some("active")
    );
    assert_eq!(
        schedule.get("interval_seconds").and_then(Value::as_u64),
        Some(600)
    );
    let schedule_id = schedule
        .get("id")
        .and_then(Value::as_str)
        .context("missing schedule id")?
        .to_string();

    let cancel_output = test
        .cmd()
        .arg("--skip-git-repo-check")
        .arg("--json")
        .arg("schedule")
        .arg("cancel")
        .arg(&schedule_id)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let cancelled: Value = serde_json::from_slice(&cancel_output)?;
    assert_eq!(
        cancelled.get("cancelled").and_then(Value::as_bool),
        Some(true)
    );

    let list_after_cancel_output = test
        .cmd()
        .arg("--skip-git-repo-check")
        .arg("--json")
        .arg("schedule")
        .arg("list")
        .arg("--session-id")
        .arg(&session_id)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let schedules_after_cancel: Vec<Value> = serde_json::from_slice(&list_after_cancel_output)?;
    assert_eq!(schedules_after_cancel.len(), 1);
    assert_eq!(
        schedules_after_cancel[0]
            .get("status")
            .and_then(Value::as_str),
        Some("cancelled")
    );

    Ok(())
}
