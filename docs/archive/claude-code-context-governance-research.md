# Claude Code Context Governance 调研

面向 micos 0.3.4 的设计输入。调研范围以本仓库本地材料为准：`refs/claude-code-rebuilt`、`docs/what is harness/`，以及当前 micos 0.3.3 代码状态。`docs/what is harness/book1-claude-code.pdf` 是压缩 PDF，当前环境没有可用的文本抽取工具；本报告对 harness 的理解补充主要来自同目录 `zhihu-harness.md`，核心证据来自 Claude Code rebuilt 源码。

## 结论

Claude Code 的 context governance 是一条运行时管线，覆盖“要给模型什么、何时给、怎么统计、何时压缩、怎么向用户暴露”。它的提示词按优先级、缓存边界、动态区段、用户环境、工具能力、记忆文件、token budget 等组件装配，形态上已经远离单个大字符串。

micos 当前已经有 session JSONL、权限 trace、完整 assistant message 记录，但模型上下文仍然是 `AgentRuntime.transcript` 的 append-only 克隆：每轮直接把全量 transcript、全量工具 schema 和固定 `system_instructions()` 发给模型。短期最值得做的是先补 `ContextBuilder` 与 `/context`：先用粗估 token 统计和分类视图建立可观测性，再做 compact。没有统计就直接做压缩，容易看不到收益和误伤。

建议 0.3.4 目标聚焦三件事：

1. 建立模型可见上下文视图：把 session log、UI transcript、model input 分开。
2. 增加 token/context 统计：粗估可先行，按 system、messages、tool schema、tool output 分类。
3. 增加 `/context` 和 `ContextSnapshot` trace：让用户知道当前上下文占用、最大窗口、风险来源。

## Claude Code 的上下文分层

### 1. Effective System Prompt

`refs/claude-code-rebuilt/src/utils/systemPrompt.ts` 中 `buildEffectiveSystemPrompt` 明确写了优先级：override prompt 最高；coordinator prompt、agent prompt、custom prompt、default prompt 按条件取用；`appendSystemPrompt` 最后追加。这里的关键是系统提示词被当作“可组合配置”管理，已经不再是一次性拼接文本。

默认 prompt 的主体在 `refs/claude-code-rebuilt/src/constants/prompts.ts`。它把内容分成静态区和动态区：

- 静态区：intro、system、doing tasks、actions、using tools、tone/style、output efficiency。
- 动态区：session guidance、memory、model override、environment、language、output style、MCP instructions、scratchpad、function-result clearing、tool result summarization、token budget、brief。
- 中间有 `SYSTEM_PROMPT_DYNAMIC_BOUNDARY`，用于区分可以全局缓存的 prompt prefix 和用户/会话相关内容。

`refs/claude-code-rebuilt/src/constants/systemPromptSections.ts` 进一步把动态区段做成 registry：普通 section 会缓存到 `/clear` 或 `/compact`，特殊 section 才允许每轮重算并打破 prompt cache。这个设计说明 Claude Code 关心的不只是“写什么提示词”，还关心提示词变化对缓存、成本和一致性的影响。

### 2. System Context 与 User Context

`refs/claude-code-rebuilt/src/context.ts` 把上下文分成两类：

- `getSystemContext`：会话级系统上下文，包含 git status 快照、最近 commits、当前分支等，并限制 git status 最大字符数。
- `getUserContext`：用户上下文，包含 CLAUDE.md/memory 文件和当前日期，且支持 `CLAUDE_CODE_DISABLE_CLAUDE_MDS`、bare mode 等开关。

这两个函数都 memoize 到当前 conversation，并在特定注入变化时清缓存。它们承担“把运行环境转成模型可见事实”的角色。

### 3. API Cache-Key Prefix

`refs/claude-code-rebuilt/src/utils/queryContext.ts` 把 `defaultSystemPrompt`、`userContext`、`systemContext` 作为 API cache-key prefix 的三个核心部分。它还在 side question fallback 里去掉未完成的 assistant message，避免 mid-turn partial assistant 内容污染 cache-safe 上下文。

这说明 Claude Code 有一个稳定边界：session 里发生了什么，不等于下一次 API 请求应该看到什么。模型可见视图需要专门构造。

### 4. Messages API View

`refs/claude-code-rebuilt/src/commands/context/context-noninteractive.ts` 的 `/context` 统计路径会复用 query loop 前的上下文转换：先取 compact boundary 之后的 messages，再按 contextCollapse 投影视图，之后跑 microcompact，最后才分析 token。这个命令的目标是估算“模型下一次实际会看到的内容”，单纯数 session log 不够。

这点对 micos 很重要：session JSONL 应该保留完整事件；model input 可以是经过裁剪、摘要、预览和分类后的视图。

## Token 与 Context 统计

Claude Code 统计 token 的思路分三层：

