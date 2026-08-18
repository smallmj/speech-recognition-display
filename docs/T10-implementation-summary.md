# T10 实现总结：会议纪要（停止后分批汇总）

对应 Issue #11「T10 会议纪要（停止后分批汇总）」。停止识别后把整段内容按
时间窗分批（每批约 500 字 + 滚动上文，上限 2000 token）交给 LLM，逐批生成
部分纪要，再汇总为结构化会议纪要（【要点】【行动项】【待办】），前端可查看。
依赖 T9 真实 LLM 接入（PR #15），基于 `feat/t9-real-llm` 分支开发，
分支 `feat/t10-minutes`。

## 架构位置

分批编排逻辑放 **engine**（唯一测试缝，纯函数、确定性），真实 LLM 调用放
**Tauri 壳**（沿用 T9 的 `OpenAiLlmClient` 模式），前端加会话控制与纪要展示。

## 改动的文件

### engine（核心库，唯一测试缝）

- `engine/src/minutes.rs`（新增）：
  - 常量：`BATCH_MAX_CHARS = 500`（每批正文上限，对齐 ADR-0003「单次输入
    ≤500 字」）、`ROLLING_CONTEXT_CHARS = 100`（滚动上文长度，约 1-2 条
    片段）、`MAX_TOKENS = 2000`（每批正文 + 滚动上文的 token 预算，按中文
    约 1 字符 ≈ 1 token 估算，500+100=600 字远低于预算）、
    `ROLLING_CONTEXT_MARKER = "【上文】"`。
  - `chunk_for_summarize(segments, max_chars_per_batch) -> Vec<Vec<String>>`：
    把有序片段切成时间窗批次。文本优先取 `cleaned`（整理版）、无则回退
    `raw`（原文）、空白跳过；贪心累计每批正文 ≤ 上限，超限开新批；单条
    超长片段**单独成批、不截断**（宁可超预算也不丢字，注释说明取舍）；
    第 2+ 批开头插入上一批**正文**（不含其自身滚动上文）末尾至多 100 字
    作滚动上文，防丢失跨批语义。
  - `summarize_minutes(llm, segments, max_chars) -> String`：同步模拟路径的
    完整编排——分批 → 逐批 `llm.summarize(batch)` 生成部分纪要 → ≥2 批时
    `llm.summarize(&partials)` 汇总为最终纪要（单批时该批即最终纪要）。
    Tauri 壳的真实异步链路与它同构（只是把 `LlmPort` 换成真实客户端）。
- `engine/src/lib.rs`：`pub use minutes::{...}` 导出（分批函数 + 常量 +
  同步编排）；模块文档补 T10 说明。
- `engine/src/cleanup.rs`：
  - `CleanupPipeline::store_mut()`：可变访问片段存储（停止会话时冻结剩余
    active 用）。
  - **T9 遗留缺口修复**：`apply_cleanup_result` 现在与 `fail_pending` 对称，
    无论结果是否生效都清 `pending` 并解锁单在途。此前异步成功路径会残留
    在途状态，驱动线程会无限重复处理同一条 pending（T9 壳层从未在本机编译
    运行，缺口未被发现）；T10 停止排空循环同样依赖此行为。新增回归测试
    `async_apply_cleanup_result_releases_in_flight_and_pending` 锁定。

### src-tauri（Tauri 2 壳）

- `src-tauri/src/llm.rs`：
  - 新增 `MINUTES_PERSONA` 纪要人设（要求输出结构化纪要：分节【要点】
    【行动项】【待办】，分条列出，只输出正文）。
  - **重构出可复用请求执行**：T9 的 `stream_once` 泛化为 `chat_once(system,
    user, on_delta)`（整理/纪要共用；`stream` 由是否有回调决定，非 SSE 响应
    整段解析兜底保留），退避重试循环提取为 `run_with_retries`（复用
    `MAX_RETRIES`/`BACKOFF_MS`，避免复制粘贴）。
  - 新增 `OpenAiLlmClient::summarize(chunks, on_delta)`：user 消息为各批
    文本按【第 N 段】标记拼接，system 用 `MINUTES_PERSONA`；`on_delta` 为
    Some 时 SSE 流式、None 时非流式整段返回（纪要展示用）；失败退避重试
    3 次。
- `src-tauri/src/pipeline.rs`：
  - 新增 `SessionControl`（`stop_requested` / `session_active` 两个
    `Arc<AtomicBool>`）与 `stop_session` / `start_session` 两个
    `#[tauri::command]`（State 注入）。
  - `spawn_cleanup_driver` 改造：每拍先检查停止信号，收到后走停止分支；
    停止后等待「开始识别」，检测 `session_active` 上升沿时重建整理管线
    （清空上一会话片段，id 从 0 复用）并 emit `SessionStarted`。启动即自动
    开始一个会话（对齐 T9 演示行为）。
  - 新增 `run_stop_flow`：停止追加 → `freeze_all_active` 冻结剩余 → 排空
    整理队列（在途 LLM 结果照常回填，纪要尽可能基于整理版文本）→ emit
    `SessionStopped` → `chunk_for_summarize` 分批 → 逐批
    `client.summarize(&[batch_text])` 生成部分纪要（每批一个请求，≤500 字 +
    滚动上文，防上下文溢出）→ ≥2 批时 `client.summarize(&partials)` 汇总
    为最终纪要 → emit `MinutesReady`。单批失败回退该批原文、汇总失败拼接
    各批部分纪要兜底（尽力保证有内容可看）。
