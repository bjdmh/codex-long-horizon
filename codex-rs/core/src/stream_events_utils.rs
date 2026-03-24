use std::pin::Pin;
use std::sync::Arc;

use codex_protocol::config_types::ModeKind;
use codex_protocol::items::TurnItem;
use codex_utils_stream_parser::strip_citations;
use regex_lite::Regex;
use tokio_util::sync::CancellationToken;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::error::CodexErr;
use crate::error::Result;
use crate::function_tool::FunctionCallError;
use crate::memories::citations::get_thread_id_from_citations;
use crate::parse_turn_item;
use crate::state_db;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::router::ToolRouter;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_utils_stream_parser::strip_proposed_plan_blocks;
use futures::Future;
use tracing::debug;
use tracing::instrument;

pub(crate) const TASK_COMPLETE_OPEN_TAG: &str = "<task_complete>";
pub(crate) const TASK_COMPLETE_CLOSE_TAG: &str = "</task_complete>";
pub(crate) const GOAL_COMPLETE_OPEN_TAG: &str = "<goal_complete>";
pub(crate) const GOAL_COMPLETE_CLOSE_TAG: &str = "</goal_complete>";
pub(crate) const AWAIT_USER_INPUT_OPEN_TAG: &str = "<await_user_input>";
pub(crate) const AWAIT_USER_INPUT_CLOSE_TAG: &str = "</await_user_input>";
pub(crate) const INNOVATION_CANDIDATE_OPEN_TAG: &str = "<innovation_candidate>";
pub(crate) const INNOVATION_CANDIDATE_CLOSE_TAG: &str = "</innovation_candidate>";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum AssistantControlSignal {
    #[default]
    Continue,
    AwaitUserInput,
    TaskComplete,
    GoalComplete,
}

pub(crate) fn execute_mode_auto_continue_message(attempt: usize) -> String {
    format!(
        "Continue executing the current task autonomously. This is long-run auto-continuation #{attempt}. Do not stop for a status update, a completion guess, or an optional next step. Do not ask the user whether to continue unless you are truly blocked on required information. Only end the turn if the task is complete and you include {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG}, or if you are blocked on information only the user can provide and you include {AWAIT_USER_INPUT_OPEN_TAG}...{AWAIT_USER_INPUT_CLOSE_TAG}."
    )
}

pub(crate) fn non_stop_mode_auto_continue_message(attempt: usize) -> String {
    format!(
        "Continue operating in Non-stop mode. This is non-stop auto-continuation #{attempt}. Do not stop for a status update, a completion guess, or an optional next step. Treat the active goal as an end-to-end delivery contract, not merely the next subtask. Before ending the turn, audit remaining user-visible deliverables, required verification, and any clearly implied cleanup, tests, or documentation needed for the goal to be genuinely complete. If a highest-leverage next action is available, do it now instead of stopping. If you can name the next concrete step and it fits in this turn, that is evidence you should execute it now rather than end with advice. Do not end the turn with a 'next steps', 'remaining work', or recommendation list when you already know how to do those steps yourself. If you finish a subtask, immediately search for the next concrete step and keep going. Use {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG} only if you have actively searched for the next step, exhausted the useful actions that fit in this turn, and there is truly nothing meaningful left to do here except deliberate handoff while the overall goal still remains active. If the overall user goal is already achieved, do not emit {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG}; emit {GOAL_COMPLETE_OPEN_TAG}...{GOAL_COMPLETE_CLOSE_TAG} instead. Repeated {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG} without new end-to-end work is wrong; reassess whether the goal is already complete and prefer {GOAL_COMPLETE_OPEN_TAG}...{GOAL_COMPLETE_CLOSE_TAG} when it is. Self-directed innovation is allowed only when this run explicitly enables it, it stays inside the active goal, it fits the remaining time budget, and it does not create obvious high-risk side effects. Before starting self-directed innovation, record it with {INNOVATION_CANDIDATE_OPEN_TAG}{{\"title\":\"...\",\"rationale\":\"...\",\"relevance\":\"...\",\"risk\":\"low|medium|high\",\"estimated_duration\":\"30m\"}}{INNOVATION_CANDIDATE_CLOSE_TAG} and then hand off with {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG}. Use {GOAL_COMPLETE_OPEN_TAG}...{GOAL_COMPLETE_CLOSE_TAG} only when the user's requested outcome is actually achieved and verified end to end. Use {AWAIT_USER_INPUT_OPEN_TAG}...{AWAIT_USER_INPUT_CLOSE_TAG} only when the next action truly requires information, credentials, approval, or a decision that only the user can provide. If you merely need time to pass or an external process to settle, use the `turn_sleep` tool instead."
    )
}

