# T9/T10 整合到 main 的 rebase 记录

> 记录把 `feat/t9-real-llm`（T9）与 `feat/t10-minutes`（T10）两个本地功能分支
> rebase 到已大幅推进的 `main`（T3 显示定制 + T4 真实 ASR + T3 置顶大字修复）
> 上的架构级冲突整合决策、改动清单、验证结果与已知限制。

## 1. 背景与分支拓扑

- `origin/main` = `b4c8d1c`：含 T3（显示定制）+ T4（真实麦克风 + sherpa-onnx ASR）
  + b4c8d1c（T3 置顶大字无退出途径修复：全局 ESC + 浮动退出按钮 + 不变量检查脚本）。
  *注意：任务简报写的是 `8243196`，实际 rebase 期间 owner 又推送了 T3 修复
  （`b4c8d1c`），按「rebase 到**最新** origin/main」的目标以 `b4c8d1c` 为基底。*
- `feat/t9-real-llm`：T9 真实 LLM 接入（PR #15），原 tip `ee8fe37`。
- `feat/t10-minutes`：T10 会议纪要（基于 T9），本地 tip `c5012b4`；
  远程 tip 是 `f65b5ca`（基于**未含 T9 修复**的 `350882a`，owner 更早推送的旧版本），
  以本地 `c5012b4`（含 T9 修复）为准重写。

### 最终 commit

| 分支 | commit | 说明 |
|---|---|---|
| `feat/t9-real-llm` | `375f163` | feat(T9) 整合版：T3 + T4 + T9 共存（真实 ASR → 整理管线 → 真实 LLM） |
| `feat/t9-real-llm` | `1c6dac2` | fix(T9)：异步成功路径释放单在途（原 `ee8fe37` 内容） |
| `feat/t9-real-llm` | `bb36f01` | fix(T9)：按 PR #15 审查意见修复（editId 乱序防御 / LlmPort trait 化 / 500 字窗口 / 命名/词汇/死代码，见 §3） |
| `feat/t10-minutes` | `e49cf3e` | feat(T10) 整合版：会话控制 + 分批纪要叠加于修复后的 T9；纪要经 `LlmPort::summarize_streaming` 走同一 trait |
| `feat/t10-minutes` | `f1f8e60` | docs：本 rebase 记录（含 §3 审查修复记录） |
| 基底 | `b4c8d1c` | 最新 origin/main（含 T3 修复） |

## 2. 整合决策

### 2.1 `src-tauri/src/pipeline.rs` —— 数据流合并（最核心）

三边意图（T4 / T9 / T10）合并为一个驱动线程 `spawn_engine_emitter`：

```text
麦克风 (cpal) → sherpa-onnx sidecar ── partial → emit partialResult（实时状态行，T4）
                │                        └ final → SherpaAsr.finals 队列
                │（spawn 失败 → MockAsrPort::demo() 合成转写，emit engine://status mode=mock）
                ▼
        ┌─────────────────────────────┐
        │ CleanupPipeline（T9，逻辑时钟）│
        │  append(now, speaker_id, raw)│
        │  tick → 冻结 → pending（单在途）│
        └──────────────┬──────────────┘
                       ▼
        LlmPort::cleanup_streaming（SSE 流式，经 Box<dyn LlmPort> 调用）
           │ 每个 delta → emit segmentCleaning{editId, partial}（逐字填充整理版）
           ├ 成功 → apply_cleanup_result → emit segmentCleaned
           └ 失败 → fail_pending（重试 3 次后）→ emit cleanupFailed（回退原文）
           （配置了 API Key → OpenAiLlmClient；未配置 → MockLlmPort 降级，ASR 不受影响）

        T10 会话控制：SessionControl（stop_requested / session_active 原子标志）
           stop_session 命令 → run_stop_flow：
             冻结剩余 active → 排空整理队列（在途结果照常回填）
             → emit sessionStopped → engine::minutes::chunk_for_summarize 分批
             → 逐批 LlmPort::summarize_streaming → 汇总 → emit minutesReady
           start_session 命令 → 上升沿重建管线 → emit sessionStarted
```

关键决策：

1. **final 不再进 `engine::Engine`**（T4 的 `Engine::new(Box::new(real))` 移除），
   改为直接喂 `CleanupPipeline`（T9 做法）——真实 ASR 的 final 与合成转写走同一
   整理链路。`engine::Engine` 保留在 engine crate 内，壳层不再使用。
