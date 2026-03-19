// - In the default output mode, it is paramount that the only thing written to
//   stdout is the final message (if any).
// - In --json mode, stdout must be valid JSONL, one event per line.
// For both modes, any other output must be written to stderr.
#![deny(clippy::print_stdout)]

mod cli;
mod event_processor;
mod event_processor_with_human_output;
pub mod event_processor_with_jsonl_output;
pub mod exec_events;

pub use cli::Cli;
pub use cli::Command;
pub use cli::ReviewArgs;
use codex_arg0::Arg0DispatchPaths;
use codex_cloud_requirements::cloud_requirements_loader;
use codex_core::AuthManager;
use codex_core::LMSTUDIO_OSS_PROVIDER_ID;
use codex_core::NewThread;
use codex_core::NonStopBudgetSource;
use codex_core::NonStopCheckpoint;
use codex_core::NonStopCheckpointControlSignal;
use codex_core::NonStopCheckpointStatus;
use codex_core::NonStopInnovationRisk;
use codex_core::NonStopInnovationStatus;
use codex_core::OLLAMA_OSS_PROVIDER_ID;
use codex_core::RegisterNonStopSessionOptions;
use codex_core::ThreadManager;
use codex_core::auth::enforce_login_restrictions;
use codex_core::check_execpolicy_for_warnings;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core::config::ConfigOverrides;
use codex_core::config::find_codex_home;
use codex_core::config::load_config_as_toml_with_cli_overrides;
use codex_core::config::resolve_oss_provider;
use codex_core::config_loader::ConfigLoadError;
use codex_core::config_loader::format_config_error_with_source;
use codex_core::format_exec_policy_error_with_source;
use codex_core::git_info::get_git_repo_root;
use codex_core::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_core::models_manager::manager::RefreshStrategy;
use codex_core::read_non_stop_checkpoint;
use codex_core::register_non_stop_session;
use codex_otel::set_parent_from_context;
use codex_otel::traceparent_context_from_env;
use codex_protocol::approvals::ElicitationAction;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::config_types::Settings;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::TurnCompleteReason;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_oss::ensure_oss_provider_ready;
use codex_utils_oss::get_default_model_for_oss_provider;
use event_processor_with_human_output::EventProcessorWithHumanOutput;
use event_processor_with_jsonl_output::EventProcessorWithJsonOutput;
use serde_json::Value;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use supports_color::Stream;
use tokio::sync::Mutex;
use tracing::Instrument;
use tracing::debug;
use tracing::error;
use tracing::field;
use tracing::info;
use tracing::info_span;
use tracing::warn;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

use crate::cli::Command as ExecCommand;
use crate::event_processor::CodexStatus;
use crate::event_processor::EventProcessor;
use codex_core::default_client::set_default_client_residency_requirement;
use codex_core::default_client::set_default_originator;
use codex_core::find_thread_path_by_id_str;
use codex_core::find_thread_path_by_name_str;

const DEFAULT_ANALYTICS_ENABLED: bool = true;
const NON_STOP_DEFAULT_MODEL: &str = "gpt-5.4";
const DEFAULT_NON_STOP_RESUME_PROMPT: &str = "Resume Non-stop execution toward the active goal from the existing thread state. Review the thread, avoid redoing finished work, and continue with the next highest-leverage concrete task.";

fn self_directed_innovation_requires_non_stop_error() -> &'static str {
    "--self-directed-innovation requires Non-stop mode. Pass --non-stop or set initial_collaboration_mode=\"non_stop\" in config."
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

enum InitialOperation {
    UserTurn {
        items: Vec<UserInput>,
        output_schema: Option<Value>,
    },
    Review {
        review_request: ReviewRequest,
    },
}

#[derive(Clone)]
struct ThreadEventEnvelope {
    thread_id: codex_protocol::ThreadId,
    thread: Arc<codex_core::CodexThread>,
    event: Event,
    suppress_output: bool,
}

struct ExecRunArgs {
    command: Option<ExecCommand>,
    config: Config,
    cursor_ansi: bool,
    dangerously_bypass_approvals_and_sandbox: bool,
    exec_span: tracing::Span,
    images: Vec<PathBuf>,
    json_mode: bool,
    last_message_file: Option<PathBuf>,
    model_provider: Option<String>,
    oss: bool,
    output_schema_path: Option<PathBuf>,
    prompt: Option<String>,
    skip_git_repo_check: bool,
    self_directed_innovation: bool,
    stderr_with_ansi: bool,
}

#[derive(Clone)]
struct TurnSubmitDefaults {
    cwd: PathBuf,
    approval_policy: AskForApproval,
    sandbox_policy: codex_protocol::protocol::SandboxPolicy,
    model: String,
    effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    collaboration_mode: Option<CollaborationMode>,
    output_schema: Option<Value>,
}

fn exec_root_span() -> tracing::Span {
    info_span!(
        "codex.exec",
        otel.kind = "internal",
        thread.id = field::Empty,
        turn.id = field::Empty,
    )
}

