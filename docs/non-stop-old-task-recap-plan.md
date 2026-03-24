# Non-stop 模式旧任务回顾问题治理规划

## 背景

在 `Non-stop` 模式下，用户提交新的明确请求后，系统仍可能在新任务开始阶段回顾上一个任务的尾部，总结旧任务、补充旧任务收尾信息，甚至短暂打断新任务推进。

这个问题不是单点缺陷，而是由以下三层共同造成：

1. **任务切换层**：新输入有时会被视为旧任务 follow-up，有时会被视为新任务。
2. **上下文构造层**：新任务 prompt 中仍可能保留旧任务 assistant 尾部内容。
3. **模型行为层**：即使控制流切换正确，模型看到旧任务尾巴后仍倾向于“顺手总结一下”。

## 已完成治理

### 1. 新任务与旧 turn 解耦

- 显式 `Op::UserTurn` 不再注入旧 active turn。
- 新任务启动时，旧任务会被 `Replaced`，避免新输入被旧 turn 尾声吞掉。

当前代码锚点：

- `codex-rs/core/src/codex.rs`
- `codex-rs/core/src/tasks/mod.rs`

### 2. Non-stop 新任务提示词收紧

- 明确要求“最新用户请求默认是当前主任务”。
- 明确要求不要自动 recap/continue 旧任务。
- 当上一 turn 已 `goal_complete` 时，附加更强限制。

当前代码锚点：

- `codex-rs/core/src/codex.rs`

### 3. 结构性上下文裁剪

- 当上一 turn 已 `goal_complete` / `NoMoreWork`，且用户开启新的显式 `UserTurn` 时：
  - 在下一次真正构造 prompt 前，
  - 一次性裁掉“上一轮 assistant 的模型生成尾部回复段”。
- 目标是减少模型继续旧任务尾巴的上下文诱因，而不是只依赖提示词约束。
- 当前实现已进一步扩展为：
  - 如果新的显式 `UserTurn` 会**替换当前仍在运行的 active turn**，
  - 同样会触发一次性旧回复段裁剪；
  - 不再只限于上一轮自然结束为 `goal_complete` / `NoMoreWork` 的情况。

当前代码锚点：

- `codex-rs/core/src/state/session.rs`
- `codex-rs/core/src/context_manager/history.rs`
- `codex-rs/core/src/codex.rs`

### 4. TUI 尾声 follow-up 改走 `UserInput`

- 当 TUI 处于：
  - `agent_turn_running == true`
  - 且 `is_assistant_streaming_in_tui() == false`
- 说明当前 turn 仍在运行，但已经不再流式输出；
  这是最容易把用户 follow-up 错判成 replacement `UserTurn` 的窗口。
- 当前实现已改为：
  - 在这个窄窗口里，提交改走 `Op::UserInput`
  - 也就是 steer 当前 turn，而不是 replacement `Op::UserTurn`
- 这样可以减少：
  - turn 尾声误替换旧任务
  - 新任务 prompt 再被旧任务尾巴污染

当前代码锚点：

- `codex-rs/tui/src/chatwidget.rs`
- `codex-rs/tui/src/chatwidget/tests.rs`

## 仍然存在的风险

### 1. TUI 分流仍然偏依赖 streaming

当前 TUI 更接近用“是否仍在 streaming”判断是继续旧任务还是启动新任务，而不是用“task 是否仍在 running”判断。

这会在 turn 尾声制造误判窗口：

- streaming 已结束
- 但 task 实际仍在 running
- 用户新输入被过早当成新任务或错误 follow-up

当前代码锚点：

- `codex-rs/tui/src/chatwidget.rs`

补充：

- 经过进一步验证，**不能**粗暴把所有提交路径从 `streaming` 切成 `running`，因为这会打坏既有的：
  - steer / pending_steers
  - queued draft / auto replay
  - plan implementation popup
  - collaboration mode follow-up
- 更可行的方向是：
  - 仅对 **`agent_turn_running == true` 且 `streaming == false`** 的窄窗口，
  - 把原本会走 replacement `UserTurn` 的提交改成 `UserInput` / steer；
  - 保持“正在 streaming 时先 queue”和“idle 时新 `UserTurn`”两端语义不变。
- 对照 `openai-codex`：
  - upstream 仍以 `PendingSteer` + `Op::UserTurn` 为核心，
  - 并在显式 `Interrupt` 后把 pending steers 合并重提；
  - 这条逻辑对“中断后再 steer”很强，
  - 但并不能直接解决“turn 尾声已不 streaming、却仍在 running”时被误判成 replacement turn 的问题。
  - 因此这里采用的是更窄、更直接的修补：
    - 只在该危险窗口把 follow-up 改成 `Op::UserInput`
    - 而不是全量照搬 upstream 的 pending-steer / interrupt 语义

### 2. 上下文裁剪目前只处理上一轮 assistant 尾部

