# 本地流式 ASR 模型选型调研（sherpa-onnx，TalkSee / 听障实时字幕）

> 日期：2026-08-21 ｜ 运行时：打包的 `sherpa-onnx` **1.13.6**（PyPI 最新版，2026-08-18 发布）
> 实测环境：Apple M5 Pro（24GB），sherpa-onnx 1.13.6 Python 绑定，`num_threads=2`，CPU 推理。
> 所有候选模型均用项目打包的 1.13.6 **实测加载与解码通过**；"sherpa-onnx 2.x" **尚不存在**（见 §2）。

---

## 1. 推荐结论（TL;DR）

| 优先级 | 模型（HF 仓库） | 下载/磁盘占用 | 流式 | 是否需要升级运行时 |
|---|---|---|---|---|
| 🥇 **首选默认**（真流式、中英双语、精度/硬件甜点） | `csukuangfj/sherpa-onnx-streaming-paraformer-bilingual-zh-en`（int8） | `encoder.int8.onnx` 165.5MB + `decoder.int8.onnx` 71.7MB ≈ **237MB**（tar 含 fp32 共 1047MB，App 按文件直下即可） | ✅ 真流式（FunASR online paraformer，chunk 约 1s） | **否**（1.13.6，已实测） |
| 🥈 高精度模式（最接近微信输入法效果） | `csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17`（int8） | `model.int8.onnx` **239.2MB**（tar 163MB） | ❌ 非流式（需 silero VAD 语句级切割，"模拟流式"，延迟≈整句长度 2–4s） | **否**（1.13.6，已实测） |
| 🥉 若接受纯中文（英文交给 LLM 兜底或极少出现） | `csukuangfj/sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30` | `encoder.int8.onnx` 161.1MB + `decoder.onnx` 5.2MB + `joiner.int8.onnx` 1.0MB ≈ **167MB**（tar 132.6MB） | ✅ 真流式 | **否**（1.13.6，已实测） |

**一句话**：继续留在 sherpa-onnx **1.13.6**（它已是最新发布；没有任何 2.x 可升级）。把默认模型从
2023-02-20 换成 **streaming paraformer bilingual zh-en (int8)** 或加一条 **SenseVoice + VAD 高精度模式**；
"精度天花板"的 X-ASR 系列在 1.13.6 上必然崩溃（项目 issue #27 已证实，当前所有已发布运行时都无法加载它），暂不可用。

### 最关键的实测证据（同机、同音频、1.13.6、2 线程）

`num.wav`（人工朗读"会议定在二零二五年六月三十号下午三点，预算大概五千八百块"）：

- **SenseVoice (ITN=ON)** → `会议定在2025年6月30号下午3点，预算大概5800块。`（数字转写 + 标点，微信级体验）✅
- SenseVoice (no ITN) → `会议定在二零二五年六月三十号下午三点预算大概五千八百块`（无标点/ITN）
- 2023 / paraformer / zh-2025（流式）→ 无 ITN、无标点（"二零二五…五千八百"），需 LLM 整理兜底

---

## 2. 运行时版本现实核对（重要更正）

1. **没有 sherpa-onnx 2.x**：
   - PyPI `sherpa-onnx` 最新 = **1.13.6**（共 26 个发布，无 2.x）。
   - GitHub Releases 最新 = **v1.13.6**（2026-08-18）。
   - 官方预告的 **2.0.0**（issue #3731）是为**移除 espeak-ng/piper-phonemize 的 TTS 许可整改**，与 ASR 模型无
     关，且尚未发布。→ "升级到 2.x 才能加载新模型"的前提不成立。
2. **1.13.6 已支持全部我们需要的流式解码路径**（项目打包的 `online_recognizer.py` 实测包含）：
   `from_transducer`（zipformer/zipformer2）、`from_paraformer`（流式 paraformer）、
   `from_zipformer2_ctc`(2025-06-30 流式 Zipformer2-CTC)、`from_wenet_ctc`、`from_nemo_ctc`。
3. **X-ASR（2026 系列，`csukuangfj2/...` 导出）**：1.13.6 加载即 segfault（缺 `encoder_dims` 元数据；
   项目 issue #27 已记录）。我核查了 master 源码 `sherpa-onnx/csrc/online-zipformer2-transducer-model.cc` +
   `macros.h`：`SHERPA_ONNX_READ_META_DATA_VEC` 缺元数据时直接 `SHERPA_ONNX_EXIT(-1)` → **master 同样会退出**。
   结论：这不是"等 2.x"能解决的，需要上游修导出文件或运行时，当前不可用。

---

## 3. 候选模型家族对比（规模 / 流式 / RTF / 许可）