pub async fn run_main(cli: Cli, arg0_paths: Arg0DispatchPaths) -> anyhow::Result<()> {
    if let Err(err) = set_default_originator("codex_exec".to_string()) {
        tracing::warn!(?err, "Failed to set codex exec originator override {err:?}");
    }

    let Cli {
        command,
        images,
        model: model_cli_arg,
        oss,
        oss_provider,
        config_profile,
        non_stop,
        self_directed_innovation,
        full_auto,
        dangerously_bypass_approvals_and_sandbox,
        cwd,
        skip_git_repo_check,
        add_dir,
        ephemeral,
        color,
        last_message_file,
        json: json_mode,
        sandbox_mode: sandbox_mode_cli_arg,
        prompt,
        output_schema: output_schema_path,
        config_overrides,
        progress_cursor,
    } = cli;
    let mut config_overrides = config_overrides;

    let (_stdout_with_ansi, stderr_with_ansi) = match color {
        cli::Color::Always => (true, true),
        cli::Color::Never => (false, false),
        cli::Color::Auto => (
            supports_color::on_cached(Stream::Stdout).is_some(),
            supports_color::on_cached(Stream::Stderr).is_some(),
        ),
    };
    let cursor_ansi = if progress_cursor {
        true
    } else {
        match color {
            cli::Color::Never => false,
            cli::Color::Always => true,
            cli::Color::Auto => {
                if stderr_with_ansi || std::io::stderr().is_terminal() {
                    true
                } else {
                    match std::env::var("TERM") {
                        Ok(term) => !term.is_empty() && term != "dumb",
                        Err(_) => false,
                    }
                }
            }
        }
    };

    // Build fmt layer (existing logging) to compose with OTEL layer.
    let default_level = "error";

    // Build env_filter separately and attach via with_filter.
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(stderr_with_ansi)
        .with_writer(std::io::stderr)
        .with_filter(env_filter);

    let sandbox_mode = if full_auto {
        Some(SandboxMode::WorkspaceWrite)
    } else if dangerously_bypass_approvals_and_sandbox {
        Some(SandboxMode::DangerFullAccess)
    } else {
        sandbox_mode_cli_arg.map(Into::<SandboxMode>::into)
    };
    if non_stop {
        config_overrides
            .raw_overrides
            .push("initial_collaboration_mode=\"non_stop\"".to_string());
    }

    // Parse `-c` overrides from the CLI.
    let cli_kv_overrides = match config_overrides.parse_overrides() {
        Ok(v) => v,
        #[allow(clippy::print_stderr)]
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };

    let resolved_cwd = cwd.clone();
    let config_cwd = match resolved_cwd.as_deref() {
        Some(path) => AbsolutePathBuf::from_absolute_path(path.canonicalize()?)?,
        None => AbsolutePathBuf::current_dir()?,
    };

    // we load config.toml here to determine project state.
    #[allow(clippy::print_stderr)]
    let codex_home = match find_codex_home() {
        Ok(codex_home) => codex_home,
        Err(err) => {
            eprintln!("Error finding codex home: {err}");
            std::process::exit(1);
        }
    };

    #[allow(clippy::print_stderr)]
    let config_toml = match load_config_as_toml_with_cli_overrides(
        &codex_home,
        &config_cwd,
        cli_kv_overrides.clone(),
    )
    .await
    {
        Ok(config_toml) => config_toml,
        Err(err) => {
            let config_error = err
                .get_ref()
                .and_then(|err| err.downcast_ref::<ConfigLoadError>())
                .map(ConfigLoadError::config_error);
            if let Some(config_error) = config_error {
                eprintln!(
                    "Error loading config.toml:\n{}",
                    format_config_error_with_source(config_error)
                );
            } else {
                eprintln!("Error loading config.toml: {err}");
            }
            std::process::exit(1);
        }
    };

    let cloud_auth_manager = AuthManager::shared(
        codex_home.clone(),
        false,
        config_toml.cli_auth_credentials_store.unwrap_or_default(),
    );
    let chatgpt_base_url = config_toml
        .chatgpt_base_url
        .clone()
        .unwrap_or_else(|| "https://chatgpt.com/backend-api/".to_string());
    // TODO(gt): Make cloud requirements failures blocking once we can fail-closed.
    let cloud_requirements =
        cloud_requirements_loader(cloud_auth_manager, chatgpt_base_url, codex_home.clone());

    let model_provider = if oss {
        let resolved = resolve_oss_provider(
            oss_provider.as_deref(),
            &config_toml,
            config_profile.clone(),
        );

        if let Some(provider) = resolved {
            Some(provider)
        } else {
            return Err(anyhow::anyhow!(
                "No default OSS provider configured. Use --local-provider=provider or set oss_provider to one of: {LMSTUDIO_OSS_PROVIDER_ID}, {OLLAMA_OSS_PROVIDER_ID} in config.toml"
            ));
        }
    } else {
        None // No OSS mode enabled
    };

    // When using `--oss`, let the bootstrapper pick the model based on selected provider
    let model = if let Some(model) = model_cli_arg {
        Some(model)
    } else if oss {
        model_provider
            .as_ref()
            .and_then(|provider_id| get_default_model_for_oss_provider(provider_id))
            .map(std::borrow::ToOwned::to_owned)
    } else if non_stop {
        Some(NON_STOP_DEFAULT_MODEL.to_string())
    } else {
        None // No model specified, will use the default.
    };

    // Load configuration and determine approval policy
    let overrides = ConfigOverrides {
        model,
        review_model: None,
        config_profile,
        // Default to never ask for approvals in headless mode. Feature flags can override.
        approval_policy: Some(AskForApproval::Never),
        sandbox_mode,
        cwd: resolved_cwd,
        model_provider: model_provider.clone(),
        service_tier: None,
        codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe.clone(),
        main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe.clone(),
        js_repl_node_path: None,
        js_repl_node_module_dirs: None,
        zsh_path: None,
        base_instructions: None,
        developer_instructions: None,
        personality: None,
        compact_prompt: None,
        include_apply_patch_tool: None,
        show_raw_agent_reasoning: oss.then_some(true),
        tools_web_search_request: None,
        ephemeral: ephemeral.then_some(true),
        additional_writable_roots: add_dir,
    };

    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .cloud_requirements(cloud_requirements)
        .build()
        .await?;

    if self_directed_innovation && config.initial_collaboration_mode != ModeKind::NonStop {
        eprintln!("{}", self_directed_innovation_requires_non_stop_error());
        std::process::exit(1);
    }

    #[allow(clippy::print_stderr)]
    match check_execpolicy_for_warnings(&config.config_layer_stack).await {
        Ok(None) => {}
        Ok(Some(err)) | Err(err) => {
            eprintln!(
                "Error loading rules:\n{}",
                format_exec_policy_error_with_source(&err)
            );
            std::process::exit(1);
        }
    }

    set_default_client_residency_requirement(config.enforce_residency.value());

    if let Err(err) = enforce_login_restrictions(&config) {
        eprintln!("{err}");
        std::process::exit(1);
    }

    let otel = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        codex_core::otel_init::build_provider(
            &config,
            env!("CARGO_PKG_VERSION"),
            None,
            DEFAULT_ANALYTICS_ENABLED,
        )
    })) {
        Ok(Ok(otel)) => otel,
        Ok(Err(e)) => {
            eprintln!("Could not create otel exporter: {e}");
            None
        }
        Err(_) => {
            eprintln!("Could not create otel exporter: panicked during initialization");
            None
        }
    };

    let otel_logger_layer = otel.as_ref().and_then(|o| o.logger_layer());

    let otel_tracing_layer = otel.as_ref().and_then(|o| o.tracing_layer());

    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(otel_tracing_layer)
        .with(otel_logger_layer)
        .try_init();

    let exec_span = exec_root_span();
    if let Some(context) = traceparent_context_from_env() {
        set_parent_from_context(&exec_span, context);
    }
    run_exec_session(ExecRunArgs {
        command,
        config,
        cursor_ansi,
        dangerously_bypass_approvals_and_sandbox,
        exec_span: exec_span.clone(),
        images,
        json_mode,
        last_message_file,
        model_provider,
        oss,
        output_schema_path,
        prompt,
        skip_git_repo_check,
        self_directed_innovation,
        stderr_with_ansi,
    })
    .instrument(exec_span)
    .await
}