fn trimmed_repeated_status(repeated_status: Option<&str>) -> String {
    repeated_status
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| {
            if text.chars().count() <= 160 {
                text.to_string()
            } else {
                let truncated = text.chars().take(157).collect::<String>();
                format!("{truncated}...")
            }
        })
        .unwrap_or_else(|| "(no visible assistant update)".to_string())
}

pub(crate) fn execute_mode_stall_recovery_message(
    stall_count: usize,
    repeated_status: Option<&str>,
) -> String {
    let repeated_status = trimmed_repeated_status(repeated_status);
    format!(
        "Your last visible update repeated without concrete progress {stall_count} time(s): \"{repeated_status}\". Take a concrete next action now instead of another status update. If the task is already complete, end with {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG}. If a required external dependency is missing, end with {AWAIT_USER_INPUT_OPEN_TAG}...{AWAIT_USER_INPUT_CLOSE_TAG}."
    )
}

pub(crate) fn non_stop_mode_stall_recovery_message(
    stall_count: usize,
    repeated_status: Option<&str>,
) -> String {
    let repeated_status = trimmed_repeated_status(repeated_status);
    format!(
        "Your last visible update repeated without concrete progress {stall_count} time(s): \"{repeated_status}\". Take a concrete next action now instead of another status update. Re-check the goal at the end-to-end level: what deliverable, verification step, or clearly implied cleanup is still missing, and what is the highest-leverage next action to close that gap? Execute that action in this turn if you can. If you can already describe the next concrete step, do not stop with advice or a recommendation list—perform that step now unless it requires a real user-only blocker. If the overall user goal is truly done, end with {GOAL_COMPLETE_OPEN_TAG}...{GOAL_COMPLETE_CLOSE_TAG}. Do not use {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG} if the goal is already complete; repeated {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG} with no new end-to-end work is incorrect. If this run explicitly enables self-directed innovation and you want to start it, first record it with {INNOVATION_CANDIDATE_OPEN_TAG}...{INNOVATION_CANDIDATE_CLOSE_TAG} and then end the turn with {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG}. If you are only wrapping up the current turn after exhausting the useful actions that fit here but the goal still remains active, end with {TASK_COMPLETE_OPEN_TAG}...{TASK_COMPLETE_CLOSE_TAG} so Non-stop can continue from the next turn. Use {AWAIT_USER_INPUT_OPEN_TAG}...{AWAIT_USER_INPUT_CLOSE_TAG} only for real user-only blockers; if you are simply waiting, use the `turn_sleep` tool."
    )
}

pub(crate) fn non_stop_mode_invalid_await_user_input_message() -> String {
    format!(
        "Your previous {AWAIT_USER_INPUT_OPEN_TAG}...{AWAIT_USER_INPUT_CLOSE_TAG} did not establish a clear user-only blocker. Continue autonomously instead. Only use {AWAIT_USER_INPUT_OPEN_TAG}...{AWAIT_USER_INPUT_CLOSE_TAG} when the very next step truly requires user-provided information, credentials, approval, or a decision that cannot be inferred. If you are just waiting for time to pass or an external process to settle, call the `turn_sleep` tool instead."
    )
}

