# 长程任务状态持久化设计判断

## 结论

长程任务稳定推进不能依赖一次 `/compact`。`/compact` 只能压缩当前 model-visible history，适合作为上下文预算治理的一环；它不能替代跨 session 的状态持久化。

更可靠的设计是把状态拆成四层：

- **Session log**：精确记录本次会话里发生过什么，用于 resume、审计和调试。
- **Project memory / instructions**：每个新 session 都重新加载的稳定项目规则、架构事实、工作流和用户偏好。
- **Task progress artifacts**：当前任务进度、下一步、失败尝试、验证状态写进文件和 git，而不是只留在聊天里。
- **Model-visible context view**：下一轮实际给模型看的内容，由 prompt、memory、active plan、compact summary、recent tail 和当前用户输入组成。

核心原则是：chat history 不是 memory。聊天历史可以被摘要、裁剪、恢复和回放；长期任务状态必须落到文件系统、session log、git 和受控 context builder 中。

## Claude Code 的状态分层

Claude Code 的公开文档和 rebuilt 代码都指向同一个图景：新 session 从 fresh context window 开始，跨 session 依赖磁盘上的项目记忆、session 文件和可恢复的运行时元数据。

Claude Code 的主要状态层包括：

- **CLAUDE.md / rules / auto memory**：项目级和用户级持久上下文，每个 session 加载。`CLAUDE.md` 更适合放稳定规则，auto memory 更适合累积 Claude 自己发现的偏好和经验。
- **Session JSONL**：保存 prompt、tool call、tool result、assistant response 等对话链。resume 读取这些记录，恢复到旧 session。
- **Session restore state**：恢复 file history、todo、context-collapse commits、worktree state、agent setting 等运行时状态，而不只是恢复消息文本。
- **Context governance**：session 里完整发生过什么，不等于下一次 API request 应该看到什么。Claude Code 会构造专门的 model-visible view，并在 compact 后保留边界和必要尾部上下文。
- **Session memory**：rebuilt 里有实验性 session memory，会维护一个结构化 markdown 文件，记录 Current State、Task specification、Files and Functions、Workflow、Errors & Corrections、Learnings、Worklog 等。它用于 compaction 和任务续接，比单次 summary 更稳。

Claude Code compact 的关键点是：压缩不是简单整段替换。较成熟的策略会保留 recent tail，保证 `tool_use` / `tool_result` pair 不被切断，也会处理 thinking block 与 assistant message 的 API 不变量。

## Harness 视角

`docs/what is harness/` 的核心判断可以压缩成一句：harness 要承担模型结构上不适合承担的职责，包括状态持久化、停止控制、失败恢复、上下文选择、权限、验证和可观测性。

状态持久化尤其不能交给模型的“记忆感”。模型每次 API 调用看到的是一份被组装出来的上下文快照。跨 session 的连续性如果只靠聊天记录，会遇到三个问题：

- 上下文窗口有限，早期决策会被稀释或压缩掉。
- compact 是有损转换，不能保证保留所有关键约束。
- fresh session 没有旧上下文，除非 harness 主动从磁盘重新加载状态。

所以长程稳定性的基础不是“写一个更好的 summary prompt”，而是让 harness 明确拥有状态：

- 哪些信息是 durable memory。
- 哪些信息只是本轮 transient context。
- 哪些信息必须每次启动加载。
- 哪些信息只在相关文件/路径/任务触发时加载。
- 哪些信息必须写入 git 或 task progress 文件。

## micos 当前差距

micos 现在已经有几块基础：

- `.micos/sessions/*.jsonl` 保存 session event。
- `context_snapshot` 记录 context 估算和 prompt section。
- `context_summary` 保存手动 compact 结果。
- `PromptBuilder` 已有 section metadata 和 runtime section。
- model-visible tool output 已经有 preview/full 分离。

但距离长程稳定还缺三类能力：