async fn run_exec_session(args: ExecRunArgs) -> anyhow::Result<()> {
    let ExecRunArgs {
        command,
        config,
        cursor_ansi,
        dangerously_bypass_approvals_and_sandbox,
        exec_span,
        images,
        json_mode,
        last_message_file,
        model_provider,
        oss,
        output_schema_path,
        prompt,
        skip_git_repo_check,
        self_directed_innovation,
        stderr_with_ansi,
    } = args;

    let mut event_processor: Box<dyn EventProcessor> = match json_mode {
        true => Box::new(EventProcessorWithJsonOutput::new(last_message_file.clone())),
        _ => Box::new(EventProcessorWithHumanOutput::create_with_ansi(
            stderr_with_ansi,
            cursor_ansi,
            &config,
            last_message_file.clone(),
        )),
    };
    let required_mcp_servers: HashSet<String> = config
        .mcp_servers
        .get()
        .iter()
        .filter(|(_, server)| server.enabled && server.required)
        .map(|(name, _)| name.clone())
        .collect();

    if oss {
        // We're in the oss section, so provider_id should be Some
        // Let's handle None case gracefully though just in case
        let provider_id = match model_provider.as_ref() {
            Some(id) => id,
            None => {
                error!("OSS provider unexpectedly not set when oss flag is used");
                return Err(anyhow::anyhow!(
                    "OSS provider not set but oss flag was used"
                ));
            }
        };
        ensure_oss_provider_ready(provider_id, &config)
            .await
            .map_err(|e| anyhow::anyhow!("OSS setup failed: {e}"))?;
    }

    let default_cwd = config.cwd.to_path_buf();
    let default_approval_policy = config.permissions.approval_policy.value();
    let default_sandbox_policy = config.permissions.sandbox_policy.get();
    let default_effort = config.model_reasoning_effort;

    // When --yolo (dangerously_bypass_approvals_and_sandbox) is set, also skip the git repo check
    // since the user is explicitly running in an externally sandboxed environment.
    if !skip_git_repo_check
        && !dangerously_bypass_approvals_and_sandbox
        && get_git_repo_root(&default_cwd).is_none()
    {
        eprintln!("Not inside a trusted directory and --skip-git-repo-check was not specified.");
        std::process::exit(1);
    }

    let auth_manager = AuthManager::shared(
        config.codex_home.clone(),
        true,
        config.cli_auth_credentials_store_mode,
    );
    let thread_manager = Arc::new(ThreadManager::new(
        config.codex_home.clone(),
        auth_manager.clone(),
        SessionSource::Exec,
        config.model_catalog.clone(),
        CollaborationModesConfig {
            default_mode_request_user_input: config
                .features
                .enabled(codex_core::features::Feature::DefaultModeRequestUserInput),
        },
    ));
    let default_model = thread_manager
        .get_models_manager()
        .get_default_model(&config.model, RefreshStrategy::OnlineIfUncached)
        .await;
    let output_schema = load_output_schema(output_schema_path.clone());
    let non_stop_developer_instructions = if self_directed_innovation {
        Some(
            "Bounded self-directed innovation is enabled for this Non-stop run. You may only use it when it clearly stays within the active goal, fits the remaining time budget, and does not create obvious high-risk side effects. Before executing any self-directed innovation, first record it with <innovation_candidate>{\"title\":\"...\",\"rationale\":\"...\",\"relevance\":\"...\",\"risk\":\"low|medium|high\",\"estimated_duration\":\"30m\"}</innovation_candidate> and then hand off with <task_complete>...</task_complete>."
                .to_string(),
        )
    } else {
        Some(
            "Self-directed innovation is disabled for this run unless the user explicitly enables it with the matching CLI flag. Keep working only on tasks that are directly requested or clearly implied by the active goal."
                .to_string(),
        )
    };
    let requested_collaboration_mode = (config.initial_collaboration_mode == ModeKind::NonStop)
        .then_some(CollaborationMode {
            mode: ModeKind::NonStop,
            settings: Settings {
                model: default_model.clone(),
                reasoning_effort: default_effort,
                developer_instructions: non_stop_developer_instructions,
            },
        });
    let turn_submit_defaults = TurnSubmitDefaults {
        cwd: default_cwd.clone(),
        approval_policy: default_approval_policy,
        sandbox_policy: default_sandbox_policy.clone(),
        model: default_model.clone(),
        effort: default_effort,
        collaboration_mode: requested_collaboration_mode.clone(),
        output_schema: output_schema.clone(),
    };

    // Handle resume subcommand by resolving a rollout path and using explicit resume API.
    let NewThread {
        thread_id: primary_thread_id,
        thread,
        session_configured,
    } = if let Some(ExecCommand::Resume(args)) = command.as_ref() {
        let resume_path = resolve_resume_path(&config, args).await?;

        if let Some(path) = resume_path {
            thread_manager
                .resume_thread_from_rollout(config.clone(), path, auth_manager.clone())
                .await?
        } else {
            thread_manager.start_thread(config.clone()).await?
        }
    } else {
        thread_manager.start_thread(config.clone()).await?
    };
    if matches!(command.as_ref(), Some(ExecCommand::Resume(_))) {
        thread.discard_startup_regular_task().await;
    }
    let primary_thread_id_for_span = primary_thread_id.to_string();
    exec_span.record("thread.id", primary_thread_id_for_span.as_str());

    let (
        initial_operation,
        prompt_summary,
        non_stop_goal_prompt,
        reset_non_stop_budget_from_user_input,
    ) = match (command, prompt, images) {
        (Some(ExecCommand::Review(review_cli)), _, _) => {
            let review_request = build_review_request(review_cli)?;
            let summary = codex_core::review_prompts::user_facing_hint(&review_request.target);
            (
                InitialOperation::Review { review_request },
                summary,
                None,
                false,
            )
        }
        (Some(ExecCommand::Resume(args)), root_prompt, imgs) => {
            let prompt_arg = args
                .prompt
                .clone()
                .or_else(|| {
                    if args.last {
                        args.session_id.clone()
                    } else {
                        None
                    }
                })
                .or(root_prompt);
            let checkpoint = if config.initial_collaboration_mode == ModeKind::NonStop {
                let thread_checkpoint =
                    read_non_stop_checkpoint(config.codex_home.as_path(), primary_thread_id).await;
                match thread_checkpoint {
                    Some(checkpoint) => Some(checkpoint),
                    None => read_latest_non_stop_checkpoint(config.codex_home.as_path()),
                }
            } else {
                None
            };
            let reset_budget_from_user_input = prompt_arg.is_some();
            let (prompt_text, goal_prompt) = match prompt_arg {
                Some(prompt) => {
                    let prompt_text = resolve_prompt(Some(prompt));
                    (prompt_text.clone(), Some(prompt_text))
                }
                None if config.initial_collaboration_mode == ModeKind::NonStop => (
                    checkpoint
                        .as_ref()
                        .map(build_non_stop_supervisor_prompt)
                        .unwrap_or_else(|| DEFAULT_NON_STOP_RESUME_PROMPT.to_string()),
                    checkpoint
                        .and_then(|checkpoint| checkpoint.goal_prompt)
                        .or_else(|| Some(DEFAULT_NON_STOP_RESUME_PROMPT.to_string())),
                ),
                None => {
                    let prompt_text = resolve_prompt(None);
                    (prompt_text.clone(), Some(prompt_text))
                }
            };
            let mut items: Vec<UserInput> = imgs
                .into_iter()
                .chain(args.images.into_iter())
                .map(|path| UserInput::LocalImage { path })
                .collect();
            items.push(UserInput::Text {
                text: prompt_text.clone(),
                // CLI input doesn't track UI element ranges, so none are available here.
                text_elements: Vec::new(),
            });
            (
                InitialOperation::UserTurn {
                    items,
                    output_schema: output_schema.clone(),
                },
                prompt_text,
                goal_prompt,
                reset_budget_from_user_input,
            )
        }
        (None, root_prompt, imgs) => {
            let prompt_text = resolve_prompt(root_prompt);
            let mut items: Vec<UserInput> = imgs
                .into_iter()
                .map(|path| UserInput::LocalImage { path })
                .collect();
            items.push(UserInput::Text {
                text: prompt_text.clone(),
                // CLI input doesn't track UI element ranges, so none are available here.
                text_elements: Vec::new(),
            });
            (
                InitialOperation::UserTurn {
                    items,
                    output_schema: output_schema.clone(),
                },
                prompt_text.clone(),
                Some(prompt_text),
                true,
            )
        }
    };

    // Print the effective configuration and initial request so users can see what Codex
    // is using.
    event_processor.print_config_summary(&config, &prompt_summary, &session_configured);
    if config.initial_collaboration_mode == ModeKind::NonStop {
        register_non_stop_session(
            config.codex_home.as_path(),
            session_configured.session_id,
            session_configured.model.as_str(),
            session_configured.cwd.as_path(),
            SessionSource::Exec,
            RegisterNonStopSessionOptions {
                goal_prompt: non_stop_goal_prompt,
                reset_budget_from_user_input: reset_non_stop_budget_from_user_input,
                self_directed_innovation_enabled: self_directed_innovation,
            },
        )
        .await;
    }

    info!("Codex initialized with event: {session_configured:?}");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ThreadEventEnvelope>();
    let attached_threads = Arc::new(Mutex::new(HashSet::from([primary_thread_id])));
    spawn_thread_listener(primary_thread_id, thread.clone(), tx.clone(), false);

    {
        let thread = thread.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::debug!("Keyboard interrupt");
                // Immediately notify Codex to abort any in-flight task.
                thread.submit(Op::Interrupt).await.ok();
            }
        });
    }

    {
        let thread_manager = Arc::clone(&thread_manager);
        let attached_threads = Arc::clone(&attached_threads);
        let tx = tx.clone();
        let mut thread_created_rx = thread_manager.subscribe_thread_created();
        tokio::spawn(async move {
            loop {
                match thread_created_rx.recv().await {
                    Ok(thread_id) => {
                        if attached_threads.lock().await.contains(&thread_id) {
                            continue;
                        }
                        match thread_manager.get_thread(thread_id).await {
                            Ok(thread) => {
                                attached_threads.lock().await.insert(thread_id);
                                let suppress_output =
                                    is_agent_job_subagent(&thread.config_snapshot().await);
                                spawn_thread_listener(
                                    thread_id,
                                    thread,
                                    tx.clone(),
                                    suppress_output,
                                );
                            }
                            Err(err) => {
                                warn!("failed to attach listener for thread {thread_id}: {err}")
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        warn!("thread_created receiver lagged; skipping resync");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    let task_id = match initial_operation {
        InitialOperation::UserTurn {
            items,
            output_schema,
        } => {
            let task_id = thread
                .submit(Op::UserTurn {
                    items,
                    cwd: default_cwd.clone(),
                    approval_policy: default_approval_policy,
                    sandbox_policy: default_sandbox_policy.clone(),
                    model: default_model.clone(),
                    effort: default_effort,
                    summary: None,
                    service_tier: None,
                    final_output_json_schema: output_schema,
                    collaboration_mode: requested_collaboration_mode.clone(),
                    personality: None,
                })
                .await?;
            info!("Sent prompt with event ID: {task_id}");
            task_id
        }
        InitialOperation::Review { review_request } => {
            let task_id = thread.submit(Op::Review { review_request }).await?;
            info!("Sent review request with event ID: {task_id}");
            task_id
        }
    };
    exec_span.record("turn.id", task_id.as_str());
    let mut current_live_turn_id: Option<String> = None;

    // Run the loop until the task is complete.
    // Track whether a fatal error was reported by the server so we can
    // exit with a non-zero status for automation-friendly signaling.
    let mut error_seen = false;
    let mut shutdown_requested = false;
    let mut last_non_stop_follow_up_turn_id: Option<String> = None;
    while let Some(envelope) = rx.recv().await {
        let ThreadEventEnvelope {
            thread_id,
            thread,
            event,
            suppress_output,
        } = envelope;
        if suppress_output && should_suppress_agent_job_event(&event.msg) {
            continue;
        }
        if matches!(event.msg, EventMsg::Error(_)) {
            error_seen = true;
        }
        if shutdown_requested
            && !matches!(&event.msg, EventMsg::ShutdownComplete | EventMsg::Error(_))
        {
            continue;
        }
        if let EventMsg::ElicitationRequest(ev) = &event.msg {
            // Automatically cancel elicitation requests in exec mode.
            thread
                .submit(Op::ResolveElicitation {
                    server_name: ev.server_name.clone(),
                    request_id: ev.id.clone(),
                    decision: ElicitationAction::Cancel,
                    content: None,
                })
                .await?;
        }
        if let EventMsg::McpStartupUpdate(update) = &event.msg
            && required_mcp_servers.contains(&update.server)
            && let codex_protocol::protocol::McpStartupStatus::Failed { error } = &update.status
        {
            error_seen = true;
            eprintln!(
                "Required MCP server '{}' failed to initialize: {error}",
                update.server
            );
            if !shutdown_requested {
                thread.submit(Op::Shutdown).await?;
                shutdown_requested = true;
            }
        }
        if thread_id != primary_thread_id && matches!(&event.msg, EventMsg::TurnComplete(_)) {
            continue;
        }
        if thread_id == primary_thread_id
            && let EventMsg::TurnStarted(event) = &event.msg
        {
            current_live_turn_id = Some(event.turn_id.clone());
        }
        let (current_primary_turn_complete, current_primary_completed_turn_id) =
            if thread_id == primary_thread_id {
                match &event.msg {
                    EventMsg::TurnComplete(event)
                        if current_live_turn_id.as_deref() == Some(event.turn_id.as_str()) =>
                    {
                        (
                            Some(event.completion_reason.clone()),
                            Some(event.turn_id.clone()),
                        )
                    }
                    _ => (None, None),
                }
            } else {
                (None, None)
            };
        if thread_id == primary_thread_id
            && let EventMsg::TurnComplete(event) = &event.msg
            && current_live_turn_id.as_deref() != Some(event.turn_id.as_str())
        {
            continue;
        }
        let shutdown = event_processor.process_event(event);
        if thread_id != primary_thread_id && matches!(shutdown, CodexStatus::InitiateShutdown) {
            continue;
        }
        match shutdown {
            CodexStatus::Running => continue,
            CodexStatus::InitiateShutdown => {
                if should_start_non_stop_follow_up_turn(
                    config.initial_collaboration_mode,
                    current_primary_turn_complete.as_ref(),
                    current_primary_completed_turn_id.as_deref(),
                    last_non_stop_follow_up_turn_id.as_deref(),
                ) {
                    if let Some(_task_id) = maybe_submit_non_stop_follow_up_turn(
                        &config,
                        primary_thread_id,
                        &thread,
                        &turn_submit_defaults,
                        &exec_span,
                    )
                    .await?
                    {
                        last_non_stop_follow_up_turn_id = current_primary_completed_turn_id;
                        current_live_turn_id = None;
                        continue;
                    }
                }
                current_live_turn_id = None;
                if !shutdown_requested {
                    thread.submit(Op::Shutdown).await?;
                    shutdown_requested = true;
                }
            }
            CodexStatus::Shutdown if thread_id == primary_thread_id => break,
            CodexStatus::Shutdown => continue,
        }
    }
    event_processor.print_final_output();
    if error_seen {
        std::process::exit(1);
    }

    Ok(())
}

fn spawn_thread_listener(
    thread_id: codex_protocol::ThreadId,
    thread: Arc<codex_core::CodexThread>,
    tx: tokio::sync::mpsc::UnboundedSender<ThreadEventEnvelope>,
    suppress_output: bool,
) {
    tokio::spawn(async move {
        loop {
            match thread.next_event().await {
                Ok(event) => {
                    debug!("Received event: {event:?}");

                    let is_shutdown_complete = matches!(event.msg, EventMsg::ShutdownComplete);
                    if let Err(err) = tx.send(ThreadEventEnvelope {
                        thread_id,
                        thread: Arc::clone(&thread),
                        event,
                        suppress_output,
                    }) {
                        error!("Error sending event: {err:?}");
                        break;
                    }
                    if is_shutdown_complete {
                        info!(
                            "Received shutdown event for thread {thread_id}, exiting event loop."
                        );
                        break;
                    }
                }
                Err(err) => {
                    error!("Error receiving event: {err:?}");
                    break;
                }
            }
        }
    });
}

fn is_agent_job_subagent(config: &codex_core::ThreadConfigSnapshot) -> bool {
    match &config.session_source {
        SessionSource::SubAgent(SubAgentSource::Other(source)) => source.starts_with("agent_job:"),
        _ => false,
    }
}

fn should_suppress_agent_job_event(msg: &EventMsg) -> bool {
    !matches!(
        msg,
        EventMsg::ExecApprovalRequest(_)
            | EventMsg::ApplyPatchApprovalRequest(_)
            | EventMsg::RequestUserInput(_)
            | EventMsg::DynamicToolCallRequest(_)
            | EventMsg::DynamicToolCallResponse(_)
            | EventMsg::ElicitationRequest(_)
            | EventMsg::Error(_)
            | EventMsg::Warning(_)
            | EventMsg::DeprecationNotice(_)
            | EventMsg::StreamError(_)
            | EventMsg::ShutdownComplete
    )
}

async fn resolve_resume_path(
    config: &Config,
    args: &crate::cli::ResumeArgs,
) -> anyhow::Result<Option<PathBuf>> {
    if args.last {
        let default_provider_filter = vec![config.model_provider_id.clone()];
        let filter_cwd = if args.all {
            None
        } else {
            Some(config.cwd.as_path())
        };
        match codex_core::RolloutRecorder::find_latest_thread_path(
            config,
            1,
            None,
            codex_core::ThreadSortKey::UpdatedAt,
            &[],
            Some(default_provider_filter.as_slice()),
            &config.model_provider_id,
            filter_cwd,
        )
        .await
        {
            Ok(path) => Ok(path),
            Err(e) => {
                error!("Error listing threads: {e}");
                Ok(None)
            }
        }
    } else if let Some(id_str) = args.session_id.as_deref() {
        if Uuid::parse_str(id_str).is_ok() {
            let path = find_thread_path_by_id_str(&config.codex_home, id_str).await?;
            Ok(path)
        } else {
            let path = find_thread_path_by_name_str(&config.codex_home, id_str).await?;
            Ok(path)
        }
    } else {
        Ok(None)
    }
}

fn build_non_stop_supervisor_prompt(checkpoint: &NonStopCheckpoint) -> String {
    let goal = checkpoint.goal_prompt.as_deref().unwrap_or(
        "Continue making progress on the current non-stop goal from the existing thread state.",
    );
    let mut prompt = format!(
        "Resume Non-stop execution toward the active goal.\nGoal: {goal}\nReview the existing thread state, avoid redoing finished work, and immediately choose the next highest-leverage concrete task. Treat the goal as still incomplete unless you can now verify that the user's requested outcome is actually achieved."
    );
    if let Some(budget_summary) = non_stop_budget_summary(checkpoint) {
        prompt.push_str(&format!("\nTime budget: {budget_summary}"));
    }
    match checkpoint.status {
        NonStopCheckpointStatus::TurnComplete => {}
        NonStopCheckpointStatus::TurnAborted | NonStopCheckpointStatus::Error => {
            prompt.push_str(
                "\nThe last turn ended early. Recover from that interruption and continue.",
            );
        }
        NonStopCheckpointStatus::Pending
        | NonStopCheckpointStatus::Running
        | NonStopCheckpointStatus::Shutdown => {
            prompt.push_str(
                "\nThe previous run did not finish cleanly. Reconstruct state from the thread and keep going.",
            );
        }
    }
    if let Some(last_agent_message) = checkpoint
        .last_agent_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        prompt.push_str(&format!(
            "\nMost recent agent summary: {last_agent_message}"
        ));
    }
    if checkpoint.self_directed_innovation_enabled
        && let Some(innovation_summary) = non_stop_pending_innovation_summary(checkpoint)
    {
        prompt.push_str(&format!(
            "\nPending innovation backlog:\n{innovation_summary}"
        ));
    }
    if checkpoint.self_directed_innovation_enabled && non_stop_has_blocked_innovation(checkpoint) {
        prompt.push_str(
            "\nAt least one pending innovation candidate is currently too risky or too large for the remaining budget. Do not execute those items unchanged; either reject them explicitly in your reasoning and choose a safer in-scope action, or record a smaller safer innovation candidate first.",
        );
    }
    if checkpoint.last_assistant_control_signal
        == Some(NonStopCheckpointControlSignal::TaskComplete)
    {
        prompt.push_str(
            "\nThe previous turn ended with <task_complete>, which in Non-stop means the turn finished a concrete step but the overall goal still remains active. Keep monitoring or advancing the goal until it is actually satisfied.",
        );
    }
    if checkpoint.self_directed_innovation_enabled {
        prompt.push_str(
            "\nSelf-directed innovation is allowed only when it remains clearly inside the active goal, fits the remaining time budget, and has no obvious high-risk side effects. Before executing self-directed innovation, first record it with <innovation_candidate>{\"title\":\"...\",\"rationale\":\"...\",\"relevance\":\"...\",\"risk\":\"low|medium|high\",\"estimated_duration\":\"30m\"}</innovation_candidate> and then hand off with <task_complete>...</task_complete> so the next Non-stop turn can review it.",
        );
    }
    prompt.push_str(
        "\nOnly stop if you are blocked on information the user must provide and include <await_user_input>...</await_user_input>, or if the active goal is truly achieved and you include <goal_complete>...</goal_complete>.",
    );
    prompt
}

fn should_start_non_stop_follow_up_turn(
    collaboration_mode: ModeKind,
    completion_reason: Option<&TurnCompleteReason>,
    completed_turn_id: Option<&str>,
    last_followed_up_turn_id: Option<&str>,
) -> bool {
    collaboration_mode == ModeKind::NonStop
        && completion_reason == Some(&TurnCompleteReason::Completed)
        && completed_turn_id.is_some()
        && completed_turn_id != last_followed_up_turn_id
}

fn non_stop_budget_summary(checkpoint: &NonStopCheckpoint) -> Option<String> {
    let budget = checkpoint.budget_window.as_ref()?;
    let remaining_secs = budget.deadline_at.saturating_sub(current_unix_timestamp());
    let total = format_non_stop_duration(budget.duration_secs);
    let remaining = format_non_stop_duration(remaining_secs.max(0));
    let source = match budget.source {
        NonStopBudgetSource::Default48Hours => "default 48-hour budget",
        NonStopBudgetSource::UserPrompt => "user-provided budget",
    };
    Some(format!(
        "{remaining} remaining out of {total} ({source}); the timer resets only when the user sends new input."
    ))
}

fn non_stop_pending_innovation_summary(checkpoint: &NonStopCheckpoint) -> Option<String> {
    let mut lines = checkpoint
        .innovation_backlog
        .iter()
        .filter(|candidate| candidate.status == NonStopInnovationStatus::Proposed)
        .map(|candidate| {
            let estimate = candidate
                .estimated_duration_secs
                .map(format_non_stop_duration)
                .unwrap_or_else(|| "unknown duration".to_string());
            let rationale = candidate
                .rationale
                .as_deref()
                .filter(|text| !text.is_empty())
                .unwrap_or("no rationale recorded");
            format!(
                "- {} (risk: {}, estimate: {}, rationale: {})",
                candidate.title,
                non_stop_risk_label(candidate.risk),
                estimate,
                rationale
            )
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        None
    } else {
        Some(lines.drain(..).collect::<Vec<_>>().join("\n"))
    }
}

fn non_stop_risk_label(risk: NonStopInnovationRisk) -> &'static str {
    match risk {
        NonStopInnovationRisk::Unknown => "unknown",
        NonStopInnovationRisk::Low => "low",
        NonStopInnovationRisk::Medium => "medium",
        NonStopInnovationRisk::High => "high",
        NonStopInnovationRisk::Critical => "critical",
    }
}

fn format_non_stop_duration(seconds: i64) -> String {
    if seconds >= 24 * 60 * 60 {
        format!("{}h", seconds / (60 * 60))
    } else if seconds >= 60 * 60 {
        let hours = seconds / (60 * 60);
        let minutes = (seconds % (60 * 60)) / 60;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        }
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn non_stop_budget_is_exhausted(checkpoint: &NonStopCheckpoint) -> bool {
    checkpoint
        .budget_window
        .as_ref()
        .is_some_and(|budget| current_unix_timestamp() >= budget.deadline_at)
}

fn non_stop_has_blocked_innovation(checkpoint: &NonStopCheckpoint) -> bool {
    if !checkpoint.self_directed_innovation_enabled {
        return false;
    }
    checkpoint.innovation_backlog.iter().any(|candidate| {
        candidate.status == NonStopInnovationStatus::Proposed
            && (matches!(
                candidate.risk,
                NonStopInnovationRisk::High | NonStopInnovationRisk::Critical
            ) || candidate
                .estimated_duration_secs
                .zip(
                    checkpoint
                        .budget_window
                        .as_ref()
                        .map(|budget| budget.deadline_at.saturating_sub(current_unix_timestamp())),
                )
                .is_some_and(|(estimate, remaining)| estimate > remaining.max(0)))
    })
}

fn read_latest_non_stop_checkpoint(codex_home: &std::path::Path) -> Option<NonStopCheckpoint> {
    let checkpoint_dir = codex_home.join("non-stop-checkpoints");
    let entries = std::fs::read_dir(checkpoint_dir).ok()?;
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return None;
            }
            let payload = std::fs::read(&path).ok()?;
            serde_json::from_slice::<NonStopCheckpoint>(&payload).ok()
        })
        .max_by_key(|checkpoint| checkpoint.updated_at)
}

async fn maybe_submit_non_stop_follow_up_turn(
    config: &Config,
    thread_id: codex_protocol::ThreadId,
    thread: &Arc<codex_core::CodexThread>,
    defaults: &TurnSubmitDefaults,
    exec_span: &tracing::Span,
) -> anyhow::Result<Option<String>> {
    let Some(checkpoint) = read_non_stop_checkpoint(config.codex_home.as_path(), thread_id).await
    else {
        return Ok(None);
    };
    if checkpoint.status != NonStopCheckpointStatus::TurnComplete {
        return Ok(None);
    }
    let has_assistant_signal = checkpoint.last_assistant_control_signal.is_some()
        || checkpoint
            .last_agent_message
            .as_deref()
            .is_some_and(|message| !message.trim().is_empty());
    if !has_assistant_signal {
        warn!(
            thread_id = %thread_id,
            "skipping non-stop follow-up because the completed turn produced no assistant output"
        );
        return Ok(None);
    }
    if non_stop_budget_is_exhausted(&checkpoint) {
        info!(
            thread_id = %thread_id,
            "non-stop follow-up stopped because the active time budget is exhausted"
        );
        return Ok(None);
    }
    if non_stop_has_blocked_innovation(&checkpoint) {
        info!(
            thread_id = %thread_id,
            "non-stop follow-up found a blocked innovation candidate; the next prompt will steer away from it"
        );
    }

    let prompt = build_non_stop_supervisor_prompt(&checkpoint);
    let task_id = thread
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: prompt,
                text_elements: Vec::new(),
            }],
            cwd: defaults.cwd.clone(),
            approval_policy: defaults.approval_policy,
            sandbox_policy: defaults.sandbox_policy.clone(),
            model: defaults.model.clone(),
            effort: defaults.effort,
            summary: None,
            service_tier: None,
            final_output_json_schema: defaults.output_schema.clone(),
            collaboration_mode: defaults.collaboration_mode.clone(),
            personality: None,
        })
        .await?;
    exec_span.record("turn.id", task_id.as_str());
    info!("Started non-stop follow-up turn with event ID: {task_id}");
    Ok(Some(task_id))
}

