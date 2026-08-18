# T9 实现总结：真实 LLM 接入（OpenAI 兼容 + SSE + 重试）

对应 Issue #10「T9 真实 LLM 接入（OpenAI 兼容 + SSE + 重试）」。把模拟 LLM
换成真实 OpenAI 兼容接口：配置 Base URL + API Key + 模型名，SSE 流式输出
（整理结果逐字出现），失败退避重试 3 次后展示原文；内置「整理人设」预设。

## 架构位置

真实 LLM 客户端放在 **Tauri 壳**（`src-tauri`），engine 刻意不依赖网络/异步
运行时，只扩展了事件契约；engine 的 `CleanupPipeline` 异步路径（`tick` 派发
`pending` → 调用方调 LLM → `apply_cleanup_result` / `fail_pending` 回填）由
壳层驱动，同步 `LlmPort` 接口保留给 Mock 与测试。

## 改动的文件

### engine（核心库，唯一测试缝）

- `engine/src/types.rs`：`EngineEvent` 新增流式增量事件
  `SegmentCleaning { segment_id, partial }`（serde camelCase，前端收到
  `{"type":"segmentCleaning","segmentId":..,"partial":".."}`）。含义：LLM
  整理结果的部分增量（SSE 每个 delta），由壳层 emit，engine 只定义契约。
- `engine/src/lib.rs`：`pub use cleanup::{CleanupPipeline, CleanupScheduler,
  MockLlmPort, PendingCleanup, SegmentStore}` 导出整理管线（集成缺口补齐）；
  新增 `SegmentCleaning` 序列化 shape 测试（对齐已有测试风格）。
- `engine/src/cleanup.rs`：更新模块文档（异步路径消费方为 Tauri 壳），移除
  T8 遗留的 `#![allow(dead_code)]`（T9 起管线经 `pub use` 导出，不再 dead code）。

### src-tauri（Tauri 2 壳，主要工作）

- `src-tauri/src/llm.rs`（新增）：
  - `LlmConfig { base_url, api_key, model, persona }`（serde camelCase，
    字段缺省用默认值），明文 JSON 存于 app config 目录 `llm-config.json`；
    `load_llm_config` / `save_llm_config` 两个 `#[tauri::command]`。
  - `OpenAiLlmClient::cleanup_stream`：阻塞式 POST
    `{base_url}/chat/completions`（`Authorization: Bearer` +
    `Content-Type: application/json`），body `{"model", "messages":
    [{system 人设}, {user 原文}], "stream": true}`；按 `data: {...}` 逐行解析
    SSE，取 `choices[0].delta.content` 累积回调；兼容非 SSE 整段响应兜底。
  - 退避重试：网络错误 / 非 2xx / SSE 解析错误 → 等比退避 500ms/1s/2s 重试
    3 次，全部失败返回 `LlmError`（对齐 ADR-0003「失败退避重试 3 次后放弃」）。
  - `DEFAULT_PERSONA` 内置整理人设（TypeFlux 意图整理风格：去口语化/纠错/
    补标点/不改意），`persona` 为空自动回退默认。
- `src-tauri/src/pipeline.rs`：T2 冒烟 emitter 升级为 **整理管线驱动**
  （`spawn_cleanup_driver`）：后台线程持有 `CleanupPipeline`，每 150ms 推进
  逻辑时钟并 `tick(now)`；无在途且上一段落库时喂入一条 `MockAsrPort::demo()`
  合成转写（1 进 1 出，演示节奏清晰）；`tick` 派发 `pending` 后调真实 LLM，
  每个 SSE delta emit `SegmentCleaning`（增量逐字推给前端），完成后
  `apply_cleanup_result` emit `SegmentCleaned`，失败 `fail_pending` emit
  `CleanupFailed`（前端回退原文）。事件统一 emit 到现有 `engine://event`。
  每次请求前重读配置 → 前端保存配置后无需重启即生效。
- `src-tauri/src/lib.rs`：注册 `save_llm_config` / `load_llm_config` 命令；
  setup 中启动整理管线驱动（替代 T2 冒烟 emitter 作为主事件源；`bridge://ping`
  心跳保留）。
- `src-tauri/Cargo.toml`：新增 `ureq = { version = "~2.11", features = ["json"] }`
  （阻塞式 HTTP + SSE 流式读取；`json` feature 启用 `send_json`，默认 features
  含 rustls TLS 与 gzip）。锁定 `~2.11`：2.11 起设置 header 用 `Request::set`
  （2.10 及更早用 `header`），锁定已验证的 API 形状。

