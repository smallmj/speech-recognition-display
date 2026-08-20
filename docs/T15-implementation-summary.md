# T15 实现总结 — 修复 SCD 幻影说话人（短句/噪声下每条 final 都新建说话人）

> 现象：接入 speaker embedding 模型后，1 人测试被分成 4 个说话人、2 人测试被分成 12 个
> 说话人；**每条 final 都新建一个说话人**。
> 分支: `codex/fix-scd-phantom-speakers`
> 核心实现：`engine/src/scd.rs`（三段式判定：时长门槛 + 时长自适应阈值 + 证据确认）
> 关联：T5（PR #16）、PR #22（接入真实 embedding 模型）

## 根因（实测校准，见引擎模块注释）

eres2net / eres2netv2 在**句子级短段**上的同一人余弦远低于固定的 0.75 阈值：

| 条件（不重叠窗口，eres2net-base） | 同一人余弦均值[范围] | 跨人余弦均值[范围] |
|---|---|---|
| 干净 2s | 0.51 [0.09, 0.75] | 0.14 [–0.15, 0.68] |
| 干净 3s | 0.64 [0.56, 0.75] | 0.10 [–0.15, 0.57] |
| 噪声 20dB 2s | 0.43 [0.04, 0.60] | 0.08 [–0.22, 0.60] |

0.75 阈值比模型真实产出高出一倍以上 → 任何真实 final 都匹配不上已有模板 → 每次新建。
短段（<1.5s）embedding 近乎随机，单段判定在噪声下同人/跨人范围重叠，无法可靠区分。

## 修复：三个协同机制（`engine/src/scd.rs`）

1. **时长自适应阈值 + 相对最近邻**：阈值按有效语音时长分档（≥2.5s: 0.60；1.0–2.5s: 0.48），
   多模板时要求 top1 比 top2 高出 margin（0.08）才归入（相对信号在短段上仍有效）。
2. **时长门槛**：有效语音 < 1.0s 不做 embedding 判定，沿用最近说话人（绝不新建）。
3. **新说话人证据确认 + 追溯修正**：新说话人只能由「单段 ≥2.0s 且远离所有模板」或
   「两个互相印证（互相余弦 ≥0.38）的短段」产生；印证时把前一个 pending 短段**追溯修正**
   到新说话人（新事件 `SpeakerCorrected`，前端据此更新已渲染气泡）。

同时升级 embedding 模型为 **ERes2NetV2**（`speech_eres2netv2_sv_zh-cn_16k-common`，
192 维，短语音优化；本机导出 ONNX，见下方模型说明），并让模型解析优先选它。

验证（真实 eres2netv2 embedding + SNR15–20dB 噪声，固定种子，Rust `Scd`）：
- 单人短句 → **1 说话人 100%**；
- 两人 1.5–2.5s 快速交替 → **2 说话人 ~95%**（回归 fixture 14 条全对）；
- 两人 0.9–1.6s 极短轮换 → 2 说话人 ~62–70%（首/末短句无法自证，暂挂前一说话人——模型下限）。

## 改动文件

| 文件 | 改动 |
|------|------|
| `engine/src/scd.rs` | 重写判定：`ScdConfig` 增加 `min_speech_seconds` / `long_seconds` / `match_threshold_long|short` / `match_margin` / `new_speaker_threshold` / `new_speaker_min_seconds` / `confirm_threshold` / `pending_max_span`；`process_utterance(utt_token, text, embedding, speech_seconds, gender_hint)`；`SpeakerDecision` 增加 `corrections`（追溯修正）；`SpeakerCorrection{utt_token,new_speaker_id}`；单元测试 13→ 更新/新增（含 1 人短句不幻影、2 人交替分 2 人+修正） |
| `engine/src/types.rs` | `Utterance` 增加 `utt_seq: Option<u64>`（SCD 修正引用）；`EngineEvent` 增加 `SpeakerCorrected` |
| `src-tauri/sherpa_streaming.py` | final 事件增加 `speech_duration`（有效语音秒数，裁静音后） |
| `src-tauri/src/asr.rs` | 解析 `speech_duration`；`read_stdout` 维护 `utt_seq` 与 `CorrectionState`（托管状态：`utt_seq→segment_id` + 修正队列）；模型解析优先 `eres2netv2` |
| `src-tauri/src/pipeline.rs` | `append_utterance` 记录 `utt_seq→segment_id`；`flush_corrections` 把修正队列解析成 `SpeakerCorrected` 事件并登记 `known_speakers` |
| `src-tauri/src/lib.rs` | `app.manage(asr::CorrectionState::default())` |
| `src/engineEvents.ts` / `src/components/DualTrackView.tsx` | 新增 `speakerCorrected` 事件：更新已渲染片段的 `speakerId` 并登记新说话人颜色 |
| `src-tauri/examples/scd_emit.rs` | 解析 `speech_duration`、输出 `corrections` |
| `scripts/check-scd-embedding.mjs` | 断言升级：192 维、两位真实说话人（0.wav=A，1/2/3.wav=B——旧「4 个 wav=4 说话人」前提本身是错的）、新增短句交替 fixture 断言 2 说话人 |
| `scripts/gen_scd_short_alt_fixture.py` | **新增**：生成两人快速交替 + 房间噪声的 final NDJSON（真实 embedding，固定种子） |

## 模型说明（ERes2NetV2）

- 模型：`iic/speech_eres2netv2_sv_zh-cn_16k-common`（ModelScope），由 sherpa-onnx
  `scripts/3dspeaker/export-onnx.py` 导出为 ONNX（192 维，动态时长），本机导出后放入
  `src-tauri/asr-models/sherpa-onnx-3dspeaker-eres2netv2-base/`。
- 模型文件（~68MB）gitignore（`asr-models/` 惯例：模型不提交）；换机需重新下载/导出。
- 解析优先级：`eres2netv2 > campplus > 其他`；`SHERPA_EMBEDDING_MODEL_DIR` 显式指定仍优先。

## 验证

- `cargo test`：engine 59 passed + src-tauri 45 passed，0 failed。
- `cargo check` / `cargo build --example scd_emit` 通过；`tsc --noEmit` 通过。
- `node scripts/check-scd-embedding.mjs`：6 项全 PASS（含短句交替 2 说话人、归属正确）。
- 修复前该 fixture 走旧逻辑（0.75 阈值）会 14 条 → 14 说话人（即用户报告的现象）。
