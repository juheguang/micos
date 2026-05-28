# Claude Code 权限与 Trace 调研：面向 micos 0.3.1

本文调研 Claude Code 的公开文档与本仓库中的
`refs/claude-code-rebuilt` 参考实现，用来约束 `micos` 下一步 0.3.1
的工程范围。0.3.1 应该保持聚焦：先把工具权限决策结构化、可解释、可追踪，
再考虑 hooks、MCP、多代理、自动分类器等更复杂能力。

## 调研来源

公开文档，检查时间为 2026-05-27：

- Anthropic, [Configure permissions](https://code.claude.com/docs/en/permissions)
- Anthropic, [Claude Code settings](https://code.claude.com/docs/en/settings)
- Anthropic, [Claude Code hooks](https://code.claude.com/docs/en/hooks)

本地参考代码：

- `refs/claude-code-rebuilt/src/Tool.ts`
- `refs/claude-code-rebuilt/src/tools.ts`
- `refs/claude-code-rebuilt/src/types/permissions.ts`
- `refs/claude-code-rebuilt/src/utils/permissions/permissions.ts`
- `refs/claude-code-rebuilt/src/utils/permissions/permissionRuleParser.ts`
- `refs/claude-code-rebuilt/src/utils/permissions/shellRuleMatching.ts`
- `refs/claude-code-rebuilt/src/services/tools/toolExecution.ts`
- `refs/claude-code-rebuilt/src/services/tools/toolHooks.ts`
- `refs/claude-code-rebuilt/src/services/tools/toolOrchestration.ts`
- `refs/claude-code-rebuilt/src/services/tools/StreamingToolExecutor.ts`
- `refs/claude-code-rebuilt/src/QueryEngine.ts`
- `refs/claude-code-rebuilt/src/query.ts`

## 核心结论

Claude Code 把权限处理设计成一条运行时 policy pipeline，并且每次决策都保留
可解释原因。一次工具调用会经过 schema 校验、工具自己的输入校验、规则匹配、
mode 默认策略、hook 影响、用户批准、实际执行、结果封装、transcript 记录和
telemetry。对 `micos` 0.3.1 最重要的启发是接口形状：每次决策都应该返回
`allow`、`ask` 或 `deny`，同时携带 typed reason、来源、可选的更新后输入和
trace event。

0.3.1 不应该试图完整复刻 Claude Code。classifier、sandbox、hook runner、
MCP tool pool、subagent mode、并发 streaming executor 都有价值，但都可以后置。
当前最有收益的工作，是把 `src/tools/policy.rs` 里基于 mode 的简单判断，替换成
一个小型 policy engine，并能记录工具为什么被允许、拒绝或交给用户确认。

## 应对齐的公开行为

Claude Code 公开权限模型有几个稳定特征：

- 只读操作一般不需要批准，shell 命令和文件修改一般需要批准。
- 规则行为分为三类：`allow`、`ask`、`deny`。
- 规则评估顺序是 deny -> ask -> allow。裸 tool deny 可以让模型完全看不到该
  tool；带 specifier 的 scoped deny 会让 tool 保持可见，但在运行时阻止匹配的调用。
- 规则语法是 `Tool` 或 `Tool(specifier)`。Bash 类工具支持通配符命令模式，文件规则
  使用路径匹配语义。
- permission mode 改变默认行为，例如正常提示、自动接受编辑、plan mode、auto mode、
  deny-unapproved mode、bypass mode。即使是 bypass mode，也会保留极危险操作的断路器。
- hooks 可以扩展权限行为。`PreToolUse` 可以 allow、deny 或 ask，但 deny/ask 规则仍然会
  在 hook 输出之后继续生效。hook 静默通过不等于批准工具调用。

这些契约的本质是把用户 policy 和模型指令分开。prompt 可以要求模型谨慎行事，真正
控制能不能执行的是 harness。

## 参考架构观察

### 工具能力模型

`Tool.ts` 把每个工具定义成 capability object，而非单纯函数。tool interface 同时承载
schema、call 逻辑、启用状态、只读状态、并发安全、破坏性标记、中断行为、用户可见名称、
工具级 permission check、路径提取和 permission pattern matcher。

这对 `micos` 的意义是：policy 层应该向 tool 询问只有 tool 才知道的事实。例如：

- `run_shell` 可以判断只读命令和命令模式。
- `read_file` 可以暴露路径，并声明为只读。
- `write_file` 可以暴露路径，声明为 mutating，并给出覆盖写风险摘要。

Rust 侧可以先抽象成带关联类型的 trait：

```rust
pub trait Tool {
    type Input;
    type Output;

    fn spec(&self) -> ToolSpec;
    fn validate(&self, input: &Self::Input, ctx: &ToolContext) -> Result<()>;
    fn permission_hint(&self, input: &Self::Input, ctx: &ToolContext) -> PermissionHint;
    async fn call(&self, input: Self::Input, ctx: ToolContext) -> Result<Self::Output>;
}
```

`ToolSpec` 应包含 `name`、`description`、schema、`read_only`、`destructive`、
`concurrency_safe` 和 `interrupt_behavior`。policy 层基于 capability contract 做决策，
避免每个内置工具都散落一套特殊判断。

### Permission Context 与规则来源

Claude Code 的 permission context 把 runtime mode 和规则集合拆开：always allow、
always ask、always deny、additional working directories、mode 可用性开关，以及避免弹出
prompt 的运行标记。它的 permission 类型还追踪规则来源，例如 user settings、project
settings、local settings、policy settings、CLI 参数、slash command、session state。

`micos` 初期不需要全部 scope，但类型里应该保留 source 维度。没有来源的规则，后续很难
解释。

建议的第一版类型：

```rust
pub enum RuleSource {
    Config,
    Cli,
    Session,
    Runtime,
}

pub enum RuleBehavior {
    Allow,
    Ask,
    Deny,
}

pub struct PermissionRule {
    pub source: RuleSource,
    pub behavior: RuleBehavior,
    pub tool: String,
    pub specifier: Option<String>,
}
```

### Permission Decision Pipeline

参考实现的核心顺序如下：

1. 如果匹配 whole-tool deny rule，直接拒绝。
2. 如果匹配 whole-tool ask rule，交给用户确认。
3. 调用工具自己的 tool-specific permission 逻辑。
4. 保留工具自己的 deny 和 content-scoped ask。
5. 保留 safety check，即使当前 mode 偏宽松也要生效。
6. 应用 mode-level bypass 或默认行为。
7. 应用 whole-tool allow rule。
8. 把 unresolved passthrough 转换为 ask decision。

`micos` 应该学习这条 pipeline 的概念结构。pipeline 最终生成一条 durable decision record：

```rust
pub enum PermissionDecision {
    Allow { reason: DecisionReason, updated_input: Option<Value> },
    Ask { reason: DecisionReason, message: String, suggestions: Vec<PermissionSuggestion> },
    Deny { reason: DecisionReason, message: String },
}

pub enum DecisionReason {
    Rule { rule: PermissionRule },
    Mode { mode: PermissionMode },
    Tool { reason: String },
    SafetyCheck { reason: String },
    RuntimeApproval { user_choice: String },
    Other { reason: String },
}
```

现有 `safe|ask|auto` 可以映射进这个模型：

- `safe`：允许只读工具和安全 shell read；mutating 工具按当前产品行为 ask 或 deny。
- `ask`：mutating 和 shell 工具默认 ask，除非规则明确 allow 或 deny。
- `auto`：已知安全命令 allow，不明确命令 ask，已知危险命令 deny。名称可以保留，但实现应由
  policy 驱动，不应是宽泛快捷路径。

### 规则解析与 Shell 匹配

Claude Code 使用 `Tool` 和 `Tool(specifier)` 规则语法。Bash 规则支持 exact 与 wildcard
command pattern，也会保守处理 compound command 和 process wrapper。

0.3.1 适合实现一个小而有用的子集：

- 解析 `tool` 和 `tool(specifier)`。
- 空 specifier 或 `*` 视为 whole-tool。
- shell 命令支持 exact text 和简单 `*` wildcard。
- 只按明显 top-level separator 拆 compound command：`&&`、`||`、`;`、`|`、`|&`、`&`、newline。
- compound command 需要每个 subcommand 都被允许，整体才允许。

PowerShell AST、wrapper stripping、symlink path check、classifier-aware dangerous pattern stripping
先不做，等 policy core 有测试和 trace 输出后再扩展。

### Tool Pool Filtering

Claude Code 会在模型看到 tool list 之前过滤 blanket-denied tools。这和 scoped deny 不同：
`Bash` 作为 deny rule 会隐藏 Bash；`Bash(rm *)` 保持 Bash 可见，只在运行时拒绝匹配调用。

`micos` 应采用这个拆分：

- `deny: run_shell` 从模型 tool schema 中移除 `run_shell`。
- `deny: run_shell(rm *)` 保留 `run_shell` 可见，只拒绝匹配调用。

这样能减少模型尝试永远不可能执行的工具，同时保留 scoped policy 的严格运行时控制。

### Hooks

hooks 是后续能力，但 0.3.1 的类型设计应该预留位置。Claude Code 暴露了 `PreToolUse`、
`PostToolUse`、permission request、permission denied、stop、compact、session start、
subagent 等生命周期事件。hook output 可以 block、add context、request permission、update
input 或 continue。

当前 `micos` 不需要运行 hook command。只需要预留 decision reason 和 trace event 字段：

- `DecisionReason::Hook { hook_name, reason }`
- `TraceEvent::HookStarted`
- `TraceEvent::HookFinished`
- `TraceEvent::HookDecision`

需要保留的主要契约是：hook 可以影响决策，但规则仍然执行 deny/ask 优先级。

### 执行与 Streaming

Claude Code 有两条执行路径：

- batch executor：按 concurrency safety 对 tool calls 分组。安全的连续调用可并发；mutating 或
  exclusive 调用串行。
- streaming executor：tool-use block 流式到达时即开始执行，追踪 queued/executing/completed/yielded
  状态，最终结果按模型顺序缓冲输出，并尽早发出 progress。

`micos` 已经支持模型输出 streaming。0.3.1 应该缩小改动：继续保持串行 tool execution，但在稳定点
发出 trace events：

- `tool.requested`
- `tool.permission_decided`
- `tool.started`
- `tool.progress`
- `tool.finished`
- `tool.failed`

并发执行可以等到每个 tool 都声明 `concurrency_safe`，且 context updates 有确定性 merge 规则后再做。

### Trace 与可观测性

Claude Code 会记录 permission denials，供 SDK 输出使用；当批准结果没有经过交互 UI 路径时，会发
`tool_decision` telemetry event；同时还围绕 tool execution 建 span，并记录 hooks 和 permission
check 的耗时。这里最值得学习的是本地 trace model：使用产品安全的摘要，不把可能含有 secret 的原始
参数当作 trace 专用字段到处复制。

`micos` 第一版应该把 local JSONL trace events 写入现有 session log。payload 中只有当 transcript
已经包含原始 tool arguments 时才保留完整参数；trace-specific fields 使用摘要：

```json
{
  "type": "tool_permission_decision",
  "turn_id": "01...",
  "tool": "run_shell",
  "argument_summary": "cargo test",
  "decision": "allow",
  "reason": { "type": "rule", "source": "session", "rule": "run_shell(cargo test)" },
  "elapsed_ms": 12
}
```

TUI 可以直接渲染这些信息，不需要暴露 call id 或内部 permission plumbing。

## 建议的 0.3.1 范围

### 1. 引入 Permission Policy Module

新增 `src/tools/policy/` 或 `src/policy/`，拆成几个聚焦模块：

- `rule.rs`：`PermissionRule`、`RuleBehavior`、`RuleSource`
- `parser.rs`：`Tool(specifier)` parser 与 formatter
- `matcher.rs`：tool 和 shell pattern matching
- `decision.rs`：`PermissionDecision`、`DecisionReason`
- `engine.rs`：有序 evaluation pipeline

如果兼容性需要，可以让 `src/tools/policy.rs` 保持为 thin re-export。

### 2. 增加 Tool Capability Metadata

扩展当前 tool registry，让每个 tool 具备：

- `name`
- `read_only`
- `destructive`
- `concurrency_safe`
- `permission_class`
- `argument_summary`
- 可选 `permission_hint`

policy、trace、TUI 和未来 evals 都应该消费同一份 metadata。

### 3. 在 Config 中存储规则

增加一个最小配置形状：

```toml
[permissions]
allow = ["list_files", "read_file", "run_shell(cargo test)"]
ask = ["write_file", "run_shell(git push *)"]
deny = ["run_shell(rm *)", "read_file(.env)"]
```

建议优先级：

1. CLI/session overrides
2. project config
3. default mode behavior

这个层级比 Claude Code 的 managed/user/project/local 简化很多，但已经能承载 durable rules。

### 4. 记录 Trace Events

新增稳定 session events：

- `TurnStarted`
- `TurnFinished`
- `PermissionDecision`
- `ToolRequested`
- `ToolStarted`
- `ToolFinished`
- `ToolFailed`
- `RecoveryNote`

`PermissionDecision` 至少包含 `decision`、`reason`、`rule_source`、`tool`、`argument_summary`、
`elapsed_ms` 和 `mode`。

### 5. 暴露 Operator Views

等数据结构存在后，再补 slash commands：

- `/permissions`：展示 effective mode 和 active rules。
- `/trace`：展示上一轮 compact trace。

TUI 的批准提示继续走对话布局内联展示，并复用当前斜杠菜单的下拉选择样式。

### 6. 增加确定性测试

0.3.1 需要覆盖：

- 能解析 `tool`、`tool(*)`、`tool(specifier)`。
- deny 优先于 ask 和 allow。
- whole-tool deny 会从 schema output 中隐藏工具。
- scoped deny 会阻止运行时调用，但保持工具可见。
- safe shell command 在 `safe` mode 下允许。
- dangerous shell command 根据 mode 被 deny 或 ask。
- compound command 需要每个 subcommand 都允许。
- permission decision session event 包含 typed reason 和 elapsed time。

## 暂缓工作

以下能力不纳入 0.3.1：

- auto-mode classifier 和 dangerous-rule stripping。
- hook execution。
- MCP client integration。
- concurrent tool execution。
- sandboxing。
- multi-agent delegation。
- 大型 tool result externalization。
- 完整 telemetry export。

这样 0.3.1 的中心就很清晰：typed capability、typed policy、typed trace。

## 验收标准

0.3.1 完成时应满足：

- 只看 session log 就能解释一次 tool call 的权限结果。
- permission rules 可以独立 parse、format、match、test。
- tool visibility 会尊重 whole-tool deny rules。
- runtime tool approval 会记录 `allow|ask|deny`、source、mode 和 reason。
- 现有 `safe|ask|auto` CLI 表面行为保持兼容。
- `cargo test` 覆盖 policy ordering 和 trace serialization。