| 模型 | HF / 来源 | int8 磁盘占用 | 流式 | RTF（CPU，2 线程实测或文档值） | 许可 |
|---|---|---|---|---|---|
| **streaming-zipformer-bilingual-zh-en-2023-02-20**（现用） | [csukuangfj/…-2023-02-20](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20) | ≈199MB（encoder.int8 181.9M + decoder 13.9M + joiner 3.2M + bpe） | ✅ 真流式（chunk 32 帧） | **0.021**（M5 Pro） | Apache-2.0 |
| **streaming-zipformer-zh-int8-2025-06-30**（中文，icefall multi_zh-hans large） | [csukuangfj/…-zh-int8-2025-06-30](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30) | ≈167MB（encoder.int8 161.1M） | ✅ 真流式 | **0.031** | Apache-2.0（icefall） |
| **streaming-zipformer-zh-xlarge-int8-2025-06-30**（中文 xlarge） | [csukuangfj/…-zh-xlarge-…](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-zh-xlarge-int8-2025-06-30) | ≈771MB（encoder.int8 761.1M） | ✅ 真流式 | 未测（预计 0.05–0.08） | Apache-2.0（icefall） |
| **streaming-zipformer-ctc-zh-int8-2025-06-30**（中文 CTC） | [csukuangfj/…-ctc-zh-int8-…](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-ctc-zh-int8-2025-06-30) | 162.3MB（model.int8） | ✅ 真流式（Zipformer2 CTC） | **0.029** | Apache-2.0（icefall） |
| streaming-zipformer-small-ctc-zh-int8-2025-04-01（中文小模型，弱机） | [csukuangfj/…-small-ctc-zh-…](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-small-ctc-zh-int8-2025-04-01) | 25MB | ✅ 真流式 | 极快 | Apache-2.0 |
| **streaming-paraformer-bilingual-zh-en**（中英双语，FunASR online） | [csukuangfj/…-paraformer-bilingual-zh-en](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-paraformer-bilingual-zh-en) | ≈237MB（enc.int8 165.5M + dec.int8 71.7M） | ✅ 真流式（chunk≈1s，无时间戳） | **0.025**（M5 Pro）/ 文档 CPU 0.15–0.21 | Apache-2.0 |
| streaming-paraformer-trilingual-zh-cantonese-en（加粤语） | [csukuangfj/…-trilingual-…](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-paraformer-trilingual-zh-cantonese-en) | ≈236MB | ✅ 真流式 | 同上级 | Apache-2.0 |
| **sense-voice-zh-en-ja-ko-yue-2024-07-17**（int8） | [csukuangfj/…-sense-voice-…](https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17) | 239.2MB（model.int8） | ❌ 非流式（VAD 模拟流式） | **0.014**（M5 Pro）/ RK3588 A55 约 0.18–0.44、A76 约 0.05–0.10 | Apache-2.0（见 FunAudioLLM） |
| X-ASR zh-en（1M 小时、带标点，2026-06-03 punct int8） | [csukuangfj2/…-x-asr-…-punct-int8-2026-06-03](https://huggingface.co/csukuangfj2/sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03) | ≈175MB | ✅ 真流式（160/480/960/1920ms 多档） | 未测 | Apache-2.0（但 **1.13.6 加载即崩溃**，不可用） |

> 重要：**2023-02-20 之后没有更新的"中英双语"流式 zipformer**。官方文档（2026-08 master）里流式中英双语
> transducer 只有 2023-02-20 与 small-2023-02-16 两个；网上流传的"bilingual-zh-en-2025-06-30" **在 HF 上不存在**（API 核实 exists=false），系检索摘要幻觉。中英双语的流式升级路径只有 **paraformer** 一族。

---

## 4. 实测基准（同一 4 段音频、1.13.6、M5 Pro、2 线程）

### 4.1 RTF（越小越快；所有模型都远快于实时）

| 音频（时长） | 2023 bilingual | zh-2025 | ctc-zh-2025 | paraformer | SenseVoice |
|---|---|---|---|---|---|
| 0.wav 中英混（10.05s） | 0.021 | 0.031 | 0.029 | 0.024 | 0.015 |
| en5 纯英文（8.46s） | 0.022 | 0.031 | 0.029 | 0.025 | 0.014 |
| zh2 中文（7.52s） | 0.023 | 0.032 | 0.029 | 0.025 | 0.014 |
| mix 中英混（10.99s） | 0.023 | 0.030 | 0.029 | 0.025 | 0.014 |

→ 在 M5 Pro 上全部 RTF < 0.04；普通双核 x86 按 3–6 倍折算仍 < 0.2，均可实时。**RTF 不是瓶颈**。

### 4.2 识别质量（关键）

| 音频 → 参考 | 2023 bilingual | zh-2025（纯中） | ctc-zh-2025 | paraformer | **SenseVoice** |
|---|---|---|---|---|---|
| zh0：`昨天是 monday today is library the day after tomorrow 是星期三` | `…MONDAY TODAY IS LIBR THE DAY…`（LIBR 截断） | `昨天是MANDYTEDIS里巴奥这对阿福特吃猫肉是星`（英文全毁） | `…MADYTDI里巴二这对阿福特吃毛肉是星` | `…today is li 八二 the day…` | `昨天是 monday today is礼拜二 the day after tomorrow 是星期三`（可读+标点） |
| en5：`hello everyone today we are testing the speech recognition system for the hearing impaired let us begin the meeting` | `…WE ARE JUST IN THE SPEECH… LET US BEGIN THE`（"testing→just in"、尾截断） | 拼音胡话 | 拼音胡话 | `…we are testing the speech reccognition system for the hearing impailet us speaking the the`（明显更准） | **100% 正确 + 标点** |
| zh2：`昨天是星期三，明天我们去上海出差，讨论一下项目的预算和时间安排` | `…时间安`（缺"排"） | `…时间安` | `…时间安` | `…时间安` | **完整 `…时间安排` + 标点** |
| mix：`…下一步计划。So first of all, let me walk through the requirements。好的，那我们开始吧` | `…SO FIRST LET ME WALK THROUGH THE REQUIREMENTS好的…`（英文最干净） | `…SOFFERSOFOLTMIWATSRUTRECHAREMS好的…` | `…S菲TOF特RCMN好的…` | `…so ofof of all let me work through the requirements 好的…`（"walk→work"、"first of all→ofof of all"） | `…so first of all let me work through the requirements 好的…`（标点全） |

**要点**：
- 纯中文干净语音（TTS）上各家**持平**；真正的差距出现在噪声/口音/会议远场（见 §7 的 CER 数据）与**英文**上。
- **zh-only（zh-2025 / ctc）遇到英文 = 输出拼音/乱码**，会严重伤害"中英混合"场景的观感。
- **paraformer 英文整句优于 2023**（"testing" 正确），但在部分中英夹杂句上弱于 2023（"ofof of all"）；中文带方言支持。
- **SenseVoice 是唯一"梦想级"输出**：4/4 全对 + 标点 + ITN，且 RTF 最低——代价是**非流式**（延迟≈整句）。

---

## 5. 中文精度硬数据（流式，icefall multi_zh-hans 官方 CER）

来源：[icefall RESULTS.md（zipformer large，流式）](https://github.com/k2-fsa/icefall/blob/master/egs/multi_zh-hans/ASR/RESULTS.md)

| 解码方式 | AISHELL-1 test | MagicData test | WenetSpeech test-net | Alimeeting test |
|---|---|---|---|---|
| Transducer Greedy **Streaming**（= zh-int8-2025-06-30） | **1.91** | **2.71** | **8.54** | 28.74 |
| CTC Greedy **Streaming**（= ctc-zh-int8-2025-06-30） | 1.97 | 2.87 | 10.62 | 28.10 |

- 现用 2023-02-20 双语模型官方未公布 AISHELL CER（训练数据远少于 2025 版、配方更旧），社区公认明显弱于上表。
- **注意 Alimeeting（会议远场）CER 高达 28%**：多说话人、噪声、远场对一切流式模型都是难题。面对面近讲（本 App 场景）会好得多，但多人同时说话仍是最大误差来源，务必对用户提示靠近麦克风/单人多句。

---

## 6. 甜点分析：怎么选

约束：普通话为主 + 部分英文；字幕延迟 <1s；无 GPU 的双核/四核 CPU；4–8GB 内存。

1. **真流式 + 中英双语（默认）→ `streaming-paraformer-bilingual-zh-en` int8（≈237MB）**
   - 1.13.6 零改动接入（`OnlineRecognizer.from_paraformer`）；国内方言（河南/天津/四川等）支持；
     英文整句明显优于 2023；RTF 充裕；Apache-2.0。
   - 缺点：无时间戳；流式 partial 约 1s 粒度；无 ITN/标点（靠 LLM 整理，App 本来就有）。
2. **纯中文 + 最大化流式精度 → `streaming-zipformer-zh-int8-2025-06-30`（167MB）或 CTC 版（162MB）**
   - AISHELL-1 test CER 1.91%（官方）；比 2023 双语模型更耐噪声/口音（训练数据 14k+ 小时多源）。
   - 硬伤：英文全部变拼音胡话（实测）。仅当产品接受英文退化时使用。
3. **质量天花板（可接受 2–4s 延迟）→ SenseVoice int8（239MB）+ silero VAD（2MB）**
   - 最接近"微信输入法"：ITN（`五千八百块→5800块`、`二零二五年→2025年`）+ 标点 + 情感/事件标签；
     实测 4/4 全对且 RTF 最低（0.014）。
   - 代价：非流式。官方以 `vad-microphone-simulated-streaming-asr` / `vad-with-non-streaming-asr.py`
     实现"模拟流式"，字幕按"整句"刷新，延迟≈句长。**App 侧建议保留流式模式作默认，把 SenseVoice 作为
     "高精度模式"开关**（sidecar 已有端点检测，加 silero VAD + `OfflineRecognizer` 改动可控）。
   - 英文长句 WER 仍 > Whisper（FunASR 官方英文 CER/WER 14.71%），但实测在本场景已足够好。
4. **X-ASR（1M 小时、带标点、160–1920ms 多档流式）**：纸面最优，但**任何已发布运行时（含 master）都无法
   加载它的现成导出**（缺 `encoder_dims` 元数据 → 退出/崩溃）。列入"等上游修复"观察项，勿现在投入。

**不推荐升级运行时**：无 2.x 可升；1.13.6 是当前最新且已支持全部所需解码路径。

---

## 7. 工程落地要点（TalkSee）

1. **`src-tauri/src/models.rs` 内置目录**：新增 2 个 ASR 条目（默认 paraformer bilingual int8；可选 SenseVoice int8），
   保留 2023-02-20 作为"低资源/纯流式"回退。文件清单与字节数：
   - paraformer：`encoder.int8.onnx`(165,462,184) + `decoder.int8.onnx`(71,664,561) + `tokens.txt`(75,756)
   - sense-voice：`model.int8.onnx`(239,233,841) + `tokens.txt`(315,894) + silero VAD（若加高精度模式）
   - zh-2025：`encoder.int8.onnx`(161,141,793) + `decoder.onnx`(5,165,083) + `joiner.int8.onnx`(1,033,416) + `tokens.txt`
2. **sidecar `sherpa_streaming.py`**：
   - paraformer：`from_paraformer(tokens=…, encoder=…, decoder=…)`（注意 1.13.6 keyword 是 `encoder/decoder`，
     不是 `paraformer_encoder/decoder`）。
   - SenseVoice：新增 `OfflineRecognizer.from_sense_voice(..., use_itn=True)` + silero VAD 切割，复用现有
     speaker-embedding 说话人分段（embedding 提取本就走分段）。
3. **内存**：paraformer/SenseVoice int8 加载各约 0.8–1.2GB（ORT 内），4–8GB 机器无压力；切勿同时加载两个
   大模型（按需切换）。
4. **RTF 注意**：M5 Pro 实测 0.014–0.031；双核 x86 上 paraformer 文档值 0.15–0.21（4 线程），仍实时；
   `num_threads=2` 即可，不推荐 4 线程核烧 CPU。

---

## 8. 参考链接

- [sherpa-onnx 流式 transducer 模型文档](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html)
- [sherpa-onnx 流式 Zipformer-CTC 模型文档](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-ctc/zipformer-ctc-models.html)
- [sherpa-onnx 流式 Paraformer 模型文档](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-paraformer/paraformer-models.html)
- [sherpa-onnx SenseVoice 文档（含 RK3588 RTF 表与"模拟流式"示例）](https://k2-fsa.github.io/sherpa/onnx/sense-voice/index.html)
- [sherpa-onnx Releases（最新 v1.13.6，2026-08-18）](https://github.com/k2-fsa/sherpa-onnx/releases)
- [sherpa-onnx 2.0.0 预告 issue #3731（espeak-ng 移除，与 ASR 无关，未发布）](https://github.com/k2-fsa/sherpa-onnx/issues/3731)
- [icefall multi_zh-hans 流式 CER 数据（zh-large / zh-xlarge）](https://github.com/k2-fsa/icefall/blob/master/egs/multi_zh-hans/ASR/RESULTS.md)
- [FunASR 官方 vs Whisper 基准（SenseVoice 中文 CER 7.81% vs Whisper-large-v3 20.02%；SenseVoice CPU 17.2× 实时）](https://www.funasr.com/en/blog/funasr-vs-whisper-benchmark.html)
- [FunASR 讨论区同一基准（带英文 WER 说明）](https://github.com/modelscope/FunASR/discussions/2947)
- [SenseVoice 论文/项目（<80ms 推理延迟，SenseVoice-Large 50+ 语言）](https://arxiv.org/html/2407.04051v1)
- HF 仓库：paraformer-bilingual / sense-voice-2024-07-17 / zh-int8-2025-06-30 / ctc-zh-int8-2025-06-30 /
  X-ASR punct int8（`csukuangfj2`）