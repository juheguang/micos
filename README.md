# micos

`micos` 是一个最小 Rust agent harness。它提供 REPL，会调用模型、在本地执行工具，并把会话记录写入 `.micos/sessions`。

## 安装

在仓库根目录构建：

```sh
cargo build
```

创建本地环境文件：

```sh
cp .env.example .env
```

把 API key 和接口配置写进 `.env`。`.env` 已加入 `.gitignore`。

## 模型配置

默认 OpenAI Responses API：

```env
MICOS_MODEL=gpt-5.3-codex
MICOS_BASE_URL=https://api.openai.com/v1/responses
MICOS_API_KEY=sk-...
```

DeepSeek：

```env
MICOS_MODEL=deepseek-chat
MICOS_BASE_URL=https://api.deepseek.com/chat/completions
MICOS_API_KEY=sk-...
MICOS_THINKING=enabled
MICOS_REASONING_EFFORT=max
```

其他 OpenAI-compatible Chat Completions 服务：

```env
MICOS_MODEL=your-model-name
MICOS_BASE_URL=https://service.example/v1/chat/completions
MICOS_API_KEY=service-key
```

所有服务都统一使用 `MICOS_API_KEY`。

`micos` 会根据 `MICOS_BASE_URL` 自动选择请求格式：以 `/responses` 结尾的 URL 使用 Responses API，其它 URL 使用 Chat Completions。

## 基础 Prompt

`micos` 会为每次模型请求注入一份精简的基础 system prompt，覆盖 identity、task discipline、tool use、permissions、context governance、verification 和 reporting style。它借鉴 coding agent 常见约束：改代码前先读相关文件、工具由 harness 执行、权限拒绝要尊重、验证结果不能伪造、长任务中保持当前目标、保护用户已有改动、避免擅自执行破坏性 git 命令，并保持最终汇报紧凑。

Prompt 由 registry 组装，最终仍作为单个 `instructions` 字符串发给模型，但运行时会保留 section metadata。`/prompt` 可以查看当前请求的 prompt section、来源和估算 token。基础 section 之后会追加一个 runtime context section，包含 cwd、model、API kind、permission mode、context window 和当前日期。

项目可以在 `.micos/config.toml` 中追加额外约束：

```toml
append_system_prompt = "Use this repository's existing module boundaries and keep final replies concise."
```

该字段只追加到基础 prompt 之后，不会替换基础 prompt。

## DeepSeek 思考模式

DeepSeek Chat Completions 支持思考模式。`micos` 提供两个可选配置：

```env
MICOS_THINKING=enabled
MICOS_REASONING_EFFORT=high
```

可选值：

- `MICOS_THINKING`: `enabled` 或 `disabled`
- `MICOS_REASONING_EFFORT`: `low`、`medium`、`high`、`xhigh`、`max`

DeepSeek 文档说明 `low`、`medium` 会映射为 `high`，`xhigh` 会映射为 `max`。如果不设置这两个变量，`micos` 不会发送 DeepSeek 专有参数，由服务端默认值决定。

思考模式下如果发生工具调用，DeepSeek 要求后续请求回传 `reasoning_content`。`micos` 会在 transcript 中保留该字段，并在继续工具调用循环时回传。

## 运行

启动 REPL：

```sh
cargo run -- chat
```

通过命令行覆盖配置：

```sh
cargo run -- chat --model deepseek-chat --base-url https://api.deepseek.com/chat/completions --thinking enabled --reasoning-effort max --permission ask
```

权限模式：

- `safe`：允许读工具；拒绝写文件；shell 只允许保守的只读命令。
- `ask`：允许读工具；写文件和 shell 命令执行前询问。
- `auto`：允许内置工具在当前工作目录范围内执行。

退出 REPL：

```text
/exit
```

REPL 支持常用 slash commands：