2. **真实模式与演示模式的追加节奏不同**：
   - 真实模式：每拍**排空** `next_utterance()` 全部 final 无条件 append（**不丢字**，
     LLM 在途时 final 只会积压不会丢弃）；
   - 演示模式：保留 T9 的「1 进 1 出」门控（无在途请求且上一段已落库才追加下一条），
     便于观察完整演示流程。
3. **partial 实时显示保留**（T4）：真实模式下每拍轮询 partial 队列，publish 最新一条
   为 `PartialResult` 事件；前端 `AsrLiveRow` 状态行展示（2s 无更新回到「聆听中」）。
4. **统一节拍 200ms**（原 T4：真实 200ms / 演示 800ms；T9：150ms）：partial 刷新
   及时，且 CleanupPipeline 的防抖（2s）/固定节奏（5s）由逻辑时钟驱动，与节拍无关。
5. **说话人**：T4 无 SCD，真实 final 全部归说话人 1、性别 Unknown；沿用 T9 的
   `SpeakerAssigned`（颜色/性别）在 `SegmentAppended` 前 emit 的顺序。
6. **会话停止期间的缓冲处理**（真实模式新增）：停止分支（`run_stop_flow`，可能耗时
   数秒做 LLM 汇总）期间麦克风仍在采集，final/partial 会在 sidecar 队列堆积；
   驱动线程在「等待开始」分支**排空**这些堆积（丢弃，不属于新会话），防止无界增长
   与新旧会话串扰；重新开始时真实模式**不重启** sidecar（重启开销大），演示模式
   重建 `MockAsrPort::demo()`。
7. **停止分支**（T10 `run_stop_flow` 原样搬入）：冻结全部 active → 排空整理队列
   （在途 LLM 结果照常经 `apply_cleanup_result`/`fail_pending` 回填，纪要尽可能基于
   整理版）→ `sessionStopped` → `chunk_for_summarize` 分批（≤500 字 + 滚动上文）→
   逐批 `LlmPort::summarize_streaming`（每批一个请求；单批失败回退该批原文；≥2 批
   再汇总一次，汇总失败拼接各批部分纪要兜底）→ `minutesReady` → `session_active=false`。
8. **LLM 每次请求前重读配置**（T9 保留）：前端保存 LLM 配置后无需重启生效；
   配置判定在 `current_llm`（见 §3 审查修复 #1/#3）。

### 2.2 `src-tauri/src/lib.rs`

- 模块：`pub mod audio;`（T4）+ `mod asr; mod bridge; mod llm; mod pipeline;`（两边合并）。
- setup：`app.manage(pipeline::SessionControl::new())`（T10）→ `spawn_ping_emitter`
  → `pipeline::spawn_engine_emitter`（整合后的唯一驱动）。
- invoke_handler：`ping_ack` + `llm::load_llm_config` + `llm::save_llm_config`（T9）
  + `pipeline::stop_session` + `pipeline::start_session`（T10）。

### 2.3 `engine`（types / cleanup / lib / minutes）

- `types.rs`：`PartialResult`（main）与 `SegmentCleaning`（T9）都保留，auto-merge 成功。
- `cleanup.rs`：T9 修复（`apply_cleanup_result` 清 pending + 解锁单在途）保留；
  T10 新增 `store_mut()`（停止时冻结剩余 active 用）。
- `lib.rs`：`pub use cleanup::…`（T9）+ `pub use minutes::…`（T10）。
- `minutes.rs`（T10 新增，12 个测试）：分批 + 滚动上文 + 汇总编排，纯函数可测。

### 2.4 前端布局（`src/App.tsx` 及组件）—— T3 + T4 + T9 + T10 共存

主内容区以 **DualTrackView（T9 双轨）** 为主，BubbleFlow 被替换删除：

```text
header: h1 | 事件桥徽章 | engine 徽章 | ASR 模式徽章(T4) | 会话徽章(T10)
        | [开始识别/停止并生成纪要](T10) | 📌置顶大字(T3) | ⚙显示(T3)
main:   LlmConfigPanel(T9) → DualTrackView(双轨，sessionEvents) → AsrLiveRow(T4 partial)
        → MinutesPanel(T10 纪要)
footer: …
浮动:   focusMode 时 ✕退出大字按钮（T3 修复）；全局 ESC（T3 修复）
```

- **DualTrackView**：默认显示 LLM 整理版 + 改动词 diff 高亮，一键切换原文；
  接收 `segmentCleaning` 流式增量（整理中 · 流式…）与 `segmentCleaned`/`cleanupFailed`。
- **AsrLiveRow**（新组件，T4 partial 状态行的独立化）：`partialResult` 边说边出，
  2s 无更新隐藏；演示模式无 partial 不渲染。
