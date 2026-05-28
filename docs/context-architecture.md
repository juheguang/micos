# micos Context Architecture

每次模型 API 调用时，harness 组装一份**上下文快照**（context snapshot）。这份快照不是
append-only 聊天记录，而是由 **7 个固定层** 按优先级从高到低组成的结构化视图。

## 分层全景

```
┌──────────────────────────────────────────────────┐
│ Layer 1: Base prompt         (~3K tokens)         │ ← 11 段静态行为指令
├──────────────────────────────────────────────────┤
│ Layer 2: Runtime context       (~100 tokens)       │ ← cwd / model / permission / date
├──────────────────────────────────────────────────┤
│ Layer 3: Memory index          (~500 tokens)       │ ← 一行一条的活跃记忆摘要
├──────────────────────────────────────────────────┤
│ Layer 4: Active plan         (0~2K tokens)        │ ← 当前任务进度文件
├──────────────────────────────────────────────────┤
│ Layer 5: Compact summary      (0~3K tokens)       │ ← 早期对话的结构化压缩
├──────────────────────────────────────────────────┤
│ Layer 6: Recent tail         (~4K tokens)         │ ← 最近 N 条原始消息
├──────────────────────────────────────────────────┤
│ Layer 7: Tool schemas / input  (剩余)             │ ← 工具定义 + 当前用户输入
└──────────────────────────────────────────────────┘
```

## 各层详解

### Layer 1: Base prompt

**来源**: `src/prompt.rs` → `BASE_SECTIONS` 常量（11 个章节）

固定的行为指令，不随 session 变化。包括：
- Identity — 你是谁
- Task discipline — 怎么做事
- Tool selection — 何时用哪个工具
- File editing — 怎么编辑代码
- Permissions — 怎么处理权限
- Error recovery — 工具失败怎么办
- Verification — 验证纪律
- Context governance — 长任务中保持目标
- Safety — 安全底线
- Tool reference — 每个工具的用法
- Reporting style — 输出格式

**特性**: 静态、可缓存。是所有 session 共享的。

### Layer 2: Runtime context

**来源**: `PromptRuntimeContext::body()`（cwd、model、api kind、permission mode、
context window、current date）

让模型知道自己在哪、用什么模型、有什么权限、今天几号。

### Layer 3: Memory index

**来源**: `.micos/memory/` 下所有 status=active 的 entry

**关键设计**: 注入的是**索引行**，不是全量正文。

```
## Accepted durable memory (use `/memory <id>` for details)
- [project] Use cargo test before reporting success — Always run tests before committing.
- [feedback] Prefer edit over write_file for existing files — User corrected on 2026-05-27.
```

每条一行：`[类型] 标题 — 一句话摘要`。模型需要详情时用 `/memory <id>` 或
`read_file` 按需读取。对比之前注入全量正文（每条 3-5 行），token 节省约 80%。

**生命周期**: 
1. session 结束时 `MemoryExtractor` 调模型提取 → candidates/
2. 用户 `/memory promote` → entries/（status=Active）
3. `promote_candidate()` 自动更新 MEMORY.md 索引
4. 超过 30 天未验证 → `/memory sweep` 标记为 stale
5. 用户 `/memory forget` → status=Forgotten，从索引移除

### Layer 4: Active plan

**来源**: `.micos/plans/active.md`

非空时注入。包含当前任务状态、下一步、已修改文件、验证状态。由 session 结束时的
handoff 机制自动维护。

### Layer 5: Compact summary

**来源**: 手动 `/compact` 或自动压缩后的结构化摘要（8 个标题）

只在发生过压缩的 session 中存在。代表早期对话的权威压缩。8 个固定标题：
Primary Request and Intent / Key Technical Concepts / Files and Code Sections /
Errors and Fixes / Decisions Made / Pending Tasks / Current Work / Next Step。

### Layer 6: Recent tail

**来源**: transcript 中最近 N 条原始消息（user/assistant/tool_call/tool_output）

压缩后保留的未压缩尾消息。如果没压缩过，这里就是完整的对话历史。
`CompactPlan` 保证 `function_call`/`function_call_output` 配对不被切断。

### Layer 7: Tool schemas + current input

工具定义（经过权限策略过滤的 schema 列表）+ 当前用户消息。这是模型每次调用的
"当前工作"。

## 组装流程

```
AgentRuntime::build_model_context()
  │
  ├─ 1. tools: schemas_for_policy() — 按权限模式过滤工具列表
  │
  ├─ 2. prompt: PromptBuilder::build(config, runtime)
  │     ├─ BASE_SECTIONS (11 段静态指令)
  │     ├─ Runtime context (cwd/model/permission/date)
  │     ├─ Memory index (active entries 索引行)
  │     ├─ Active plan (非空时)
  │     └─ Project append (config.append_system_prompt)
  │
  ├─ 3. ContextBuilder::build(transcript, prompt, tools)
  │     ├─ 组装 instructions（所有 prompt section 拼接）
  │     ├─ 组装 input（compact summary + recent tail + 新 user 消息）
  │     ├─ 计算 categories（instructions/tool_schemas/user_messages/...）
  │     └─ 计算 layers（7 层 token 分布）
  │
  └─ 4. ModelContext { input, instructions, prompt_sections, tools, stats }
```

## 上下文治理

### 压缩触发

- 自动：每 turn 开始前 `maybe_auto_compact()` 检查 `usage_percent >= warning_percent`
- 手动：`/compact` 命令
- Write-Before-Compaction：压缩前先 `extract_memories()` 确保关键信息落盘
- 熔断：连续 3 次压缩失败 → 禁用自动压缩

### 记忆联动

- 每次 session 结束 → 自动提取记忆（`MemoryExtractor`）
- 压缩前 → 先提取记忆（防止压缩丢失关键信息）
- 新 session 启动 → 注入 MEMORY.md 索引行 + active plan

### 查看上下文

`/context` 命令显示分层的 token 消耗：

```
context: /path/to/session.jsonl
model: deepseek-v4-pro
estimated tokens: 8200 / 1000000 (0%)
pressure: ok (warning at 80%)
categories:
  instructions              3200
  tool_schemas               200
  user_messages              800
  ...
layers:
  base_prompt              2800 tokens  34%
  runtime                   120 tokens   1%
  memory_index              450 tokens   5%
  active_plan                 0 tokens   0%
  compact_summary           800 tokens  10%
  recent_tail              1500 tokens  18%
  tool_schemas              200 tokens   2%
```

## 与工业产品的对比

| 维度 | micos 当前 | Claude Code | Codex |
|------|-----------|-------------|-------|
| Prompt 层数 | 7 层 | 10+ 层（含 MCP/skills/agents） | 5+ 层（含 collaboration mode） |
| Memory 注入 | 索引行（~500 tokens） | MEMORY.md 索引 + Sonnet 侧调用选 5 个 topic | SQLite + 两阶段流水线 |
| 压缩 | 手动 + 自动阈值 + 熔断 | 三层（micro/session/legacy）+ 远程 API | 远程 API + SQLite |
| 上下文查看 | `/context` 分层显示 | `/context` + 分析仪表盘 | telemetry pipeline |
| 持久化 | 文件系统（JSONL + TOML + MD） | 文件系统（MD + JSON） | SQLite（3 个 DB） |