pub(crate) fn non_stop_await_user_input_justification_text<'a>(
    raw_text: Option<&'a str>,
    visible_text: Option<&'a str>,
) -> Option<&'a str> {
    raw_text
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .or_else(|| visible_text.map(str::trim).filter(|text| !text.is_empty()))
}

pub(crate) fn non_stop_await_user_input_is_justified(text: Option<&str>) -> bool {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
        return false;
    };
    let normalized = text
        .replace(AWAIT_USER_INPUT_OPEN_TAG, "")
        .replace(AWAIT_USER_INPUT_CLOSE_TAG, "")
        .to_ascii_lowercase();
    let has_user_only_blocker = [
        "credential",
        "credentials",
        "api key",
        "token",
        "secret",
        "password",
        "login",
        "sign in",
        "2fa",
        "approval",
        "permission",
        "consent",
        "confirm",
        "confirmation",
        "choose",
        "decision",
        "preference",
        "clarify",
        "clarification",
        "provide",
        "tell me",
        "which",
        "account",
        "access",
        "blocked",
        "blocker",
        "授权",
        "批准",
        "审批",
        "确认",
        "选择",
        "决定",
        "澄清",
        "提供",
        "凭证",
        "密钥",
        "令牌",
        "密码",
        "账号",
        "访问",
        "阻塞",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if !has_user_only_blocker {
        return false;
    }

    let only_waiting = [
        "wait",
        "waiting",
        "retry later",
        "poll",
        "polling",
        "settle",
        "cooldown",
        "ci",
        "build",
        "deploy",
        "deployment",
        "job",
        "startup",
        "starting up",
        "come back later",
        "稍后",
        "等待",
        "轮询",
        "重试",
        "构建",
        "部署",
        "任务",
        "启动",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));

    !only_waiting
}

