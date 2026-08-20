# T5 实现总结 — 说话人切换检测（SCD）

> Issue: [#6](https://github.com/smallmj/talksee/issues/6)（OPEN）
> 分支: `feat/t5-scd`（基于 main = b4c8d1c，PR #16 已 rebase 到最新 main）
> 核心实现：`engine/src/scd.rs`（说话人模板 + speaker embedding 余弦匹配 + 自动编号 + 性别降级决策）
> 方案对齐：[ADR-0002](docs/adr/0002-local-streaming-asr-and-self-built-scd.md)（VAD 切句 +
> speaker embedding 余弦匹配，不做全自动在线 diarization；系统只分组、不认人）
> 术语：说话人向量一律称 **speaker embedding**（对齐 ADR-0002；不叫「声纹」，
> 见 CONTEXT.md 的 Avoid「声纹识别」）。

## 一、改动文件

| 文件 | 改动 |
|------|------|
| `engine/src/scd.rs` | **新增**。SCD 核心：`ScdConfig`（余弦阈值 0.75 / 最短有效发言 2 字）、`SpeakerTemplate`（id/embedding/gender/update_count）、`Scd` 状态机（`process_utterance` / `assign_speaker` / `update_template` / `infer_gender` / `assign_for_utterance`）、纯函数 `cosine_similarity`，13 个单元测试 |
| `engine/src/lib.rs` | 导出 `scd` 模块（`Scd`/`ScdConfig`/`SpeakerDecision`/`SpeakerTemplate`/`cosine_similarity`） |
| `engine/src/types.rs` | `Utterance` 新增可选字段 `is_new_speaker: Option<bool>`（serde 默认 None 且缺省省略，JSON 契约不变）：SCD 判定结果随转写流入 Engine，避免壳层二次推导 |
| `engine/src/pipeline.rs` | `emit_for_utterance` 优先采用 `Utterance.is_new_speaker`（SCD 结果），`None` 时退回 `known_speakers` 推导（mock/降级路径）；新增测试 `scd_is_new_flag_overrides_known_speakers` |
| `src-tauri/src/asr.rs` | `SherpaAsr` 的 stdout 读线程持有 `Arc<Mutex<Scd>>`；`read_stdout` 在 final 解析点做双轨判定：带 `embedding` → SCD 余弦匹配（speaker_id/gender/is_new 全部来自 SCD）；无 embedding → 降级单说话人（speaker_id=1，注释明确）；新增 speaker embedding 模型目录解析（`SHERPA_EMBEDDING_MODEL_DIR` 环境变量或 `asr-models/` 自动探测）与 `scd_configured`/`scd_embedding_active` 状态 |
| `src-tauri/src/pipeline.rs` | 真实 ASR 路径日志区分 SCD 模式（speaker embedding 模型已配置/已确认加载 vs 降级单说话人）；模块注释说明 T5 接线位置 |
| `src-tauri/sherpa_streaming.py` | 新增 `--embedding-model-dir`（可选）：加载 sherpa-onnx speaker embedding 提取器（3d-speaker 等），对每个 final 段音频（截尾静音后）提取 speaker embedding，在 `final` 事件附 `embedding` 字段；`started` 事件上报 `scd_embedding` 是否可用；模型缺失/加载失败全部优雅降级 |
| （前端） | **无改动**：`isNewSpeaker` 的渲染处理（已登记说话人不覆盖 → 颜色/头像长会话稳定，规格用户故事 37）为 T2/T9 既有逻辑（`DualTrackView` 的 `reconcileSpeakerColors`/`reconcileSegments`）；T5 仅在 engine 层填充 `Utterance.is_new_speaker` 并随 `SpeakerAssigned` 事件透传 |

## 二、验收标准逐条对照

| 验收标准 | 实现 | 证明 |
|----------|------|------|
| ① VAD 把语音切成句/段 | 由 sidecar 的端点检测承担（`StreamingRecognizer.maybe_finalize`，`enable_endpoint_detection` + 静音规则），每条 `final` 即一个句/段；SCD 消费「final + 该段音频 embedding」 | `src-tauri/sherpa_streaming.py`（T4 既有机制，注释已说明） |
| ② 每段提取说话人向量并与已注册模板余弦匹配 | `Scd::assign_speaker`：对所有已注册模板取最高余弦相似度（`cosine_similarity` 纯函数）；sidecar 提供真实 embedding 时即真实向量 | `engine/src/scd.rs` + 测试 `cosine_similarity_basics` / `threshold_boundary_join_or_new` / `threshold_decision_is_self_consistent` |
| ③ 同一人后续发言归入同一说话人；换人时新建 | 最高相似度 **≥ 阈值（0.75）→ 归入现有**；**< 阈值 → 新建**（id 自动递增）；近似/带噪向量仍归同一人；过短发言（< 2 字）与无 speaker embedding（空/全零向量）**不新建**（噪声保护，沿用最近说话人） | 测试 `same_speaker_subsequent_utterances_join_same_id` / `near_identical_embedding_joins_same_speaker` / `different_speakers_get_incremental_ids` / `short_speech_does_not_create_speaker` |
| ④ 说话人自动编号 + **性别推断（T5 明确降级）** | 编号：新建时 `next_speaker_id` 递增（说话人 1/2/3…）；性别：**T5 明确决策为降级 `Gender::Unknown`**（非临时 stub）——MVP 阶段无真实性别分类模型，基于 embedding 维度/基频的简单启发精度不可靠（实测噪声大，误判反而破坏头像一致性），故不实现低精度推断；`gender_hint`（sidecar 上报 / **T6 用户手动指定**）优先于降级值，接入点已预留 | 测试 `different_speakers_get_incremental_ids` / `infer_gender_degrades_to_unknown_without_model` / `gender_hint_sets_template_gender`；`DualTrackView.tsx`（reconcileSpeakerColors，已登记说话人绝不覆盖） |
| ⑤ 长时间会话中说话人颜色稳定 | 颜色由 `speaker_color(speaker_id)` 8 色调色板取模稳定映射；SCD 保证同一人 speaker_id 恒定 → 颜色不跳变；模板移动平均更新只增强匹配、不改变 id | 测试 `long_session_colors_are_stable`（100 次交替发言 id 恒定 + 颜色 50 次断言）+ 既有 `speaker_colors_are_stable_and_distinct` |

测试结果：`cargo +stable-x86_64-pc-windows-gnu test --manifest-path engine\Cargo.toml` → **全部通过**（含 SCD 测试 + Engine 集成测试 `scd_is_new_flag_overrides_known_speakers` + T9 既有测试）。
前端构建：`pnpm build`（tsc --noEmit && vite build）通过，无回归。

## 三、说话人 speaker embedding 模型如何配置（真实 embedding 路径）

1. 下载 sherpa-onnx 生态的 speaker embedding 模型（如 3d-speaker eres2net 系列，
   仓库含 `*.onnx`），放入 `src-tauri/asr-models/<模型目录>/`；
2. 两种指定方式（二选一）：
   - 自动探测：`asr-models/` 下文件名含 `3dspeaker`/`speaker`/`embedding` 且后缀
     `.onnx` 的目录会被自动选中（`SherpaAsr::resolve_embedding_model_dir`）；
   - 显式指定：设置环境变量 `SHERPA_EMBEDDING_MODEL_DIR=<目录>`；
3. 启动应用：sidecar 收到 `--embedding-model-dir` 后加载提取器，每条 `final`
   事件附带 `embedding` 字段，`started` 事件上报 `scd_embedding: true`；
   Rust 端 `read_stdout` 经 `Scd::process_utterance` 余弦匹配决定 speaker_id；
4. 控制台可见 `[asr] SCD: speaker embedding 模式（模型 …）` 与
   `[engine] SCD: speaker embedding 模型已配置，sidecar 已确认加载…` 日志。

> 注意：本机未下载 speaker embedding 模型、也无法编译 src-tauri（缺 MSVC link.exe / GNU gcc），
> 因此真实 embedding 路径为「代码完整、需模型文件」形态，未实机验证；
> sidecar 的 sherpa-onnx Python 绑定接口按官方 speaker embedding 示例编写，
> 全部调用包在 try/except 内，API 差异只会导致该段降级，不影响识别主流程。

## 四、降级说明

| 场景 | 行为 |
|------|------|
| 未配置 speaker embedding 模型（无 `--embedding-model-dir`） | final 无 `embedding` 字段 → `read_stdout` 降级为单说话人：speaker_id=1、gender=Unknown、`is_new_speaker=None`（由 Engine 按已见说话人推导，即首个 true、之后 false）——保持 T4 行为，注释明确「未配置 embedding 模型，SCD 降级为单说话人」 |
| 配置了模型但加载失败 / 提取失败 | sidecar `available=False` 或该段返回 None → 不输出 embedding → Rust 端同上降级；不崩溃 |
| final 文本过短（< `min_speech_chars`，默认 2 字） | 视为语气词/噪声：不新建说话人、不更新模板，沿用最近说话人（首个过短句归说话人 1） |
| embedding 为空/全零/含 NaN | 视为无 speaker embedding 信号，沿用最近说话人（`Scd::is_empty_signal` 含 NaN 检测；`cosine_similarity` 对 NaN/Inf/零模长返回 0；`read_stdout` 对非有限分量整段降级） |
| 音色选性别 | **T5 明确决策**：无性别分类模型时 `infer_gender` 返回 `Unknown`（降级为正式决策并文档化，非临时 stub——MVP 阶段低精度推断不可靠）；`gender_hint` 入口已预留（sidecar 上报或 T6 用户手动指定性别覆盖） |

## 五、设计决策与已知限制

- **SCD 状态放哪**：main 分支真实 ASR 走 `SherpaAsr → Engine` 直连（无 T9 的
  `append_utterance` 壳层封装），因此 SCD 放在 `SherpaAsr::read_stdout`（final
  解析点，也是 embedding 的唯一来源处），等价于「append 前先经 SCD 决定
  speaker_id」；`is_new_speaker` 经 `Utterance` 新增可选字段流入 Engine，
  不再由壳层用 `known_speakers` 二次推导。
- **阈值口径**：`cosine_threshold` 默认 0.75（ADR 口径：≥ 归入 / < 新建）；
  T5 为内部常量，T12 设置系统再做 UI 调节。
- **模板更新**：移动平均 `avg = (avg·n + new)/(n+1)`；新建时模板用首条向量
  初始化（update_count=1），后续有效发言逐条并入。
- **已知限制**（沿 ADR-0002）：重叠说话时 ASR 与 SCD 均退化；系统只能分组
  不能自动认人（手动命名属 T6）；性别推断为 T5 明确决策的降级（Unknown，
  T6 手动指定）；speaker embedding 模型路径未实机验证。

## 六、验证记录

- `cargo +stable-x86_64-pc-windows-gnu test --manifest-path engine\Cargo.toml` → 全部通过（exit 0）
- `cargo +stable-x86_64-pc-windows-gnu check --manifest-path src-tauri\Cargo.toml` → 零错误
- `pnpm build` → tsc + vite build 通过
- `python -m py_compile src-tauri/sherpa_streaming.py` → 通过
- src-tauri（Rust 壳）完整链接未做：本机缺 MSVC link.exe / GNU gcc+dlltool，
  `cargo check` 已验证类型/引用正确性（含 PR #16 审查指出的引用路径修复）。
