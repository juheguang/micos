# micos 0.6: Memory & Context Lifecycle

## 定位

0.4.x 搭建了 agent 运行循环、工具、权限、会话持久化。
0.5.x 硬化了错误处理、补齐了工具矩阵、专业化了 prompt。

0.6 的目标是**让 harness 真正承担起记忆和上下文的职责**——不是让模型自己决定记什么、什么时候压缩，而是 harness 拥有触发时机、执行策略、质量控制，模型只负责语义理解这
件它擅长的事。

这是 micos 从"能跑"到"能长跑"的关键版本。

## 当前问题

### 记忆：不调模型的字符串拼接

`candidate_from_draft()` 做的事：

```
HandoffDraft.current_state + "\n\n" +
HandoffDraft.next_step + "\n\n" +
"Verification status: " + ... + "\n\n" +
"Files touched: " + ... + "\n\n" +
"Commands run: " + ... + "\n\n" +
"Known failures: " + ...
```

一个 80 字符截断的标题 + 拼接的 body。没有语义提取、没有去重判断、没有重要性评估。harness 把记忆这件事完全交给了用户手动操作（`/memory promote`），等于没做。

### 压缩：手动 + 被动

- 只能手动 `/compact`
- `compact_context_with_ui` 的 UI 参数根本没用到
- `context_warning_percent` 已配置但只写日志，不触发动作
- 压缩结果不反馈给记忆系统

### 记忆和压缩是两套不互通的系统

压缩时不提取记忆。记忆不影响压缩保留策略。两者独立运行，信息不流动。

## 设计原则

1. **Harness 拥有时机和策略，模型拥有语义能力。**
   Harness 决定何时提取记忆、何时压缩、保留什么、丢弃什么。模型负责理解对话内容、判断事实重要性、生成摘要。

2. **记忆是文件系统上的结构化事实，不是嵌入向量。**
   索引文件 + 主题文件的模式（参照 Claude Code）已被证明有效。不需要向量数据库、知识图谱、多层 SQLite。文件系统就是存储后端。

3. **压缩和记忆是同一个生命周期的两端。**
   压缩前提取记忆（Write-Before-Compaction），压缩后的摘要本身就是候选记忆来源。二者共享事件流、共享触发条件。

4. **每一步都是可审计的。**
   提取了什么、为什么、何时压缩、保留了多少——全部写入 session JSONL。

5. **模型失败不导致状态丢失。**
   提取和压缩的模型调用失败时，原始数据仍在 session log 中。可以重试、可以跳过、可以降级。

## 目标架构

```
                    ┌─────────────────────┐
                    │    Context Monitor   │ ← 每 turn 检查 token 压力
                    └─────────┬───────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼                               ▼
    ┌─────────────────┐             ┌─────────────────┐
    │  Memory Extractor│             │  Auto Compactor │
    │  (模型驱动的语义   │             │  (阈值触发 +      │
    │   提取 + 去重)    │◄────────────│   写前提取)       │
    └────────┬────────┘             └────────┬────────┘
             │                               │
             ▼                               ▼
    ┌─────────────────┐             ┌─────────────────┐
    │  Memory Store    │             │  Compact Engine │
    │  (index + topics)│             │  (摘要 + tail)   │
    └────────┬────────┘             └────────┬────────┘
             │                               │
             └───────────┬───────────────────┘
                         ▼
              ┌─────────────────────┐
              │   Context Builder    │ ← 组装每轮模型输入
              │  (prompt + memory +  │
              │   plan + compact +   │
              │   recent tail)       │
              └─────────────────────┘
```

## 功能分解

### Feature A: 模型驱动的记忆提取

**现状：** `candidate_from_draft()` 字符串拼接 → 用户手动 promote。
**目标：** session 结束时 harness 自动调用模型提取结构化记忆 → 候选记忆带置信度和来源 → 用户审核 promote。

**Harness 的职责：**
- 决定何时触发提取：session 正常结束、非正常停止、用户 `/memory refresh`
- 组装提取请求：handoff draft + 对话摘要 + 已有记忆索引 → 发给模型
- 解析模型输出：结构化记忆条目（标题、正文、类型、scope）
- 对已有条目做去重检查：与现有 entries 比较，标记 ADD/UPDATE/DUPLICATE
- 写入 candidates/

**模型的职责：**
- 理解对话内容
- 判断哪些事实值得持久化
- 按四种类型分类（user/feedback/project/reference）
- 输出结构化记忆条目

**实现：**
- 新增 `MemoryExtractor` 模块（`src/memory/extract.rs`）
- 提取 prompt 明确四种类型定义、排除规则、输出格式
- 在 `AgentRuntime` 的 stop 路径中调用：`final_answer` → 自动提取，其他 stop → 也触发提取
- 新增 `MemoryExtraction` session event
- 提取的模型调用与 compact 共享 client，独立的 instructions

### Feature B: 自动上下文压缩

**现状：** 手动 `/compact`，无自动触发，UI 参数未使用。
**目标：** harness 监控上下文压力，超过阈值自动触发压缩。warn 模式先确认，auto 模式直接执行。

**Harness 的职责：**
- 每 turn 后检查 `usage_percent >= context_warning_percent`
- 决定压缩模式：off → 不触发 / warn → 询问用户 / auto → 自动执行
- 压缩前确保最近关键信息已被记忆系统提取（Write-Before-Compaction）
- 执行压缩：调用模型生成摘要 + 验证 + 必要时修复
- 压缩后更新 context snapshot
- 熔断：连续 3 次压缩失败 → 禁用自动压缩，记录 recovery 事件