- **T3 显示定制接入双轨**：`DualTrack.css` 的 `.dual-text` 改用
  `--bubble-font-size/--bubble-font-family/--bubble-text-color` 变量（displaySettings
  localStorage 持久化即时生效）；置顶大字模式下隐藏 dual-toolbar/dual-status 并放大
  片段文字。
- **T10 会话**：`sessionEvents`（自最近一次 `sessionStarted` 起切片，避免新旧会话
  片段混排）、开始/停止按钮、会话徽章、MinutesPanel（`【要点】/【行动项】/【待办】`
  分节渲染）。
- **两套配置系统不混淆**：显示设置走 localStorage（`displaySettings.ts`）；
  LLM 配置走 Tauri 命令（`llm.rs` 明文 JSON 于 app config 目录）。

### 2.5 `src/engineEvents.ts`

事件契约合并后为：`sessionStarted / sessionStopped / segmentAppended /
speakerAssigned / partialResult(T4) / segmentCleaning(T9) / segmentCleaned /
cleanupFailed / minutesReady`；保留 `STATUS_EVENT` + `StatusPayload`（T4）；
`Segment` 增加客户端侧 `cleaningPartial`（T9）。

### 2.6 `src-tauri/Cargo.toml` 与 `capabilities/default.json`

- Cargo.toml：`cpal 0.18` + `rubato 0.15`（T4）+ `ureq ~2.11, features=["json"]`（T9），
  auto-merge 成功。
- capabilities：保留 main 的 `core:window:allow-set-always-on-top`（T3 已加），
  T9/T10 无额外权限需求。

## 3. PR #15 审查修复记录（owner CHANGES_REQUESTED，8 项）

rebase 完成后按 owner 审查意见逐条落实（`feat/t9-real-llm` 的 `bb36f01`，
`feat/t10-minutes` 在其上继承）：

| # | 审查意见 | 修复实现 | 状态 |
|---|---|---|---|
| 1 | 保留 T4 真实 ASR 优先；LLM 未配置时整理可降级、**绝不降级 ASR** | `SherpaAsr::spawn()` 优先 + mock 回退不变；`pipeline::current_llm`：API Key 为空 → `MockLlmPort`（整理降级），真实/合成 ASR 路径完全不受影响；partial 仍发 `PartialResult` | ✅ |
| 2 | `SegmentCleaning` 带 `edit_id: u64`；前端只接受 `editId >= 当前` | types.rs 变体加 `edit_id`；pipeline.rs 两处 emit 带上 `p.edit_id`；engineEvents.ts 加 `editId` + 客户端 `cleaningEditId`；DualTrackView 拒 `editId < 当前` 的残余增量；序列化测试更新（`editId:5` 断言） | ✅ |
| 3 | `LlmPort` trait 化消除死代码：驱动持 `Box<dyn LlmPort>`，流式方法进 trait（默认实现），engine 测试缝覆盖流式路径 | trait 加 `cleanup_streaming`（默认走 `cleanup` 一次性回调）与 `summarize_streaming`（默认走 `summarize`）；`OpenAiLlmClient` 覆盖两个流式方法（SSE 增量回调）；驱动 `Box<dyn LlmPort>` 调用；engine 新增 2 个默认实现测试（cleanup/summarize 流式回调断言）+ 1 个 mock 流式累积测试 | ✅ |
| 4 | ADR-0003「单次输入 ≤500 字」实施 + 滚动窗口 ≤2000 token 注明 | `OpenAiLlmClient::clip_input_window`：超 `MAX_INPUT_CHARS=500` 按句末标点（。！？…）切分只送首个完整窗口，无标点硬截断；`MAX_INPUT_CHARS` 注释说明 500 字 ≈ 500-700 token < 2000（`minutes::MAX_TOKENS`）；纪要分批口径一致（`BATCH_MAX_CHARS` = 500） | ✅ |
| 5 | `MAX_RETRIES=3`（实为 4 次尝试）命名误导 | 改名 `MAX_ATTEMPTS=4`，循环 `0..MAX_ATTEMPTS`（1 次初始 + 3 次重试），日志「第 N 次重试…共 4 次尝试」 | ✅ |
| 6 | 词汇违规：规范词「原文」，Avoid「转写稿」（llm.rs 注释 / LlmConfigPanel DEFAULT_PERSONA / docs 摘要） | `DEFAULT_PERSONA` 与注释「口语化转写」→「口语化原文」；全仓 grep 无残留（docs 亦核查） | ✅ |
| 7 | `OpenAiLlmClient::config()` getter 无调用方 → 删除 | 已删除（trait impl 直接访问 `self.config`） | ✅ |
| 8 | `DEFAULT_PERSONA` Rust 与前端双重定义 → 加对齐注释、指向权威源 | llm.rs 注明「权威源在本文件，前端 LlmConfigPanel.tsx 需同步」；LlmConfigPanel.tsx 注明「权威源在 Rust 端，改动需两处同步」 | ✅ |

