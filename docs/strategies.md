# micos 策略机制总览

## 权限模式 (Permission Mode)

三种模式 + plan 模式：

| 模式 | 读工具 | 写文件 | Shell 安全 | Shell 危险/未知 |
|---|---|---|---|---|
| **Safe** | 允许 | 拒绝 | 允许 | 拒绝 |
| **Ask** | 允许 | 询问 | 询问 | 询问 |
| **Auto** | 允许 | 项目内允许 | 允许 | 危险拒绝/未知询问 |
| **Plan** | 允许 | 仅允许 `.micos/plans/active.md` | 允许 | 拒绝 |

### 权限规则系统

配置文件 `.micos/config.toml` 中的 `[permissions]` 支持三种规则：

```
allow = ["list_files", "shell(cargo *)"]   # 白名单
ask = ["write_file", "shell(git push *)"]  # 需要审批
deny = ["shell(rm *)", "shell(curl *)"]    # 黑名单
```

**决策优先级**：硬保护（路径安全）→ deny 规则 → ask 规则 → allow 规则 → 模式默认

**硬保护**（不可被规则覆盖）：
- 符号链接路径拒绝写入
- 敏感路径拒绝：`.git` `.ssh` `.env` `*.pem` `*secret*` `*token*` 等
- 非项目目录（无 `.git` `Cargo.toml` 等标记）写入需审批

**运行时审批**：`Ask` 模式下弹窗提供三个选项：
- **Session** — 本次会话记住规则
- **Once** — 仅本次调用
- **Project** — 持久化到 `.micos/config.toml`

---

## Plan 模式

### 进入/退出

- `/plan-mode` 斜杠命令 或 模型调用 `enter_plan_mode` / `exit_plan_mode` 工具
- 进入时暂存 `pre_plan_mode`，退出时恢复原模式
- 进入时自动加载 Task List

### 退出审批弹窗

模型调用 `exit_plan_mode` 时：
1. 读取 `.micos/plans/active.md` 内容
2. TUI 底部显示黄色 "plan review" 弹窗
3. 三个选项：
   - **Approve** (`a`) — 批准，退出 plan mode，开始实现
   - **Edit** (`e`) — 暂不执行，保持 plan mode
   - **More** (`m`) — 追加指导，保持 plan mode

### Plan Mode Prompt（4 阶段引导）

- Phase 1 — 探索代码库
- Phase 2 — 设计方案，用 `task_create` 分解任务
- Phase 3 — 写计划到 `.micos/plans/active.md`
- Phase 4 — 调用 `exit_plan_mode`

---

## 工具执行与错误处理

### 工具调用流程

```
模型返回 tool_call → 权限策略判断 → 
  ├─ Allow → 执行工具 → 结果 push 到 transcript
  ├─ Ask  → 弹窗审批 → Allow/Deny 同上
  └─ Deny  → denied 结果 push 到 transcript（不停止）
```

### 工具结果分类

| 结果 | 处理 |
|---|---|
| 成功 | push 到 transcript，模型看到完整输出 |
| 被拒 (Deny) | push 到 transcript 含拒绝原因，模型可调整策略 |
| 执行错误 (Error) | push 到 transcript，模型可尝试其他方法 |
| 大输出截断 | 会话日志保留完整输出，模型看到截断预览 |

### 工具不停止 Turn

- 工具被拒 → **不停止**，流过 transcript
- 工具错误 → **不停止**，流过 transcript  
- 只有 API 错误、用户中断、Esc 取消才停止 turn

### Shell 命令分类

对常用命令做了安全分级：

- **Safe**：`pwd` `ls` `cat` `head` `tail` `rg` `find`（无 `-delete`/`-exec`）`git status` `git diff`
- **Dangerous**：`rm` `rmdir` `mv` `cp` `chmod` `sudo` `curl` `git push` `git reset` 等
- **Unknown**：未分类的命令

---

## 上下文压缩 (Context Compaction)

### 三层策略

| 层级 | 触发条件 | 机制 |
|---|---|---|
| **Micro-compact** | 超过 6 个 tool output + 空闲 10 分钟 | 清除旧 tool output 内容，保留最近 6 个 |
| **Auto-compact** | 上下文用量超过警告阈值 (默认 80%) | 调模型生成摘要，替换旧消息为摘要 |
| **Manual compact** | `/compact` 命令 | 同 auto-compact，用户主动触发 |

### Compact 验证与修复

- 生成摘要后验证 8 个必需标题
- 验证最新用户请求是否被保留
- 验证是否包含验证状态关键词
- 验证失败 → 自动发起一次修复请求（带错误信息）
- 修复失败 → 保持原始 transcript 不变

### Auto-compact 安全机制

- 连续 3 次失败后永久禁用
- 两次 compact 之间至少间隔 60 秒
- `Warn` 模式下弹窗询问用户

---

## 中断与取消

### 三种取消方式

| 操作 | 效果 |
|---|---|
| **Esc** | 取消当前 turn，恢复 composer 文本 |
| **Ctrl+C** | 取消当前 turn + 清空队列，再按一次退出 |
| **Enter 提交新消息** | 排队新消息，取消当前 turn |

### 取消传播链路