1. 从真实 API response 取 usage：`refs/claude-code-rebuilt/src/utils/tokens.ts` 会读取 `input_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens`、`output_tokens`。
2. 对新增消息做估算：`tokenCountWithEstimation` 用最后一次 API usage 作为锚点，再估算此后新增消息，避免累积重复计数。
3. 必要时调用 token count API 或粗估：`refs/claude-code-rebuilt/src/services/tokenEstimation.ts` 有 `countMessagesTokensWithAPI`，失败时有粗估逻辑，默认 `chars / 4`，JSON 类内容按更密集的比例估算。

`refs/claude-code-rebuilt/src/utils/analyzeContext.ts` 把统计拆成多个 category：messages、tool schema、memory files、MCP tools、builtin tools、system prompt sections、agents、slash commands、skills、autocompact buffer、free space。`refs/claude-code-rebuilt/src/commands/context/context-noninteractive.ts` 再把这些输出成 `/context` 可读表格。

所以“token 统计是否可以加上”的答案是：可以，而且应该先加一个简化版。第一版不需要精确 tokenizer，也不需要调用供应商 token count API。对 micos 来说，先有稳定的粗估和分类，比追求精准 token 数更关键。

## Compact 与 Context Pressure

Claude Code 的 compact 是上下文治理管线里的压缩策略，已经超出临时摘要按钮的层次。

`refs/claude-code-rebuilt/src/services/compact/autoCompact.ts` 定义了有效上下文窗口、summary 输出预留、warning/error/autocompact/blocking 阈值，以及自动 compact 熔断。它先预留 summary 输出空间，再计算触发阈值，避免到了极限才发现没有空间写摘要。

`refs/claude-code-rebuilt/src/services/compact/prompt.ts` 的 compact prompt 很结构化：要求总结用户意图、技术概念、文件和代码段、错误修复、问题解决、全部用户消息、待办、当前工作、下一步。它还明确要求 compact 阶段只输出文本，不调用工具。这里的本质是“状态转移协议”：把长历史转成可继续执行的结构化状态。

`refs/claude-code-rebuilt/src/services/compact/microCompact.ts` 处理更细粒度的工具结果压缩，尤其是旧工具输出和缓存相关场景。`refs/claude-code-rebuilt/src/services/contextCollapse/` 在 rebuilt 版本里大多是 feature-gated/stub，但从 `/context`、warning UI 和 autoCompact 的调用位置看，它预留的是更细粒度的 collapse 投影和状态恢复机制。

micos 现阶段不适合直接实现完整 contextCollapse。更合理的顺序是：统计与可视化先行；然后做手动 `/compact`；最后再考虑自动 compact 和工具输出外置。

## Harness 视角

`docs/what is harness/zhihu-harness.md` 的核心判断可以概括为：harness 承担模型结构上不适合承担的运行时职责，例如状态持久化、停止条件、失败处理、权限、验证和上下文压缩。这个视角和 Claude Code 的实现是吻合的。

放到 context governance 上，模型只负责在当前上下文里生成；harness 负责决定上下文的来源、顺序、压缩、缓存、可观测性和边界。提示词只是其中一层。真正的 governance 是一套围绕 query loop 的上下文操作系统。

## micos 当前差距

当前 micos 0.3.3 的主路径在 `src/agent.rs`：

- `run_turn_with_ui` 把用户输入、assistant output、function call、function call output 直接 append 到 `self.transcript`。
- 每轮构造 `ModelRequest` 时直接 clone `self.transcript`。
- `tools.schemas_for_policy(...)` 已经会按权限策略过滤工具 schema。
- `system_instructions()` 仍是固定四行字符串。
- session JSONL 已有 `AssistantText`、`ToolCall`、`ToolOutput`、`PermissionDecision`、`ToolFinished` 等事件。

这说明 micos 已经具备治理的基础事件流，但还缺少“模型可见上下文”的独立构造层。现在 session 事件、UI 呈现、model input 基本绑在一起；后续一旦 tool output 变大、对话变长、工具 schema 增多，就会遇到不可解释的上下文膨胀。

## 建议设计：0.3.4 Context Governance Foundation

### 目标

0.3.4 不急着做自动 compact。先让 micos 能回答四个问题：

1. 下一轮模型会看到多少 token？
2. 哪些部分占用最多？
3. 当前离模型上下文上限还有多远？
4. session 里完整记录和 model-visible input 是否分离？

### 新模块

建议增加 `src/context/`：

- `builder.rs`：`ContextBuilder`，输入 transcript、system sections、tool schemas、config，输出 `ModelContext`。
- `stats.rs`：`ContextStats`、`ContextCategory`、`estimate_tokens`。
- `view.rs`：`ModelVisibleItem` 或直接先用 `serde_json::Value` 包一层 metadata。
- `window.rs`：模型上下文窗口配置，先给默认值，支持 config/env override。

第一版可以非常小：