**修复后的验证**：engine 测试 **38 passed**（24 + minutes 12 + 流式默认实现 2，含
`async_apply_cleanup_result` 回归与 editId 序列化断言）；`pnpm build`（tsc + vite）
通过；`check:focus-exit` PASS。src-tauri 仍未本机编译（同 §6 已知限制），Rust 改动
经逐项审查（trait 对象安全、`&mut dyn FnMut` 回调重借用、闭包捕获、借用检查）。

## 4. 改动文件清单

**feat/t9-real-llm（375f163 + 1c6dac2 + bb36f01）**：engine 的 types.rs（事件/端口）、
cleanup.rs、lib.rs（测试）；src-tauri 的 Cargo.toml、llm.rs（新增，含 trait impl +
500 字窗口）、lib.rs、pipeline.rs（含 current_llm 降级）；前端 App.tsx、engineEvents.ts
（editId）、DualTrackView.tsx（editId 乱序防御 + cleaningEditId）、DualTrack.css、
LlmConfigPanel（新增，含人设对齐注释）；AsrLiveRow（新增）；删除 BubbleFlow.tsx；
Cargo.lock；docs/T9-implementation-summary.md。

**feat/t10-minutes（e49cf3e + f1f8e60）**：engine 的 cleanup.rs（store_mut）、lib.rs
（minutes 导出 + summarize_streaming 测试）、minutes.rs（新增，12 测试）；src-tauri
的 llm.rs（chat_once/run_with_retries/MAX_ATTEMPTS/summarize_streaming/MINUTES_PERSONA）、
lib.rs（SessionControl 管理 + stop/start 命令）、pipeline.rs（会话控制 + run_stop_flow
经 trait 调 LLM）；前端 App.tsx（会话状态机 + 按钮 + 徽章）、styles.css（session-controls
样式）、MinutesPanel（新增）；docs/T10-implementation-summary.md、docs/rebase-t9-t10-onto-main.md。

## 5. 验证结果

| 验证项 | 结果 |
|---|---|
| engine 测试（feat/t9-real-llm，含审查修复 bb36f01） | `cargo +stable-x86_64-pc-windows-gnu test`：**25 passed**（含 editId 序列化断言 + 流式默认实现测试） |
| engine 测试（feat/t10-minutes，含审查修复） | **38 passed**（25 + minutes 12 + summarize_streaming 默认实现 1） |
| 前端构建（两个分支） | `pnpm build`（`tsc --noEmit && vite build`）通过，exit 0 |
| T3 不变量检查 | `npm run check:focus-exit` PASS（置顶大字模式存在退出途径） |
| 远程推送 | `feat/t9-real-llm` → `bb36f01`（force-with-lease）；`feat/t10-minutes` → `f1f8e60`（force-with-lease），PR #15 反映最新 T9 + 审查修复 |

## 6. 已知限制

- **src-tauri 未在本机编译**：本机 MSVC 缺 `link.exe`、GNU 缺 `gcc/dlltool`，且
  `ring`/`cpal` 等依赖需要 C 编译器；`src-tauri` 全部 Rust 改动经仔细审查
  （API 签名、生命周期、借用、trait 约束、Tauri command 注册）但**未编译验证**，
  需在具备工具链的机器上 `cargo build` 复核。
- **真实 ASR 链路未实机运行**：sherpa sidecar + 麦克风 + 真实 LLM 的端到端行为
  （partial 节奏、final 入管线、停止排空、纪要生成）未实机验证；代码路径与 T4/T9/T10
  各自已验证的行为一致，但整合后的组合行为需运行确认。
- **统一节拍 200ms**：演示模式下原 T9 为 150ms，现统一 200ms，演示节奏略缓
  （防抖 2s / 节奏 5s 不变，仅逻辑时钟步进粒度变化，不影响整理行为）。
- **死代码**：`src/styles.css` 仍保留 `.bubble-*` 样式（BubbleFlow 已删除，样式未被
  引用）；`DualTrackDemo.tsx` 为 T8 遗留浏览器内演示，未导入。均不影响构建。
- 远程 `feat/t10-minutes` 原 tip `f65b5ca`（基于未含 T9 修复的 350882a）已被覆盖，
  若外部有基于该旧 tip 的引用需注意。
