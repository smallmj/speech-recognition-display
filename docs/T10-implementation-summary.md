# T10 实现总结：会议纪要（停止后分批汇总）

> Issue: [#11](https://github.com/smallmj/speech-recognition-display/issues/11)
> 依赖：T9 真实 LLM（OpenAI 兼容 + SSE + 重试）已在 main；本实现基于 main（含 PR #20 非阻塞 LLM 整理）。

## 实现内容

- **engine 分批/汇总纯函数**（`engine/src/minutes.rs`，唯一测试缝）：
  - `chunk_for_summarize(segments, max_chars)`：把有序片段按时间窗分批。每批正文
    ≤ `BATCH_MAX_CHARS`（500 字）；第 2+ 批开头携带上一批正文末尾 ≤
    `ROLLING_CONTEXT_CHARS`（100 字）作滚动上文；正文 + 滚动上文计入
    `MAX_TOKENS`（2000 token）预算。文本优先取整理版、回退原文、空白跳过；
    单条超长片段单独成批不截断（宁可超预算也不丢字，注释说明取舍）。
  - `summarize_minutes(llm, segments, max_chars)`：同步模拟路径的分批 → 逐批
    `summarize` → ≥2 批再汇总的完整编排，可确定性断言「分批边界与汇总顺序」。
  - `CleanupPipeline::freeze_all_active()`：停止会话时冻结剩余 active 片段。
- **真实纪要调用**（`src-tauri/src/llm.rs`）：
  - 新增 `MINUTES_PERSONA`（要求输出【要点】【行动项】【待办】分节）。
  - 抽出 `chat_once`（整理/纪要共用同一 SSE 客户端）与 `run_with_retries`
    （等比退避 500ms/1s/2s，最多 4 次尝试）。
  - `OpenAiLlmClient::summarize`：按【第 N 段】标记拼接各批文本送 LLM，失败
    返回错误信息（由壳层回退该批原文/拼接兜底）。
- **停止/开始会话**（`src-tauri/src/pipeline.rs` + `lib.rs`）：
  - `SessionControl`（`stop_requested` / `start_requested` 原子标志）与
    `stop_session` / `start_session` Tauri 命令。
  - 驱动线程：应用启动自动开始一个会话；收到停止 → 收净缓冲 final → 冻结
    剩余 active → 排空在途整理（PR #20 的非阻塞 worker 结果照常回填）→
    `SessionStopped` → engine 分批 → 逐批真实 LLM → 汇总 → `MinutesReady`。
  - 「开始识别」重建整理管线（片段 id 从 0 复用）并 emit `SessionStarted`。
- **前端**（薄渲染）：
  - `src/components/MinutesPanel.tsx` + `.css`：纪要面板，轻量解析【分节】
    标题渲染，生成中显示「正在生成纪要…」。
  - `src/App.tsx`：会话状态徽章 + 「▶ 开始识别」「⏹ 停止并生成纪要」按钮；
    `sessionStarted`/`sessionStopped`/`minutesReady` 驱动状态机；`sessionEvents`
    只保留最近一次 `sessionStarted` 之后的事件，避免新旧会话混排。
  - `src/styles.css`：会话控制按钮样式。

## 验收标准逐条对照

- [x] **停止后触发分批总结流程**：「停止并生成纪要」→ `stop_session` →
  停止追加 → 冻结剩余 → 排空整理 → `SessionStopped` → 分批 → 逐批 LLM →
  `MinutesReady` → 前端纪要面板。
- [x] **分批边界（500 字 / 2000 token）正确**：`chunk_for_summarize` 纯函数
  每批正文 ≤500 字、第 2+ 批带 ≤100 字滚动上文、正文 + 滚动上文 ≤2000 token
  预算；13 个测试断言边界（少量 1 批 / 超限多批 / 滚动上文 / 空输入 / 空白
  跳过 / 超长单条不截断 / 优先整理版 / token 预算）。
- [x] **汇总生成结构化纪要（要点/行动项/待办）并可查看**：`MINUTES_PERSONA`
  强制分节；逐批部分纪要 → ≥2 批再汇总；`MinutesPanel` 按分节渲染展示。

## 验证

- `cargo test`：engine 59 个测试 + 壳层 29 个测试全部通过（含 minutes 13、
  纪要 prompt、事件序列化、freeze_all_active、pipeline_idle 回归）。
- `pnpm build`：TypeScript 类型检查与 Vite 构建通过。
- `pnpm check:focus-exit`：通过。

## 已知限制

- 分批按字符数时间窗切分（每批 ≤500 字），滚动上文取上一批正文末尾 ≤100 字；
  超长单条不截断（宁超预算不丢字）。
- 单批纪要失败回退该批原文、汇总失败拼接各批部分纪要，尽力保证 `MinutesReady`
  总有内容。
- 再次「开始识别」重建管线并清空上一会话片段；历史纪要仅保留最近一次
  `minutesReady`（会话历史与导出属 T11）。