pub(crate) fn normalize_execute_progress_message(text: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn assistant_control_signal_from_text(text: &str, mode: ModeKind) -> AssistantControlSignal {
    if !mode.is_autonomous() {
        return AssistantControlSignal::Continue;
    }
    if text.contains(AWAIT_USER_INPUT_OPEN_TAG) {
        return AssistantControlSignal::AwaitUserInput;
    }
    if mode == ModeKind::NonStop && text.contains(GOAL_COMPLETE_OPEN_TAG) {
        return AssistantControlSignal::GoalComplete;
    }
    if text.contains(TASK_COMPLETE_OPEN_TAG) {
        return AssistantControlSignal::TaskComplete;
    }
    AssistantControlSignal::Continue
}

fn strip_autonomous_control_tags(text: &str) -> String {
    let innovation_candidate_regex =
        match Regex::new(r"(?s)<innovation_candidate>.*?</innovation_candidate>") {
            Ok(regex) => regex,
            Err(err) => panic!("valid innovation candidate strip regex: {err}"),
        };
    innovation_candidate_regex
        .replace_all(text, "")
        .replace(TASK_COMPLETE_OPEN_TAG, "")
        .replace(TASK_COMPLETE_CLOSE_TAG, "")
        .replace(GOAL_COMPLETE_OPEN_TAG, "")
        .replace(GOAL_COMPLETE_CLOSE_TAG, "")
        .replace(AWAIT_USER_INPUT_OPEN_TAG, "")
        .replace(AWAIT_USER_INPUT_CLOSE_TAG, "")
}

fn strip_hidden_assistant_markup(text: &str, mode: ModeKind) -> String {
    let (without_citations, _) = strip_citations(text);
    let without_plan = if mode == ModeKind::Plan {
        strip_proposed_plan_blocks(&without_citations)
    } else {
        without_citations
    };
    if mode.is_autonomous() {
        strip_autonomous_control_tags(&without_plan)
    } else {
        without_plan
    }
}

pub(crate) fn raw_assistant_output_text_from_item(item: &ResponseItem) -> Option<String> {
    if let ResponseItem::Message { role, content, .. } = item
        && role == "assistant"
    {
        let combined = content
            .iter()
            .filter_map(|ci| match ci {
                codex_protocol::models::ContentItem::OutputText { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        return Some(combined);
    }
    None
}

/// Persist a completed model response item and record any cited memory usage.
pub(crate) async fn record_completed_response_item(
    sess: &Session,
    turn_context: &TurnContext,
    item: &ResponseItem,
) {
    sess.record_conversation_items(turn_context, std::slice::from_ref(item))
        .await;
    maybe_mark_thread_memory_mode_polluted_from_web_search(sess, turn_context, item).await;
    record_stage1_output_usage_for_completed_item(turn_context, item).await;
}

async fn maybe_mark_thread_memory_mode_polluted_from_web_search(
    sess: &Session,
    turn_context: &TurnContext,
    item: &ResponseItem,
) {
    if !turn_context
        .config
        .memories
        .no_memories_if_mcp_or_web_search
        || !matches!(item, ResponseItem::WebSearchCall { .. })
    {
        return;
    }
    state_db::mark_thread_memory_mode_polluted(
        sess.services.state_db.as_deref(),
        sess.conversation_id,
        "record_completed_response_item",
    )
    .await;
}

async fn record_stage1_output_usage_for_completed_item(
    turn_context: &TurnContext,
    item: &ResponseItem,
) {
    let Some(raw_text) = raw_assistant_output_text_from_item(item) else {
        return;
    };

    let (_, citations) = strip_citations(&raw_text);
    let thread_ids = get_thread_id_from_citations(citations);
    if thread_ids.is_empty() {
        return;
    }

    if let Some(db) = state_db::get_state_db(turn_context.config.as_ref(), None).await {
        let _ = db.record_stage1_output_usage(&thread_ids).await;
    }
}

/// Handle a completed output item from the model stream, recording it and
/// queuing any tool execution futures. This records items immediately so
/// history and rollout stay in sync even if the turn is later cancelled.
pub(crate) type InFlightFuture<'f> =
    Pin<Box<dyn Future<Output = Result<ResponseInputItem>> + Send + 'f>>;

#[derive(Default)]
pub(crate) struct OutputItemResult {
    pub last_agent_message: Option<String>,
    pub last_agent_raw_message: Option<String>,
    pub assistant_control_signal: AssistantControlSignal,
    pub needs_follow_up: bool,
    pub tool_future: Option<InFlightFuture<'static>>,
}

pub(crate) struct HandleOutputCtx {
    pub sess: Arc<Session>,
    pub turn_context: Arc<TurnContext>,
    pub tool_runtime: ToolCallRuntime,
    pub cancellation_token: CancellationToken,
}

#[instrument(level = "trace", skip_all)]
pub(crate) async fn handle_output_item_done(
    ctx: &mut HandleOutputCtx,
    item: ResponseItem,
    previously_active_item: Option<TurnItem>,
) -> Result<OutputItemResult> {
    let mut output = OutputItemResult::default();
    let mode = ctx.turn_context.collaboration_mode.mode;

    match ToolRouter::build_tool_call(ctx.sess.as_ref(), item.clone()).await {
        // The model emitted a tool call; log it, persist the item immediately, and queue the tool execution.
        Ok(Some(call)) => {
            let payload_preview = call.payload.log_payload().into_owned();
            tracing::info!(
                thread_id = %ctx.sess.conversation_id,
                "ToolCall: {} {}",
                call.tool_name,
                payload_preview
            );

            record_completed_response_item(ctx.sess.as_ref(), ctx.turn_context.as_ref(), &item)
                .await;

            let cancellation_token = ctx.cancellation_token.child_token();
            let tool_future: InFlightFuture<'static> = Box::pin(
                ctx.tool_runtime
                    .clone()
                    .handle_tool_call(call, cancellation_token),
            );

            output.needs_follow_up = true;
            output.tool_future = Some(tool_future);
        }
        // No tool call: convert messages/reasoning into turn items and mark them as complete.
        Ok(None) => {
            if let Some(turn_item) = handle_non_tool_response_item(&item, mode) {
                if previously_active_item.is_none() {
                    let mut started_item = turn_item.clone();
                    if let TurnItem::ImageGeneration(item) = &mut started_item {
                        item.status = "in_progress".to_string();
                        item.revised_prompt = None;
                        item.result.clear();
                    }
                    ctx.sess
                        .emit_turn_item_started(&ctx.turn_context, &started_item)
                        .await;
                }

                ctx.sess
                    .emit_turn_item_completed(&ctx.turn_context, turn_item)
                    .await;
            }

            record_completed_response_item(ctx.sess.as_ref(), ctx.turn_context.as_ref(), &item)
                .await;
            if let Some(raw_text) = raw_assistant_output_text_from_item(&item) {
                output.last_agent_raw_message = Some(raw_text.clone());
                output.assistant_control_signal =
                    assistant_control_signal_from_text(&raw_text, mode);
            }
            let last_agent_message = last_assistant_message_from_item(&item, mode);

            output.last_agent_message = last_agent_message;
        }
        // Guardrail: the model issued a LocalShellCall without an id; surface the error back into history.
        Err(FunctionCallError::MissingLocalShellCallId) => {
            let msg = "LocalShellCall without call_id or id";
            ctx.turn_context
                .otel_manager
                .log_tool_failed("local_shell", msg);
            tracing::error!(msg);

            let response = ResponseInputItem::FunctionCallOutput {
                call_id: String::new(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text(msg.to_string()),
                    ..Default::default()
                },
            };
            record_completed_response_item(ctx.sess.as_ref(), ctx.turn_context.as_ref(), &item)
                .await;
            if let Some(response_item) = response_input_to_response_item(&response) {
                ctx.sess
                    .record_conversation_items(
                        &ctx.turn_context,
                        std::slice::from_ref(&response_item),
                    )
                    .await;
            }

            output.needs_follow_up = true;
        }
        // The tool request should be answered directly (or was denied); push that response into the transcript.
        Err(FunctionCallError::RespondToModel(message)) => {
            let response = ResponseInputItem::FunctionCallOutput {
                call_id: String::new(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text(message),
                    ..Default::default()
                },
            };
            record_completed_response_item(ctx.sess.as_ref(), ctx.turn_context.as_ref(), &item)
                .await;
            if let Some(response_item) = response_input_to_response_item(&response) {
                ctx.sess
                    .record_conversation_items(
                        &ctx.turn_context,
                        std::slice::from_ref(&response_item),
                    )
                    .await;
            }

            output.needs_follow_up = true;
        }
        // A fatal error occurred; surface it back into history.
        Err(FunctionCallError::Fatal(message)) => {
            return Err(CodexErr::Fatal(message));
        }
    }

    Ok(output)
}

pub(crate) fn handle_non_tool_response_item(
    item: &ResponseItem,
    mode: ModeKind,
) -> Option<TurnItem> {
    debug!(?item, "Output item");

    match item {
        ResponseItem::Message { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. } => {
            let mut turn_item = parse_turn_item(item)?;
            if let TurnItem::AgentMessage(agent_message) = &mut turn_item {
                let combined = agent_message
                    .content
                    .iter()
                    .map(|entry| match entry {
                        codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
                    })
                    .collect::<String>();
                let stripped = strip_hidden_assistant_markup(&combined, mode);
                agent_message.content =
                    vec![codex_protocol::items::AgentMessageContent::Text { text: stripped }];
            }
            Some(turn_item)
        }
        ResponseItem::FunctionCallOutput { .. } | ResponseItem::CustomToolCallOutput { .. } => {
            debug!("unexpected tool output from stream");
            None
        }
        _ => None,
    }
}

pub(crate) fn last_assistant_message_from_item(
    item: &ResponseItem,
    mode: ModeKind,
) -> Option<String> {
    if let Some(combined) = raw_assistant_output_text_from_item(item) {
        if combined.is_empty() {
            return None;
        }
        let stripped = strip_hidden_assistant_markup(&combined, mode);
        if stripped.trim().is_empty() {
            return None;
        }
        return Some(stripped);
    }
    None
}

pub(crate) fn response_input_to_response_item(input: &ResponseInputItem) -> Option<ResponseItem> {
    match input {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            Some(ResponseItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: output.clone(),
            })
        }
        ResponseInputItem::CustomToolCallOutput { call_id, output } => {
            Some(ResponseItem::CustomToolCallOutput {
                call_id: call_id.clone(),
                output: output.clone(),
            })
        }
        ResponseInputItem::McpToolCallOutput { call_id, result } => {
            let output = match result {
                Ok(call_tool_result) => FunctionCallOutputPayload::from(call_tool_result),
                Err(err) => FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text(err.clone()),
                    success: Some(false),
                },
            };
            Some(ResponseItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::AWAIT_USER_INPUT_OPEN_TAG;
    use super::AssistantControlSignal;
    use super::TASK_COMPLETE_OPEN_TAG;
    use super::execute_mode_stall_recovery_message;
    use super::handle_non_tool_response_item;
    use super::last_assistant_message_from_item;
    use super::non_stop_await_user_input_is_justified;
    use super::non_stop_await_user_input_justification_text;
    use super::non_stop_mode_auto_continue_message;
    use super::non_stop_mode_stall_recovery_message;
    use super::normalize_execute_progress_message;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::items::TurnItem;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use pretty_assertions::assert_eq;

    fn assistant_output_text(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: Some(true),
            phase: None,
        }
    }

    #[test]
    fn handle_non_tool_response_item_strips_citations_from_assistant_message() {
        let item = assistant_output_text("hello<oai-mem-citation>doc1</oai-mem-citation> world");

        let turn_item = handle_non_tool_response_item(&item, ModeKind::Default)
            .expect("assistant message should parse");

        let TurnItem::AgentMessage(agent_message) = turn_item else {
            panic!("expected agent message");
        };
        let text = agent_message
            .content
            .iter()
            .map(|entry| match entry {
                codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
            })
            .collect::<String>();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn last_assistant_message_from_item_strips_citations_and_plan_blocks() {
        let item = assistant_output_text(
            "before<oai-mem-citation>doc1</oai-mem-citation>\n<proposed_plan>\n- x\n</proposed_plan>\nafter",
        );

        let message = last_assistant_message_from_item(&item, ModeKind::Plan)
            .expect("assistant text should remain after stripping");

        assert_eq!(message, "before\nafter");
    }

    #[test]
    fn last_assistant_message_from_item_returns_none_for_citation_only_message() {
        let item = assistant_output_text("<oai-mem-citation>doc1</oai-mem-citation>");

        assert_eq!(
            last_assistant_message_from_item(&item, ModeKind::Default),
            None
        );
    }

    #[test]
    fn last_assistant_message_from_item_returns_none_for_plan_only_hidden_message() {
        let item = assistant_output_text("<proposed_plan>\n- x\n</proposed_plan>");

        assert_eq!(
            last_assistant_message_from_item(&item, ModeKind::Plan),
            None
        );
    }

    #[test]
    fn handle_non_tool_response_item_strips_execute_control_tags() {
        let item = assistant_output_text("<task_complete>Done and verified.</task_complete>");

        let turn_item = handle_non_tool_response_item(&item, ModeKind::LongRun)
            .expect("assistant message should parse");

        let TurnItem::AgentMessage(agent_message) = turn_item else {
            panic!("expected agent message");
        };
        let text = agent_message
            .content
            .iter()
            .map(|entry| match entry {
                codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
            })
            .collect::<String>();
        assert_eq!(text, "Done and verified.");
    }

    #[test]
    fn handle_non_tool_response_item_strips_non_stop_goal_complete_tags() {
        let item = assistant_output_text("<goal_complete>Done and verified.</goal_complete>");

        let turn_item = handle_non_tool_response_item(&item, ModeKind::NonStop)
            .expect("assistant message should parse");

        let TurnItem::AgentMessage(agent_message) = turn_item else {
            panic!("expected agent message");
        };
        let text = agent_message
            .content
            .iter()
            .map(|entry| match entry {
                codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
            })
            .collect::<String>();
        assert_eq!(text, "Done and verified.");
    }

    #[test]
    fn output_item_result_defaults_to_continue_control_signal() {
        assert_eq!(
            super::OutputItemResult::default().assistant_control_signal,
            AssistantControlSignal::Continue
        );
    }

    #[test]
    fn normalize_execute_progress_message_collapses_whitespace() {
        assert_eq!(
            normalize_execute_progress_message(
                " still   working
 on	this "
            ),
            Some("still working on this".to_string())
        );
        assert_eq!(
            normalize_execute_progress_message(
                "   
	  "
            ),
            None
        );
    }

    #[test]
    fn execute_mode_stall_recovery_message_includes_repeated_status() {
        let message = execute_mode_stall_recovery_message(2, Some("Still working on it."));

        assert!(message.contains("Still working on it."));
        assert!(message.contains(TASK_COMPLETE_OPEN_TAG));
        assert!(message.contains(AWAIT_USER_INPUT_OPEN_TAG));
    }

    #[test]
    fn non_stop_messages_emphasize_end_to_end_completion() {
        let auto_continue = non_stop_mode_auto_continue_message(3);
        assert!(auto_continue.contains("end-to-end delivery contract"));
        assert!(auto_continue.contains("clearly implied cleanup, tests, or documentation"));
        assert!(auto_continue.contains("achieved and verified end to end"));
        assert!(auto_continue.contains("do not emit"));
        assert!(auto_continue.contains("Repeated <task_complete>...</task_complete>"));
        assert!(auto_continue.contains("execute it now"));
        assert!(auto_continue.contains("recommendation list"));
        assert!(auto_continue.contains("actively searched for the next step"));

        let stall_recovery = non_stop_mode_stall_recovery_message(2, Some("Still monitoring."));
        assert!(stall_recovery.contains("end-to-end level"));
        assert!(stall_recovery.contains("highest-leverage next action"));
        assert!(stall_recovery.contains("Still monitoring."));
        assert!(stall_recovery.contains("repeated <task_complete>...</task_complete>"));
        assert!(stall_recovery.contains("do not stop with advice or a recommendation list"));
    }

    #[test]
    fn non_stop_await_user_input_requires_clear_user_only_blocker() {
        assert!(non_stop_await_user_input_is_justified(Some(
            "I need production credentials before I can continue."
        )));
        assert!(non_stop_await_user_input_is_justified(Some(
            "<await_user_input>I need production credentials before I can continue.</await_user_input>"
        )));
        assert!(non_stop_await_user_input_is_justified(Some(
            "I am still blocked in the test fixture until you provide the missing credential."
        )));
        assert!(!non_stop_await_user_input_is_justified(Some(
            "I need to wait for CI to finish."
        )));
        assert!(!non_stop_await_user_input_is_justified(Some(
            "Waiting for the deployment job to settle before checking again."
        )));
    }

    #[test]
    fn non_stop_await_user_input_uses_raw_reason_when_visible_text_is_empty() {
        let text = non_stop_await_user_input_justification_text(
            Some(
                "<await_user_input>I need your approval for production access.</await_user_input>",
            ),
            None,
        );

        assert_eq!(
            text,
            Some(
                "<await_user_input>I need your approval for production access.</await_user_input>"
            )
        );
    }
}