```
Esc/Ctrl+C → CancellationToken.cancel() → agent loop 检查 is_cancelled()
  ├─ API 调用中 → tokio::select! 竞速取消
  └─ Shell 执行中 → kill 子进程 + abort stdout/stderr
```

### 取消后处理

- Composer 文本在 UserInterrupt 时自动恢复（Esc 取消场景）
- Ctrl+C 清空排队消息
- 工具结果完整性保持（已执行的 tool 结果已在 transcript 中）

---

## 消息排队 (Message Queue)

- agent 执行中按 Enter → 输入入队 + 取消信号中断当前 turn
- TurnFinished 时 `drain_pending_messages()` 逐条出队执行
- Ctrl+C 清空队列
- 页脚显示 `[N queued]`

---

## API 错误恢复

### 错误分类

| 类别 | 匹配条件 | 行为 |
|---|---|---|
| **ContextPressure** | 413 / prompt_too_long | 分层恢复 |
| **RateLimit** | 429 | 直接停止 |
| **ServerError** | 5xx | 分层恢复 |
| **Fatal** | 其他 | 直接停止 |

### 分层恢复

```
API 错误
  └─ Layer 1: micro-compact 清理 + retry (需要 max_retries >= 1)
  └─ Layer 2: 强制 full compact + retry (需要 max_retries >= 2)  
  └─ Layer 3: StopReason::ApiError
```

`max_retries` 默认 2（环境变量 `MICOS_MAX_RETRIES`）。

---

## Task 系统

### 数据模型

- 存储：`.micos/tasks/{id}.json` — 每个 task 一个 JSON 文件
- 状态：`pending` → `in_progress` → `completed`
- 字段：id, subject, description, active_form, status, blocks, blocked_by

### 四个 Task 工具

| 工具 | 功能 |
|---|---|
| `task_create` | 创建任务，status = pending |
| `task_get` | 查询单个任务详情 |
| `task_update` | 更新状态、标题、描述 |
| `task_list` | 列出所有活跃任务 |

- 进入 plan mode 自动加载 Task List
- 退出 plan mode 后 tasks 保留，执行阶段继续使用

---

## 记忆系统 (Project Memory)

### 存储结构

```
.micos/memory/
  index.md          — 记忆索引（总是注入 system prompt）
  topics/           — 主题文件（按需注入）
  entries/          — 接受的记忆条目
  candidates/       — 候选记忆（待用户审批）
```

### 记忆命令

| 命令 | 功能 |
|---|---|
| `/memory` | 查看记忆索引、条目和候选 |
| `/memory sweep` | 扫描过期和重复记忆 |
| `/memory promote <id>` | 批准候选记忆 |
| `/memory stale <id>` | 标记记忆为过期 |
| `/memory forget <id>` | 删除记忆或候选 |

### 记忆生命周期

- 会话结束后模型提取候选记忆
- 用户审批后进入 entries/ 成为持久记忆
- 启动时加载 active entries 到 system prompt
- `/memory sweep` 清理过期条目

---

## 会话与恢复

### Session 存储

- 路径：`.micos/sessions/{uuid}.jsonl`
- 每行一个 JSON 事件，共 26 种事件类型
- 覆盖完整生命周期：session_start → user_input → tool_call → tool_output → context_snapshot → stop

### 会话恢复

- `/resume <session-id>` 从 JSONL 重建上下文
- 支持 compact 过的会话（摘要 + 保留尾部）

### Handoff & Recovery

- **Handoff**：非 FinalAnswer 停止时自动写入 `.micos/plans/active.md`
- **Recovery Report**：非 FinalAnswer 停止时写入 `.micos/recovery/latest.md`
- 包含：停止原因、失败分类、文件变更、命令记录、验证状态

---

## 验证 (Verification)

- 配置：`.micos/verify.toml`
- 命令：`/verify` 执行配置的检查
- 默认检测 Rust 项目（`cargo test` `cargo fmt --check` `cargo build`）
- 验证失败自动写 recovery report

---

## Transition 追踪

每个 turn loop 迭代记录 `last_transition`：

| Transition | 含义 |
|---|---|
| `NextTurn` | 正常工具执行完毕，继续下一轮 |
| `MicroCompactApplied` | Micro-compact 清除了旧输出 |
| `AutoCompactApplied` | 自动压缩成功 |
| `AutoCompactSkipped` | 自动压缩被跳过 |
| `CompactRetry` | API 错误恢复了 compact 重试 |

可通过 `agent.last_transition()` 查询。

---

## 配置速查

| 配置项 | 默认值 | 环境变量 |
|---|---|---|
| permission | ask | `MICOS_PERMISSION` |
| max_steps | 无上限 | `MICOS_MAX_STEPS` |
| max_retries | 2 | `MICOS_MAX_RETRIES` |
| context_warning_percent | 80 | `MICOS_CONTEXT_WARNING_PERCENT` |
| context_window_tokens | 200000 | `MICOS_CONTEXT_WINDOW_TOKENS` |
| auto_compact | off | `MICOS_AUTO_COMPACT` |
| model | gpt-5.3-codex | `MICOS_MODEL` |
| thinking | 未设置 | `MICOS_THINKING` |
| reasoning_effort | 未设置 | `MICOS_REASONING_EFFORT` |