- `/help`：显示命令列表。
- `/status`：显示 model、API kind、base URL、permission、context window、cwd、session id 和日志路径。
- `/sessions`：列出 `.micos/sessions` 最近会话，包含可用于 resume 的 session id。
- `/transcript`：显示当前 transcript 路径和最近事件摘要。
- `/summary`：显示当前 session 最近一次 compact summary 正文。
- `/trace`：显示最近工具调用、权限决策、拒绝原因和 stop reason。
- `/prompt`：显示当前 system prompt 的 section、来源和估算 token。
- `/context`：显示当前模型上下文的估算 token、窗口大小和分类占用。
- `/compact`：调用模型生成固定格式摘要，并用摘要替换当前 model-visible context。
- `/resume <session-id-or-path>`：从旧 session JSONL 恢复当前运行时 model-visible context。
- `/memory`：显示项目本地 memory index 和 topic 列表。
- `/model`：在 TUI 模式中选择模型和思考设置。
- `/clear`：清屏。
- `/exit`：退出。

模型输出会以流式方式显示。工具调用会先显示工具卡片；`ask` 权限模式下，`write_file` 和未知风险的 `shell` 会在执行前确认。批准时可以选择仅本次、本 session 记住，或写入项目配置。

## 内置工具

- `list_files`：列出当前工作目录下的非递归目录项。
- `read_file`：读取当前工作目录内的 UTF-8 文本文件。
- `write_file`：在权限策略允许后覆盖写入文件。
- `shell`：在当前工作目录运行 shell 命令，带超时和输出截断。

## 配置优先级

配置优先级从高到低：

1. CLI flags
2. `.env` 或 shell 中的环境变量
3. `.micos/config.toml`
4. 默认值

`.micos/config.toml` 示例：

```toml
model = "deepseek-chat"
base_url = "https://api.deepseek.com/chat/completions"
thinking = "enabled"
reasoning_effort = "max"
permission = "ask"
max_steps = 20
context_window_tokens = 200000
append_system_prompt = "Prefer concise engineering reports."

[permissions]
allow = ["list_files", "read_file", "shell(git status*)", "shell(git diff*)"]
ask = ["shell(git push *)"]
deny = ["shell(rm *)", "shell(curl *)"]
```

`permission = "safe|ask|auto"` 仍然是默认权限模式。`[permissions]` 会在模式默认值之前生效：

- `tool` 或 `tool(*)` 表示 whole-tool rule，例如 `deny = ["shell"]` 会让模型请求 schema 中不再出现 `shell`。
- `tool(pattern)` 表示 scoped rule，例如 `deny = ["shell(rm *)"]` 会保留 `shell` schema，但匹配调用会在运行时被拒绝。
- `*` 是简单通配符。`shell` 规则会把 `&&`、`||`、`;`、`|`、`|&`、`&` 和换行分隔出的子命令逐段检查。

`context_window_tokens` 默认是 `200000`。当 `base_url` 是 DeepSeek 官方 API 且 model 为 `deepseek-v4-*` 时，默认窗口自动使用 `1000000`；显式配置仍然优先。

每次模型请求前都会写入 session JSONL 的 `context_snapshot` 事件，用于记录粗估 token、窗口大小、分类占用和 prompt section 摘要。每次工具权限判断都会写入 `permission_decision` 事件。REPL 中可用 `/prompt`、`/context` 和 `/trace` 查看当前 session 的 prompt、上下文和权限 trace。

## 手动 Compact

`/compact` 会对当前 model-visible transcript 发起一次无工具模型调用，生成固定结构摘要：

- Primary Request and Intent
- Key Technical Concepts
- Files and Code Sections
- Errors and Fixes
- Decisions Made
- Pending Tasks
- Current Work
- Next Step

compact 成功后，运行时 transcript 会变成 `summary message + recent tail`：早期 model-visible context 由 summary 承接，最近 8 条 transcript items 继续保留原文，且会向前扩展以避免 `function_call` / `function_call_output` 被切断。原始事件仍完整保存在 `.micos/sessions/*.jsonl` 中。

compact summary 会先做基础校验：必须非空、包含固定 headings、保留最近用户请求片段，并说明 test/verification 状态。校验失败时不会替换当前 transcript。

compact 会在 session JSONL 中记录：