**模型的职责：**
- 生成结构化压缩摘要（保持现有 8 个标题）
- 修复失败时按验证错误重新生成

**实现：**
- 新增 `AutoCompactMode` 枚举（`src/config.rs`）：`Off` / `Warn` / `Auto`
- 新增 `src/agent/auto_compact.rs`：阈值检查 + 触发逻辑 + Write-Before-Compaction
- 重构 `compact_context_with_ui`：真正使用 `UiSink`，warn 模式弹出确认对话框
- Session 事件：`CompactTriggered`（trigger=threshold/manual）、`CompactSkipped`（reason）
- 配置项：`auto_compact` mode、`context_warning_percent`（已有）、`auto_compact_cooldown_secs`（默认 60s）

### Feature C: 记忆生命周期管理

**现状：** 手动 promote/stale/forget，无自动管理。
**目标：** harness 辅助管理记忆的新鲜度和一致性。

**Harness 的职责：**
- 生成 candidate 时与已有 entries 比较：完全相同 → DUPLICATE / 更新已有 → UPDATE / 冲突 → CONFLICT 标记
- 追踪每条 entry 的 `last_validated_at`，超过阈值标记为 stale
- MEMORY.md 索引自动维护：promote/forget 时更新，行数超限时裁剪
- `/memory sweep` 命令：列出 stale 条目、冲突条目、候选去重建议

**实现：**
- 新增 `deduplicate_candidate()` 函数（`src/memory/records.rs`）——比较新 candidate 与现有 entries 的相似度
- `ProjectMemory` 新增 `sweep()` 方法——返回需要关注的条目列表
- 新增 `SessionEvent::MemorySwept` 
- MEMORY.md 自动更新：`promote_candidate()` 追加索引行，`mark_entry_status(Forgotten)` 移除索引行

### Feature D: 上下文构建器重构

**现状：** `ContextBuilder` 负责 token 估算，但记忆注入逻辑分散在 `prompt_runtime_context()` 中，全量注入所有 active entries。
**目标：** 上下文构建统一管理所有输入源，分层组装，每层有明确的 token 预算。

**Harness 的职责：**
- 组装模型输入的固定顺序：
  1. Base prompt（静态，可缓存）
  2. 运行时上下文（cwd、model、权限模式）
  3. MEMORY.md 索引行（摘要行，非全量正文）
  4. Active plan（非空时）
  5. Compact summary（存在时）
  6. Recent tail（最近 N 条消息）
  7. 当前用户输入
- 监控每层的 token 消耗
- 当总 token 接近预算时，从低优先级层开始截断

**实现：**
- 重构 `src/context/mod.rs`：`ContextBuilder` 支持分层构建 + 每层预算
- `prompt_runtime_context()` 改为只注入 memory 索引行，不注入全量 entries
- `/context` 命令显示每层 token 分布

## 版本规划

### v0.6.0 — 记忆提取核心

- Feature A：模型驱动的记忆提取
- 四种记忆类型定义
- 自动提取触发（stop 路径）
- 手动提取（`/memory refresh` 改为调模型）

### v0.6.1 — 自动压缩

- Feature B：自动压缩 warn/auto 模式
- Write-Before-Compaction 联动
- 压缩熔断器
- 真正的 UI 交互（warn 确认对话框）

### v0.6.2 — 记忆生命周期

- Feature C：去重、过期检测、索引自动维护
- `/memory sweep` 命令
- 记忆新鲜度追踪

### v0.6.3 — 上下文分层

- Feature D：分层上下文构建
- 按需记忆检索替代全量注入
- 每层 token 预算监控

## 涉及模块

| 模块 | 0.6 改动 |
|------|---------|
| `src/memory/` | +`extract.rs`（模型提取）、+`dedup.rs`（去重）、重构 `records.rs` |
| `src/agent/` | +`auto_compact.rs`、重构 `compact.rs`、stop 路径加提取触发 |
| `src/context/` | 分层构建、每层预算 |
| `src/config.rs` | +`AutoCompactMode`、+cooldown 配置 |
| `src/session.rs` | +`MemoryExtraction`、`CompactTriggered`、`CompactSkipped`、`MemorySwept` 事件 |
| `src/prompt.rs` | +记忆提取 prompt、+压缩触发 prompt |
| `src/ui/` | +压缩确认对话框（warn 模式） |

## 不做什么

- 不引入外部数据库（SQLite、向量库、图谱）——文件系统足够
- 不做自动 promote——用户始终拥有记忆的最终审核权
- 不做多 Agent 记忆共享——单 Agent 场景优先
- 不自动执行破坏性记忆操作（forget、批量清理）——必须用户确认
- 不引入嵌入模型或语义搜索——当前规模不需要

## 验收标准

- `/memory refresh` 调用模型提取记忆，输出结构化 candidates
- candidates 携带类型、置信度、来源 session
- 新 candidate 与已有 entry 重复时标记 DUPLICATE 而非静默创建
- 上下文超阈值时 warn 模式弹出确认框，auto 模式自动压缩
- 压缩前自动触发记忆提取
- 连续 3 次压缩失败后熔断
- MEMORY.md 索引在 promote/forget 时自动更新
- `/context` 显示每层 token 分布