fn load_output_schema(path: Option<PathBuf>) -> Option<Value> {
    let path = path?;

    let schema_str = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            eprintln!(
                "Failed to read output schema file {}: {err}",
                path.display()
            );
            std::process::exit(1);
        }
    };

    match serde_json::from_str::<Value>(&schema_str) {
        Ok(value) => Some(value),
        Err(err) => {
            eprintln!(
                "Output schema file {} is not valid JSON: {err}",
                path.display()
            );
            std::process::exit(1);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptDecodeError {
    InvalidUtf8 { valid_up_to: usize },
    InvalidUtf16 { encoding: &'static str },
    UnsupportedBom { encoding: &'static str },
}

impl std::fmt::Display for PromptDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptDecodeError::InvalidUtf8 { valid_up_to } => write!(
                f,
                "input is not valid UTF-8 (invalid byte at offset {valid_up_to}). Convert it to UTF-8 and retry (e.g., `iconv -f <ENC> -t UTF-8 prompt.txt`)."
            ),
            PromptDecodeError::InvalidUtf16 { encoding } => write!(
                f,
                "input looked like {encoding} but could not be decoded. Convert it to UTF-8 and retry."
            ),
            PromptDecodeError::UnsupportedBom { encoding } => write!(
                f,
                "input appears to be {encoding}. Convert it to UTF-8 and retry."
            ),
        }
    }
}

fn decode_prompt_bytes(input: &[u8]) -> Result<String, PromptDecodeError> {
    let input = input.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(input);

    if input.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return Err(PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32LE",
        });
    }

    if input.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return Err(PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32BE",
        });
    }

    if let Some(rest) = input.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, "UTF-16LE", u16::from_le_bytes);
    }

    if let Some(rest) = input.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, "UTF-16BE", u16::from_be_bytes);
    }

    std::str::from_utf8(input)
        .map(str::to_string)
        .map_err(|e| PromptDecodeError::InvalidUtf8 {
            valid_up_to: e.valid_up_to(),
        })
}