当前结构性裁剪仅去掉：

- 上一 turn 中，位于“上一条 user turn”与“当前新 user turn”之间的 model-generated segment

仍未覆盖：

- 更早的 assistant 总结消息复用
- 开发者指令或上下文快照中的旧目标残留
- 模型自行生成的“过渡性总结语句”

### 3. exec / TUI / resume 入口语义尚未统一

目前不同入口对“这是继续当前任务”还是“这是开启新任务”的语义仍不完全一致。

## 后续治理路线

### Phase 1：补强黑盒验证

目标：证明问题具体残留在哪一层。

建议至少覆盖以下场景：

1. `exec` 模式，同一 session，旧任务完成后 `resume` 提交新任务。
2. `exec` 模式，旧任务完成后立即发不相关新任务，要求最终输出必须精确匹配。
3. TUI 模式，旧任务尾声提交新输入，观察是否出现旧任务总结插入。
4. TUI 模式，上一 turn `goal_complete` 后提交完全不相关新任务。
5. `Non-stop` 模式下连续两次不同目标任务，检查第二次 prompt 中是否还带第一任务尾巴。

建议固定验收方式：

- 必须使用 `exec` 模式做黑盒验证。
- 每个场景都要求：
  - 给出明确目标；
  - 不允许中途停下来问问题；
  - 用实际文件或结构化输出验收；
  - 检查最终回复是否仍夹带旧任务 recap。

### Phase 2：改为基于 running 的前端分流

目标：减少 turn 尾声误判。

计划：

- 不做“全量从 `streaming` 改成 `running`”的硬切换。
- 改为只修正最关键的窄窗口：
  - 当 `agent_turn_running == true`
  - 且 `is_assistant_streaming_in_tui() == false`
  - 把用户提交改走 `Op::UserInput`
  - 也就是 follow-up steer，而不是 replacement `Op::UserTurn`
- 继续保留：
  - streaming 中的 queue 语义
  - idle 状态下的新 `UserTurn`
  - pending turn start 时仍按新 turn 处理

建议优先检查代码：

- `codex-rs/tui/src/chatwidget.rs`
- `codex-rs/tui/src/app.rs`

### Phase 3：上下文隔离再收紧

目标：进一步降低旧任务尾巴残留概率。

计划：

- 新任务启动时，对上一轮 `goal_complete` turn 做更彻底的上下文隔离：
  - 可选地剔除上一 turn 的最后一个 assistant message
  - 或仅保留用户事实，不保留 assistant 收尾自然语言

建议优先检查代码：

- `codex-rs/core/src/context_manager/history.rs`
- `codex-rs/core/src/codex.rs`

### Phase 4：协议层显式化

目标：不再让 UI 猜语义。

计划：

- 引入明确的“继续当前任务”与“替换为新任务”语义。
- 让调用方显式声明：
  - continue current task
  - supersede previous task

建议优先检查代码：

- `codex-rs/core/src/codex.rs`
- `codex-rs/protocol`
- `codex-rs/tui`

## 验收标准

满足以下条件时，认为问题基本被根治：

1. 用户提交新任务后，模型不再自动总结旧任务尾部。
2. 新任务最终输出能稳定只聚焦当前请求。
3. turn 尾声提交输入时，不再因为 streaming/running 不一致而误判。
4. `exec` 与 TUI 两条主路径都通过黑盒验证。
5. 至少 5 个真实场景下，新任务最终输出不夹带旧任务 recap。

## 建议执行顺序

1. 先做 `exec` 黑盒验证，量化问题残留。
2. 再改 TUI 分流，从 streaming 切到 running。
3. 然后扩大上下文裁剪范围。
4. 最后再考虑协议层显式化，作为长期彻底方案。

## 当前建议

如果只选一个下一步，优先做：

1. 用 `exec resume` 做同一 session 的多轮黑盒验证，确认旧任务 recap 具体残留在新任务的哪个阶段；
2. 然后优先把 TUI 在“running but not streaming”窗口的 follow-up 改走 `UserInput` / steer，而不是全量切 `running`。

## 当前验证状态

截至目前，以下结论已经有代码与定点测试支撑：

- replacement `UserTurn` 会触发一次性旧回复段裁剪
- replacement 场景下，新 prompt 会裁掉上一轮 assistant/tool 尾巴
- TUI 在 `running == true && streaming == false` 的尾声窗口里，follow-up 已改走 `UserInput`
- 相关 `codex-core` 与 `codex-tui` 定点回归已通过

当前唯一尚未完成的高价值验证是：

- **真实在线 `exec` / `exec resume` 黑盒**
  - 当前环境下，OpenAI provider 请求返回 `403 Forbidden: Country, region, or territory not supported`
  - 因此无法在本环境里完成“真实在线同 session 多轮验证”
  - 若后续有可用 provider（例如受支持地区网络、可用 API 出口、或本地 OSS provider），应优先补做这一步