- `src-tauri/src/lib.rs`：setup 中 `app.manage(pipeline::SessionControl::new())`，
  注册 `stop_session` / `start_session` 命令。

### src（React 前端，薄渲染）

- `src/components/MinutesPanel.tsx` + `.css`（新增）：纪要展示面板——生成
  期间显示「正在生成纪要…」状态提示（用户故事 36），就绪后轻量解析
  【分节】标题（行首 `【…】`）渲染为小节，其余按段落；任何 LLM 输出形状
  都能可靠展示。
- `src/App.tsx`：顶部工具栏加「▶ 开始识别」「⏹ 停止并生成纪要」按钮与
  会话状态徽章（识别中 / 正在停止 / 正在生成纪要 / 纪要已生成）；订阅
  `engine://event` 驱动会话状态机（`sessionStarted` 清空纪要并回到识别中、
  `sessionStopped` 进入生成态、`minutesReady` 展示纪要）；`sessionEvents`
  useMemo 只保留最近一次 `sessionStarted` 之后的事件（重新开始会话时切掉
  上一会话旧事件，避免新旧片段混排——engine 重建管线后 id 从 0 复用）。
- `src/styles.css`：会话控制按钮样式（对齐现有 badge/chip 风格）。

## 验收标准逐条对照

- [x] **停止后触发分批总结流程**
  前端「停止并生成纪要」→ invoke `stop_session` → 驱动线程停止追加 →
  冻结剩余 active → 排空整理队列 → `SessionStopped` → engine 分批 →
  逐批调真实 LLM → `MinutesReady` 事件 → 前端展示纪要面板。
- [x] **分批边界（500 字 / 2000 token）正确**
  engine `chunk_for_summarize` 纯函数分批：每批正文 ≤500 字（`BATCH_MAX_CHARS`），
  第 2+ 批携带上一批末尾 ≤100 字滚动上文，正文 + 滚动上文计入 2000 token
  预算（`MAX_TOKENS`，字符 ≈ token 估算口径注释说明）；测试逐条断言分批
  边界（少量 1 批 / 超限多批且每批 ≤ 上限 / 滚动上文出现在第 2+ 批开头 /
  空输入空批 / 单条超长单独成批不截断 / 优先整理版回退原文 / 空白跳过）。
- [x] **汇总生成结构化纪要（要点/行动项/待办）并可查看**
  `MINUTES_PERSONA` 要求 LLM 输出分节【要点】【行动项】【待办】；每批生成
  部分纪要、≥2 批再汇总为最终纪要（engine `summarize_minutes` 同步路径可
  测「分批边界与汇总顺序」）；前端 `MinutesPanel` 按分节渲染展示。

## 如何操作验证

1. 配置（应用内）：展开「⚙️ LLM 配置」→ 填 Base URL / API Key / 模型名 →
   保存（纪要与整理复用同一配置）。
2. 运行：`pnpm tauri dev`。应用启动自动开始一个会话，合成转写以约 3~4s
   一条的节奏流入（气泡 → 防抖 → LLM 流式整理 → 双轨展示）。
3. 点「⏹ 停止并生成纪要」：驱动线程停止追加 → 排空整理（约 1~3s）→ 状态
   徽章变「正在生成纪要…」→ 逐批调用 LLM（控制台打印每批字数）→ 纪要面板
   展示结构化纪要（【要点】【行动项】【待办】分节）。
4. 点「▶ 开始识别」：驱动线程重建管线（片段清空）重新识别；再停止可再次
   生成新会话纪要。
5. 单独验证 engine：`cargo +stable-x86_64-pc-windows-gnu test --manifest-path
   engine\Cargo.toml`（36 个测试全绿：原 23 + 分批边界 12 + 在途回归 1）。
6. 单独验证前端：`pnpm build`（tsc --noEmit && vite build）通过。

## 已知限制

- **本机未编译 src-tauri**：本机 MSVC 缺 link.exe，src-tauri 依赖 tauri 全家桶
  （原生链接），无法在本机 `cargo build` 验证；`llm.rs` / `pipeline.rs` 按
  类型与 T9 既有 API 形状编写，需在具备完整工具链的环境编译验证（同 T9）。
- 分批是**按字符数**的时间窗切分（每批 ≤500 字），不是严格时间区间；滚动
  上文取上一批正文末尾 ≤100 字，长度口径与 token 估算见 `minutes.rs` 常量
  注释。
- 单批纪要失败（重试 3 次后）回退该批原文、汇总失败拼接各批部分纪要，
  尽力保证 `MinutesReady` 总有内容；未配置 LLM 时整理与纪要都会走「重试
  3 次 → 回退原文/拼接」路径。
- 多会话管理为 MVP 简化：再次「开始识别」重建管线（清空上一会话片段），
  历史纪要仅保留最近一次 `minutesReady`（完整会话历史与导出属 T11）。
- 驱动线程使用合成转写（`MockAsrPort::demo()`），未接真实麦克风（T4）；
  排空等待一次防抖（≤2s），与正常演示节奏一致。