fn decode_utf16(
    input: &[u8],
    encoding: &'static str,
    decode_unit: fn([u8; 2]) -> u16,
) -> Result<String, PromptDecodeError> {
    if !input.len().is_multiple_of(2) {
        return Err(PromptDecodeError::InvalidUtf16 { encoding });
    }

    let units: Vec<u16> = input
        .chunks_exact(2)
        .map(|chunk| decode_unit([chunk[0], chunk[1]]))
        .collect();

    String::from_utf16(&units).map_err(|_| PromptDecodeError::InvalidUtf16 { encoding })
}

fn resolve_prompt(prompt_arg: Option<String>) -> String {
    match prompt_arg {
        Some(p) if p != "-" => p,
        maybe_dash => {
            let force_stdin = matches!(maybe_dash.as_deref(), Some("-"));

            if std::io::stdin().is_terminal() && !force_stdin {
                eprintln!(
                    "No prompt provided. Either specify one as an argument or pipe the prompt into stdin."
                );
                std::process::exit(1);
            }

            if !force_stdin {
                eprintln!("Reading prompt from stdin...");
            }

            let mut bytes = Vec::new();
            if let Err(e) = std::io::stdin().read_to_end(&mut bytes) {
                eprintln!("Failed to read prompt from stdin: {e}");
                std::process::exit(1);
            }

            let buffer = match decode_prompt_bytes(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to read prompt from stdin: {e}");
                    std::process::exit(1);
                }
            };

            if buffer.trim().is_empty() {
                eprintln!("No prompt provided via stdin.");
                std::process::exit(1);
            }
            buffer
        }
    }
}

