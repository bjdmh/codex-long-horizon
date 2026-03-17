use std::time::Duration;

use async_trait::async_trait;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use serde::Deserialize;
use tokio::time::sleep;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

const MAX_SLEEP_MS: u64 = 600_000;

#[derive(Debug, Deserialize)]
struct SleepArgs {
    duration_ms: u64,
}

pub struct SleepHandler;

fn turn_sleep_item(call_id: &str, text: String) -> TurnItem {
    TurnItem::AgentMessage(AgentMessageItem {
        id: format!("turn-sleep-{call_id}"),
        content: vec![AgentMessageContent::Text { text }],
        phase: None,
    })
}

#[async_trait]
impl ToolHandler for SleepHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "turn_sleep handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: SleepArgs = parse_arguments(&arguments)?;
        if args.duration_ms == 0 {
            return Err(FunctionCallError::RespondToModel(
                "turn_sleep duration_ms must be greater than zero".to_string(),
            ));
        }
        if args.duration_ms > MAX_SLEEP_MS {
            return Err(FunctionCallError::RespondToModel(format!(
                "turn_sleep duration_ms must be less than or equal to {MAX_SLEEP_MS}"
            )));
        }

        let started_item = turn_sleep_item(
            &call_id,
            format!("turn_sleep: waiting for {} ms", args.duration_ms),
        );
        session
            .send_event(
                &turn,
                EventMsg::ItemStarted(ItemStartedEvent {
                    thread_id: session.conversation_id,
                    turn_id: turn.sub_id.clone(),
                    item: started_item.clone(),
                }),
            )
            .await;

        sleep(Duration::from_millis(args.duration_ms)).await;

        session
            .send_event(
                &turn,
                EventMsg::ItemCompleted(ItemCompletedEvent {
                    thread_id: session.conversation_id,
                    turn_id: turn.sub_id.clone(),
                    item: turn_sleep_item(
                        &call_id,
                        format!("turn_sleep: finished waiting {} ms", args.duration_ms),
                    ),
                }),
            )
            .await;

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(format!("slept for {} ms", args.duration_ms)),
            success: Some(true),
        })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn sleep_upper_bound_matches_tool_contract() {
        assert_eq!(MAX_SLEEP_MS, 600_000);
    }
}