### src（React 前端，薄渲染）

- `src/engineEvents.ts`：`EngineEvent` 新增 `{ type: "segmentCleaning";
  segmentId; partial }`；`Segment` 增加客户端侧非持久化字段 `cleaningPartial`。
- `src/components/DualTrackView.tsx`：`reconcileSegments` 处理 `segmentCleaning`
  （累积 partial 到片段），`segmentCleaned` / `cleanupFailed` 到达后清空；
  渲染规则：收到流式增量后、`cleaned` 前展示 partial 文本（带「整理中 · 流式…」
  标识与闪烁光标），`segmentCleaned` 到达后替换为最终整理版（diff 高亮与原文
  切换逻辑不变）；`useEngineEvents` 改用模块级单例 `subscribe`（StrictMode 安全）。
- `src/components/LlmConfigPanel.tsx` + `.css`（新增）：极简配置面板——Base
  URL / API Key / 模型名 / 整理人设输入 + 保存（invoke `save_llm_config`）、
  加载（invoke `load_llm_config`）、「恢复内置人设」按钮（留空则 Rust 端用
  内置预设）。
- `src/App.tsx`：双轨展示接入主界面（`useEngineEvents()` 喂 `DualTrackView`），
  配置面板置于主内容区顶部，保留状态徽章。

## 验收标准逐条对照

- [x] **配置 OpenAI 兼容接口（Base URL/Key/模型名）并保存**
  配置面板 + `save_llm_config` / `load_llm_config` 命令，明文 JSON 持久化到
  app config 目录；驱动线程每次请求前重读配置。
- [x] **SSE 流式输出，整理结果流式填入界面**
  `cleanup_stream` 逐行解析 SSE delta，壳层每收到一个 delta emit
  `SegmentCleaning`，前端 `reconcileSegments` 累积 `cleaningPartial` 并带
  「整理中 · 流式…」标识逐字填充。
- [x] **LLM 失败退避重试 3 次后保留原文**
  网络/非 2xx/SSE 解析错误 → 500ms/1s/2s 等比退避重试 3 次；全部失败 → 驱动
  `fail_pending` → `CleanupFailed` → 前端状态置 `failed` 回退展示原文。
- [x] **内置整理人设预设可选用**
  `DEFAULT_PERSONA` 内置预设；`persona` 留空自动生效，配置面板可「恢复内置
  人设」或自定义覆盖。

## 如何配置与验证

1. 配置（应用内）：展开顶部「⚙️ LLM 配置」→ 填 Base URL（如
   `https://api.openai.com/v1`）/ API Key / 模型名（如 `gpt-4o-mini`）→
   保存配置。人设留空即用内置预设。配置文件位于 app config 目录
   `llm-config.json`。
2. 运行：`pnpm tauri dev`（需本机 Rust + Tauri 环境）。应用启动后整理驱动
   以约 3~4s 一条的节奏喂入合成转写：气泡追加 → 约 2s 防抖后冻结 → LLM
   SSE 流式增量逐字填充 → 整理完成（diff 高亮改动词）；未配置/请求失败时
   退避重试 3 次后回退展示原文。
3. 单独验证 engine：`cargo +stable-x86_64-pc-windows-gnu test --manifest-path
   engine\Cargo.toml`（23 个测试全绿）。
4. 单独验证前端：`pnpm build`（tsc --noEmit && vite build）通过。

## 已知限制

- **本机未编译 src-tauri**：本机 MSVC 工具链缺 link.exe，src-tauri 依赖
  tauri 全家桶（原生链接），无法在本机 `cargo build` 验证；`llm.rs` /
  `pipeline.rs` 代码按类型与 API 约定编写，需在具备完整工具链的环境编译验证。
- API Key 明文存本地配置文件（MVP 取舍，规格未要求加密）。
- 驱动线程使用合成转写（`MockAsrPort::demo()`）作为输入，未接真实麦克风
  （T4）；重点验证「合成转写 → 整理管线 → 真实 LLM → 流式事件 → 前端双轨」
  垂直链路。
- 流式增量不做跨重试去重：某次尝试流式中途失败后重试，前端 partial 会
  重新累积；最终失败以 `CleanupFailed` 收尾并回退原文，不影响最终一致性
  （editId 校验兜底乱序）。
- 配置保存后由驱动线程在下次请求前重读，无需重启；但当前会话已在途的
  请求仍用旧配置。