fn build_review_request(args: ReviewArgs) -> anyhow::Result<ReviewRequest> {
    let target = if args.uncommitted {
        ReviewTarget::UncommittedChanges
    } else if let Some(branch) = args.base {
        ReviewTarget::BaseBranch { branch }
    } else if let Some(sha) = args.commit {
        ReviewTarget::Commit {
            sha,
            title: args.commit_title,
        }
    } else if let Some(prompt_arg) = args.prompt {
        let prompt = resolve_prompt(Some(prompt_arg)).trim().to_string();
        if prompt.is_empty() {
            anyhow::bail!("Review prompt cannot be empty");
        }
        ReviewTarget::Custom {
            instructions: prompt,
        }
    } else {
        anyhow::bail!(
            "Specify --uncommitted, --base, --commit, or provide custom review instructions"
        );
    };

    Ok(ReviewRequest {
        target,
        user_facing_hint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_core::NonStopCheckpointControlSignal;
    use codex_otel::set_parent_from_w3c_trace_context;
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::SessionSource;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    fn test_tracing_subscriber() -> impl tracing::Subscriber + Send + Sync {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("codex-exec-tests");
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer))
    }

    #[test]
    fn exec_defaults_analytics_to_enabled() {
        assert_eq!(DEFAULT_ANALYTICS_ENABLED, true);
    }

    #[test]
    fn exec_root_span_can_be_parented_from_trace_context() {
        let subscriber = test_tracing_subscriber();
        let _guard = tracing::subscriber::set_default(subscriber);

        let parent = codex_protocol::protocol::W3cTraceContext {
            traceparent: Some("00-00000000000000000000000000000077-0000000000000088-01".into()),
            tracestate: Some("vendor=value".into()),
        };
        let exec_span = exec_root_span();
        assert!(set_parent_from_w3c_trace_context(&exec_span, &parent));

        let trace_id = exec_span.context().span().span_context().trace_id();
        assert_eq!(
            trace_id,
            TraceId::from_hex("00000000000000000000000000000077").expect("trace id")
        );
    }

    #[test]
    fn builds_uncommitted_review_request() {
        let request = build_review_request(ReviewArgs {
            uncommitted: true,
            base: None,
            commit: None,
            commit_title: None,
            prompt: None,
        })
        .expect("builds uncommitted review request");

        let expected = ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn builds_commit_review_request_with_title() {
        let request = build_review_request(ReviewArgs {
            uncommitted: false,
            base: None,
            commit: Some("123456789".to_string()),
            commit_title: Some("Add review command".to_string()),
            prompt: None,
        })
        .expect("builds commit review request");

        let expected = ReviewRequest {
            target: ReviewTarget::Commit {
                sha: "123456789".to_string(),
                title: Some("Add review command".to_string()),
            },
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn builds_custom_review_request_trims_prompt() {
        let request = build_review_request(ReviewArgs {
            uncommitted: false,
            base: None,
            commit: None,
            commit_title: None,
            prompt: Some("  custom review instructions  ".to_string()),
        })
        .expect("builds custom review request");

        let expected = ReviewRequest {
            target: ReviewTarget::Custom {
                instructions: "custom review instructions".to_string(),
            },
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn decode_prompt_bytes_strips_utf8_bom() {
        let input = [0xEF, 0xBB, 0xBF, b'h', b'i', b'\n'];

        let out = decode_prompt_bytes(&input).expect("decode utf-8 with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_decodes_utf16le_bom() {
        // UTF-16LE BOM + "hi\n"
        let input = [0xFF, 0xFE, b'h', 0x00, b'i', 0x00, b'\n', 0x00];

        let out = decode_prompt_bytes(&input).expect("decode utf-16le with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_decodes_utf16be_bom() {
        // UTF-16BE BOM + "hi\n"
        let input = [0xFE, 0xFF, 0x00, b'h', 0x00, b'i', 0x00, b'\n'];

        let out = decode_prompt_bytes(&input).expect("decode utf-16be with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_rejects_utf32le_bom() {
        // UTF-32LE BOM + "hi\n"
        let input = [
            0xFF, 0xFE, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00, 0x00, b'\n', 0x00,
            0x00, 0x00,
        ];

        let err = decode_prompt_bytes(&input).expect_err("utf-32le should be rejected");

        assert_eq!(
            err,
            PromptDecodeError::UnsupportedBom {
                encoding: "UTF-32LE"
            }
        );
    }

    #[test]
    fn decode_prompt_bytes_rejects_utf32be_bom() {
        // UTF-32BE BOM + "hi\n"
        let input = [
            0x00, 0x00, 0xFE, 0xFF, 0x00, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00,
            0x00, b'\n',
        ];

        let err = decode_prompt_bytes(&input).expect_err("utf-32be should be rejected");

        assert_eq!(
            err,
            PromptDecodeError::UnsupportedBom {
                encoding: "UTF-32BE"
            }
        );
    }

    #[test]
    fn decode_prompt_bytes_rejects_invalid_utf8() {
        // Invalid UTF-8 sequence: 0xC3 0x28
        let input = [0xC3, 0x28];

        let err = decode_prompt_bytes(&input).expect_err("invalid utf-8 should fail");

        assert_eq!(err, PromptDecodeError::InvalidUtf8 { valid_up_to: 0 });
    }

    fn test_checkpoint(thread_id: &str, updated_at: i64, goal_prompt: &str) -> NonStopCheckpoint {
        NonStopCheckpoint {
            thread_id: ThreadId::from_string(thread_id).expect("thread id"),
            turn_id: Some("turn-1".to_string()),
            status: NonStopCheckpointStatus::TurnComplete,
            collaboration_mode: ModeKind::NonStop,
            model: "gpt-5.4".to_string(),
            cwd: PathBuf::from("/tmp"),
            session_source: SessionSource::Exec,
            created_at: updated_at - 10,
            updated_at,
            goal_prompt: Some(goal_prompt.to_string()),
            last_agent_message: Some("done".to_string()),
            last_assistant_control_signal: Some(NonStopCheckpointControlSignal::TaskComplete),
            last_user_input_at: Some(updated_at - 10),
            budget_window: None,
            innovation_backlog: Vec::new(),
            self_directed_innovation_enabled: false,
        }
    }

    #[test]
    fn read_latest_non_stop_checkpoint_prefers_highest_updated_at() {
        let dir = TempDir::new().expect("temp dir");
        let checkpoint_dir = dir.path().join("non-stop-checkpoints");
        std::fs::create_dir_all(&checkpoint_dir).expect("create checkpoint dir");

        let older = test_checkpoint("019cff45-7090-7752-a34a-d93c178b7620", 10, "older goal");
        let newer = test_checkpoint("019cff45-7090-7752-a34a-d93c178b7621", 20, "newer goal");

        std::fs::write(
            checkpoint_dir.join("older.json"),
            serde_json::to_vec(&older).expect("serialize older"),
        )
        .expect("write older");
        std::fs::write(
            checkpoint_dir.join("newer.json"),
            serde_json::to_vec(&newer).expect("serialize newer"),
        )
        .expect("write newer");

        let latest = read_latest_non_stop_checkpoint(dir.path()).expect("latest checkpoint");

        assert_eq!(latest.goal_prompt.as_deref(), Some("newer goal"));
        assert_eq!(latest.updated_at, 20);
    }

    #[test]
    fn read_latest_non_stop_checkpoint_ignores_invalid_json_files() {
        let dir = TempDir::new().expect("temp dir");
        let checkpoint_dir = dir.path().join("non-stop-checkpoints");
        std::fs::create_dir_all(&checkpoint_dir).expect("create checkpoint dir");

        let valid = test_checkpoint("019cff45-7090-7752-a34a-d93c178b7622", 30, "valid goal");
        std::fs::write(checkpoint_dir.join("broken.json"), b"{not json").expect("write broken");
        std::fs::write(
            checkpoint_dir.join("valid.json"),
            serde_json::to_vec(&valid).expect("serialize valid"),
        )
        .expect("write valid");

        let latest = read_latest_non_stop_checkpoint(dir.path()).expect("latest checkpoint");

        assert_eq!(latest.goal_prompt.as_deref(), Some("valid goal"));
    }

    #[test]
    fn non_stop_supervisor_prompt_surfaces_innovation_only_when_enabled() {
        let mut checkpoint = test_checkpoint(
            "019cff45-7090-7752-a34a-d93c178b7623",
            40,
            "watch the rollout for 2 hours",
        );
        checkpoint.self_directed_innovation_enabled = true;
        let now = current_unix_timestamp();
        checkpoint.budget_window = Some(codex_core::NonStopBudgetWindow {
            started_at: now,
            duration_secs: 2 * 60 * 60,
            deadline_at: now + 2 * 60 * 60,
            source: NonStopBudgetSource::UserPrompt,
        });
        checkpoint
            .innovation_backlog
            .push(codex_core::NonStopInnovationTask {
                id: "innovation-1".to_string(),
                title: "Add flaky-test detector".to_string(),
                rationale: Some("Tightens follow-through".to_string()),
                relevance: Some("Same goal".to_string()),
                estimated_duration_secs: Some(20 * 60),
                risk: NonStopInnovationRisk::Medium,
                status: NonStopInnovationStatus::Proposed,
                proposed_at: 40,
                source_turn_id: Some("turn-1".to_string()),
                rejection_reason: None,
            });

        let prompt = build_non_stop_supervisor_prompt(&checkpoint);
        assert!(prompt.contains("Pending innovation backlog"));
        assert!(prompt.contains("remaining out of 2h"));

        checkpoint.self_directed_innovation_enabled = false;
        let prompt_without_flag = build_non_stop_supervisor_prompt(&checkpoint);
        assert!(!prompt_without_flag.contains("Pending innovation backlog"));
        assert!(!prompt_without_flag.contains("Self-directed innovation is allowed"));
    }

    #[test]
    fn non_stop_follow_up_turn_starts_once_per_completed_turn() {
        assert!(should_start_non_stop_follow_up_turn(
            ModeKind::NonStop,
            Some(&TurnCompleteReason::Completed),
            Some("turn-1"),
            None,
        ));
        assert!(!should_start_non_stop_follow_up_turn(
            ModeKind::NonStop,
            Some(&TurnCompleteReason::Completed),
            Some("turn-1"),
            Some("turn-1"),
        ));
        assert!(should_start_non_stop_follow_up_turn(
            ModeKind::NonStop,
            Some(&TurnCompleteReason::Completed),
            Some("turn-2"),
            Some("turn-1"),
        ));
        assert!(!should_start_non_stop_follow_up_turn(
            ModeKind::NonStop,
            Some(&TurnCompleteReason::NoMoreWork),
            Some("turn-2"),
            Some("turn-1"),
        ));
    }
}