- **没有 `/resume`**：session JSONL 还不能恢复成 runtime transcript 和 model-visible state。
- **没有 project memory / active plan**：fresh session 没有 `.micos/memory/`、`.micos/plans/` 这类稳定入口。
- **compact 仍然太粗**：当前策略是 summary message 替换旧 transcript，没有 recent tail、pair preservation、summary validation、handoff artifact。

这说明 micos 现在的 compact 是 context cleanup，不是长程任务状态系统。

## 建议目标结构

后续 micos 的下一轮 model request 应该由这些输入组成：

```text
base prompt
+ runtime prompt section
+ project instructions
+ memory index
+ active plan
+ latest handoff
+ compact summary
+ recent tail
+ current user message
```

其中：

- `base prompt` 和 `runtime prompt section` 由 prompt registry 生成。
- `project instructions` 来自项目级配置或未来的 AGENTS/CLAUDE-style 文件。
- `memory index` 是长期项目事实入口。
- `active plan` 是当前任务的可执行状态。
- `latest handoff` 是上一轮或上一 session 结束时写下的当前状态。
- `compact summary` 只代表被压缩掉的早期 model-visible context。
- `recent tail` 保留最近交互原文，避免当前任务细节被 summary 抹掉。

## 建议版本安排

### 0.3.9 Compact Governance

- 已把 compact 输出改成 `summary + recent tail`。
- recent tail 默认保留最近 8 条 transcript items。
- 已保证 `function_call` / `function_call_output` 不被切断。
- compact report 已增加 retained messages、compression ratio、summary validation status。
- summary 缺固定 section、缺最近用户请求、缺验证状态时不替换 transcript。

### 0.4.0 Resume Foundation

- 新增 `micos chat --resume <session-id-or-path>`。
- 新增 `/resume <session-id-or-path>`。
- 从 `.micos/sessions/*.jsonl` 恢复 compact boundary 后的 model-visible transcript。
- 若没有 compact，则从原始 user、assistant、tool call 和 tool output 事件恢复 transcript。
- resume report 显示恢复了多少 messages、是否使用 summary、tail 数量和估算 token。

### 0.4.1 Project Memory

- 已新增 `.micos/memory/MEMORY.md`，作为项目级 memory index。
- 已新增 `.micos/memory/topics/*.md`，承载详细主题笔记。
- 启动时只把非模板 memory index 注入 prompt，topic 文件按需读取。
- 已新增 `/memory`、`/memory index`、`/memory <topic-file.md>`。
- 已新增 `memory_loaded` session event。

### 0.4.2 Active Plan / Handoff

- 新增 `.micos/plans/active.md`。
- 在 `/exit`、compact、失败 stop、用户 interrupt 前写 handoff。
- handoff 固定记录 current state、next step、files touched、commands run、verification status、known failures。
- 新 session 自动注入 latest handoff。

### 0.4.3 Memory Promotion

- 引入 `session note -> candidate memory -> accepted memory` 流程。
- 只有 accepted memory 会进入 fresh session startup context。
- 每条 memory 记录 source、last_validated、scope、staleness。
- `/memory promote` 把当前 session 结论提升为 durable memory。

## 设计底线

- 不把完整 session log 当作每轮 model input。
- 不把 compact summary 当作唯一续接状态。
- 不把长期项目规则放在早期聊天里。
- 不自动把所有 session 总结提升为 memory。
- 不让模型自己决定哪些状态必须持久化；harness 应该给出结构、入口和验证标准。

## References

- [Claude Code memory docs](https://code.claude.com/docs/en/memory)
- [Claude Code how it works](https://code.claude.com/docs/en/how-claude-code-works)
- [Claude Code Agent SDK sessions](https://code.claude.com/docs/en/agent-sdk/sessions)
- [Claude Code prompt caching](https://code.claude.com/docs/en/prompt-caching)
- [Anthropic: Effective harnesses for long-running agents](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)
- [micos Claude Code context governance research](./claude-code-context-governance-research.md)
- [micos harness roadmap](./harness-roadmap.md)
- [What is harness](./what%20is%20harness/)