- compact 前后的 `context_snapshot`
- `context_summary`，字段包含 `timestamp`、`summary`、`summary_tokens`、`messages_replaced`、`retained_messages`、`summary_format_version`、`trigger`
- `context_compacted`，字段包含 `timestamp`、`before_tokens`、`after_tokens`、`summary_tokens`、`messages_replaced`、`retained_messages`、`compression_ratio_percent`、`validation_status`

`/summary` 会从当前 session JSONL 中反向查找最近一条 `context_summary`，并显示完整摘要。`/transcript` 只显示一行 `context_summary` 摘要，不展开正文。

如果当前 model-visible transcript 为空，或太短以至于 recent tail 会保留全部内容，`/compact` 会返回 `nothing to compact`，不会调用模型，也不会写入 `context_summary` 或 `context_compacted`。如果 compact 模型调用失败，当前 transcript 不会被替换。

## Resume

可以在启动时恢复旧 session：

```bash
cargo run -- chat --resume <session-id-or-path>
```

也可以在 REPL/TUI 中执行：

```text
/resume <session-id-or-path>
```

resume 会读取 `.micos/sessions/*.jsonl`，重建当前运行时的 model-visible transcript。若旧 session 有成功 compact，则优先恢复 `compacted summary + retained tail`；否则从 `user_input`、`assistant_text`、`tool_call` 和 `tool_output` 事件重建 transcript。`context_snapshot`、`permission_decision`、trace/report 和 stop 事件不会进入 model-visible context。

resume 不会重放工具，也不会修改旧 session JSONL；它会在当前新 session 中写入 `session_resumed`，字段包含 `timestamp`、`source_session_id`、`source_path`、`restored_messages`、`used_summary`、`restored_tail_messages`、`estimated_tokens`。

## Project Memory

每次 `chat` 启动时，`micos` 会初始化项目本地 memory：

```text
.micos/memory/MEMORY.md
.micos/memory/topics/*.md
```

`.micos/` 默认由 `.gitignore` 忽略，因此这层 memory 是当前 checkout 的本地运行态。`MEMORY.md` 用于稳定项目事实、约定、常见失败和 runbook；如果它不是空模板，会作为 `Project memory` prompt section 注入每次模型请求。topic 文件不会自动进入上下文，只能按需查看。

REPL/TUI 支持：

- `/memory`：显示 memory root、index、token 估算和 topic 列表。
- `/memory index`：显示完整 `MEMORY.md`。
- `/memory <topic-file.md>`：读取 `.micos/memory/topics/` 下的单个 topic 文件；路径逃逸会被拒绝。

session JSONL 会在启动时写入 `memory_loaded`，字段包含 `timestamp`、`root`、`index_path`、`index_tokens`、`topic_count`、`created_index`。

## Model-visible 工具输出

工具原始输出仍会完整写入 session JSONL 的 `tool_output` 和 `tool_finished` 事件，供审计和排查使用。为了避免大输出污染后续模型上下文，传回模型的 `function_call_output` 会使用 bounded preview：

- 小输出完整传回模型。
- 超过 12 KiB 的输出只传回 preview。
- preview JSON 中包含 `truncated`、`original_bytes`、`preview_bytes`、`omitted_bytes`。

这层治理只影响模型可见上下文，不改变工具本身的执行结果和本地日志。

当工具需要批准时，交互选项为：

- `y`：只允许本次调用。
- `s` 或直接回车：为当前 session 加入一条 allow rule。
- `p`：把 allow rule 追加写入 `.micos/config.toml` 的 `[permissions].allow`。
- `n` 或 Esc：拒绝本次调用。

`write_file` 会优先生成较窄的路径规则，例如 `write_file(src/*)` 或 `write_file(README.md)`；`shell` 会优先生成当前命令规则，例如 `shell(cargo test)`，常见查看命令会生成 `shell(git diff*)` 这类前缀规则。已有 deny rule 仍然优先。

## 测试

运行测试：

```sh
cargo test
```

格式化：

```sh
cargo fmt
```
