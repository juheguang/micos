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
- `/status`：显示 model、API kind、base URL、permission、cwd、session id 和日志路径。
- `/sessions`：列出 `.micos/sessions` 最近会话。
- `/transcript`：显示当前 transcript 路径和最近事件摘要。
- `/clear`：清屏。
- `/exit`：退出。

模型输出会以流式方式显示。工具调用会先显示工具卡片；`ask` 权限模式下，`write_file` 和 `shell` 会在执行前确认。

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
```

## 测试

运行测试：

```sh
cargo test
```

格式化：

```sh
cargo fmt
```