```rust
pub struct ModelContext {
    pub input: Vec<serde_json::Value>,
    pub instructions: String,
    pub tools: Vec<serde_json::Value>,
    pub stats: ContextStats,
}

pub struct ContextStats {
    pub total_tokens_estimate: usize,
    pub max_tokens: usize,
    pub categories: Vec<ContextCategory>,
}

pub struct ContextCategory {
    pub name: &'static str,
    pub tokens: usize,
}
```

### Token 估算

先用粗估即可：

- 普通文本：`chars / 4`。
- JSON/tool output：`chars / 2` 或 `bytes / 2`，避免低估。
- 工具 schema：对 schema JSON stringify 后估算。
- instructions：按文本估算。

后续如果接 OpenAI/Anthropic token count API，再把 estimator 做成 trait：

```rust
trait TokenEstimator {
    fn estimate_text(&self, text: &str) -> usize;
    fn estimate_json(&self, value: &serde_json::Value) -> usize;
}
```

### Slash Command

新增 `/context`：

```text
context:
  model: gpt-5.1-codex
  estimated tokens: 12.4k / 200k (6%)
  categories:
    instructions      120
    tool schemas      1.8k
    user messages     2.1k
    assistant         3.4k
    tool output       5.0k
  largest risk: tool output
```

输出应短，和当前 `/trace` 风格一致。TUI 后续再做可视化。

### Session Trace

可以新增 `SessionEvent::ContextSnapshot`，但建议不要每个 streaming delta 都写。触发点放在每次 API request 之前：

```json
{
  "type": "context_snapshot",
  "timestamp": "...",
  "model": "...",
  "estimated_tokens": 12400,
  "max_tokens": 200000,
  "categories": [
    {"name": "tool_output", "tokens": 5000}
  ]
}
```

这会让排查“为什么突然上下文变大”有证据。

### Model Input 与 Session Log 分离

建议从 0.3.4 开始明确两条路径：

- session JSONL：完整事件、可回放、审计用。
- model input：下一次模型实际可见内容，可被裁剪、摘要、替换为 artifact 引用。

第一阶段先不裁剪，只由 `ContextBuilder` 原样输出 `self.transcript.clone()`，同时统计。这样风险低，也能为 0.3.5 compact 打基础。

## 0.3.5 以后

0.3.5 可以做手动 `/compact`：

- compact prompt 采用结构化 sections：用户意图、关键文件、已修改内容、错误与修复、待办、当前工作、下一步。
- compact 结果作为 synthetic summary 插入 model-visible transcript。
- session JSONL 保留原始历史和 compact event。

0.3.6 再考虑自动 compact：

- 基于 `/context` 的 token 统计设置 warning threshold。
- 自动 compact 前预留 summary 输出空间。
- 连续 compact 失败要有熔断，避免循环烧 API。

工具输出治理可以并行推进：

- 大输出先存 session/artifact，model input 只放摘要和路径。
- `read_file`、`shell` 输出加 preview/full 分离。
- trace 里保留完整 output hash、长度、预览，后续支持 `/show-output <call_id>`。

## 风险与取舍

不要先做复杂 tokenizer。micos 现在需要的是趋势、分类和阈值；账单级精度可以后置。

不要把 compact 混进 transcript append 逻辑。应先建立 ContextBuilder，让 compact 成为 model-visible view 的一种转换。

不要把 Claude Code 的 contextCollapse 作为近期目标。它依赖更复杂的状态投影、后台 summarizer、缓存编辑和恢复路径。micos 现在的产品阶段，更需要一个清楚、可测、可解释的小型上下文层。

## 建议验收标准

0.3.4 完成时，应能验证：

- `/context` 能显示估算 token、max tokens、分类占用。
- 每次 API 请求前写入 `context_snapshot`。
- 现有 `/transcript`、`/trace` 行为不变。
- `cargo test` 通过。
- 构造一个大 `shell` 输出后，`/context` 能显示 tool output 成为主要占用。
- model request 的构造从 `src/agent.rs` 中抽到 context 模块，后续 compact 不再需要侵入主循环。

## 参考文件

- `refs/claude-code-rebuilt/src/utils/systemPrompt.ts`
- `refs/claude-code-rebuilt/src/constants/prompts.ts`
- `refs/claude-code-rebuilt/src/constants/systemPromptSections.ts`
- `refs/claude-code-rebuilt/src/context.ts`
- `refs/claude-code-rebuilt/src/utils/queryContext.ts`
- `refs/claude-code-rebuilt/src/utils/tokens.ts`
- `refs/claude-code-rebuilt/src/services/tokenEstimation.ts`
- `refs/claude-code-rebuilt/src/utils/analyzeContext.ts`
- `refs/claude-code-rebuilt/src/commands/context/context-noninteractive.ts`
- `refs/claude-code-rebuilt/src/services/compact/autoCompact.ts`
- `refs/claude-code-rebuilt/src/services/compact/prompt.ts`
- `refs/claude-code-rebuilt/src/services/compact/microCompact.ts`
- `docs/what is harness/zhihu-harness.md`
- `src/agent.rs`
- `src/session.rs`
- `src/ui/slash.rs`
