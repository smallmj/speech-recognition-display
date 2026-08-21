# 实时说话人识别（Speaker Diarization / SCD）改善调研报告

> **场景**：TalkSee（Tauri 桌面字幕应用），sherpa-onnx **1.13.6**，实时流式 ASR + 每句 final 提取 1 个 192 维说话人 embedding（`3dspeaker_speech_eres2netv2_sv_zh-cn_16k-common.onnx`，约 71MB），余弦相似度 + 时长自适应阈值状态机做说话人指派。**纯 CPU、必须实时**。
>
> **两个报障问题**：
> (a) 多人对话时常分不清谁在说话（指派错误）；
> (b) 新说话人**首句的尾部**被续到**上一位说话人的气泡**里——即 ASR final 边界滞后，切晚了，下一位的开头落进上一位的 final。
>
> **调研方式**：web_search + 一手信源（sherpa-onnx v1.13.6 源码（git clone 到 tag `v1.13.6`）、HuggingFace / ModelScope 模型卡、3D-Speaker / ERes2NetV2 / TitaNet 论文、官方 demo 脚本、GitHub release assets）。调研日期：2026-08-19。
>
> **配套报告**：[流式语音识别与实时说话人分离调研报告](./流式语音识别与实时说话人分离调研报告.md)（框架选型阶段的宏观结论）

---

## 0. 结论速览（TL;DR）

| 问题 | 结论 | 证据 |
|---|---|---|
| **换更强的 embedding 模型能救 (a) 吗？** | ❌ **几乎不能**。你现在用的 ERes2NetV2 已经是 sherpa-onnx 能加载的**中文模型里最好**的一个（CN-Celeb test EER 3.81%，官方卡内所有模型最低）；唯一更强的 `eres2net_large` 只在一个特定中文评测集上略好（6.34% vs 6.52%），代价是 116MB / 更慢，且未在 CN-Celeb 上公布数字——不值得换。**TitaNet 更快（RTF ~0.11 vs ~0.24）但只有英文版**（`nemo_en_*`）。**wav2vec2 说话人模型在 1.13.x 根本不支持**（extractor 只认 `model_type` 元数据：`wespeaker` / `3d-speaker` / `nemo`，源码已查证）。 | §2 |
| **(a) 的真正大头在哪？** | 在**输入 embedding 的句子音频被污染**（问题 (b) 的边界泄漏，两句话混在一起 → 无论多好的模型都归错人）和**模板/阈值状态机**，不在模型本身。ERes2NetV2 对 1-2s 短句已经是专门优化的（论文：2s 截断 trial EER 1.48%，比老 ERes2Net 相对降 54.8%）。**先修边界，再打磨指派逻辑，最后才考虑模型。** | §1、§3、§4 |
| **(b) 的根因？** | sherpa OnlineRecognizer 默认 endpoint 规则 rule2 是「说话后**1.2 秒**尾静音才 finalize」——上一个说话人停嘴后，只要下一位在 1.2s 内开口，**她的开头必然被并进上一位的 final**。当前按 final 切分、按 final 提取 embedding 的结构从根上就吃这个亏。 | §4.1（源码 endpoint.h 默认值） |
| **(b) 怎么改？** | 别再用「ASR final 边界」当说话人切片单位。**在 ASR 前面跑专用 VAD（Silero 或 ten-vad），以 VAD 段为切片单位**：VAD 判定「本段结束」的尾静音阈值可以压到 0.25–0.4s，且 VAD 段边界天然和语音对齐，ASR 只负责在段内转写。sherpa 官方就有 `speaker-identification-with-vad*.py` 全套模板。**这是本报告性价比最高的一条。** | §4 |
| **每句算多个 embedding 有用吗？** | ✅ **有用，且 sherpa 流式 API 原生支持**（`create_stream / accept_waveform / is_ready / compute`，compute 是增量式的：每次吃掉「尚未处理」的特征帧，源码已查证）。推荐头/尾窗口 + 全句各算一次：头尾投票解决短句指派不稳；**头 vs 尾归到不同人 → 直接判定该句被人为切段污染，做切分**（顺带自愈 (b)）。代价≈1.5–2 个全句 embedding，CPU 可接受，且只在句子较长/得分模糊时启用。 | §3 |
| **真流式 diarization（pyannote/NeMo）要上吗？** | **CPU-only 场景：暂不建议做主力，但「后台回补校正」值得做。** Diart（pyannote 系流式）CPU 可真实时（segmentation 11ms/5s 块、embedding 26–91ms/5s 块，AMD Ryzen 9 实测），但要装 PyTorch 侧车、默认是英文 embedding（中文要自写 loader 复用你的 ERes2NetV2 ONNX）；sherpa 自带 `OfflineSpeakerDiarization`（pyannote 分割 7MB + FastClustering）可以直接在**滚动窗口后台重跑**、回填修正之前的指派——纯 C++/ONNX 无新依赖。**NVIDIA Streaming Sortformer 需要 GPU，CPU-only 直接排除**；pyannoteAI Live-1 是商业产品（<300ms）不值得现在集成。 | §5 |

**推荐落地顺序（性价比从高到低）**：
1. **P0 — VAD 切片先行**（Silero 或 ten-vad 段 = 转写与 embedding 的统一单位；`min_silence_duration≈0.3s`），直接消灭 (b)。
2. **P0 — 模板/指派状态机增强**：每说话人多模板（首句中截取多窗口注册）、`GetBestMatches` 辅助消歧、未知段延迟注册并人工确认（官方 dynamic 示例的机制）、按说话人维护得分统计做自适应阈值。
3. **P1 — 句内多 embedding**：头/尾/全句投票 + 「头≠尾」→ 二分定位切点把句子切开（自愈 (b) 泄漏 + 顺带修 (a)）。
4. **P1 — 后台离线 diarization 回补**：滚动窗口跑 `OfflineSpeakerDiarization`，回填纠错。
5. **P2 — 可选性能/模型微调**：embedding 模型保持 ERes2NetV2 不变；如需提速可 `onnxruntime quantize_dynamic` 出 int8（约 2×、一定精度损失）。

---

## 1. 两个问题的根因定性

### 1.1 问题 (b)：final 边界滞后 —— 结构性问题，不是参数问题

sherpa-onnx 的 OnlineRecognizer endpoint 检测（`sherpa-onnx/csrc/endpoint.h`，v1.13.6 源码默认值）：

```cpp
EndpointRule rule1;  // {must_contain_nonsilence=false, min_trailing_silence=2.4s}
EndpointRule rule2;  // {must_contain_nonsilence=true,  min_trailing_silence=1.2s}
EndpointRule rule3;  // {min_utterance_length=20s}   // 超长强制切
```

即：**只要两人交接的间隔短于 1.2s**（中文对话非常常见：反问、插话、抢话），decoder 根本不会在换人处 finalize，而是把 B 的开头继续 decode 进 A 的句子，直到凑够 1.2s 尾静音才切。对字幕/气泡场景这有两层坏处：

- B 的开头文字出现在 A 的气泡里（用户报的 (b)）；
- **这个「A+B 混合句」的 embedding 既不像 A 也不像 B，余弦匹配随机归边**——这同时是 (a)「分不清谁在说话」的重要来源之一。当前实现「每 final 一个 embedding」把边界错误直接放大成身份错误。

### 1.2 问题 (a) 的构成

(a) 一般由三件事叠加：

1. **输入污染**（上一条）：混合句 embedding 不可信 —— 影响比重最高，且模型越好越「忠实反映两说话人」，越不归任何一边；
2. **短句 embedding 本身偏弱**：1–2s 句子做 192 维 embedding 的分离度低于全句（量化数据见 §2.2），阈值状态机在得分模糊区抖动；
3. **模板/阈值状态机粗糙**：单一模板、单一固定阈值、无消歧、无脏模板更新。

---

## 2. 说话人 embedding 模型选型（Q1）

### 2.1 sherpa-onnx 1.13.6 实际能加载哪些模型（源码级结论）

`sherpa-onnx/csrc/speaker-embedding-extractor-impl.cc`（v1.13.6）按 ONNX 元数据 `model_type` 分派到三类实现，**其余一律拒绝加载**：

| `model_type` 元数据 | 实现 | 覆盖模型 | 输出维度 |
|---|---|---|---|
| `3d-speaker` | `SpeakerEmbeddingExtractorGeneralImpl` | 3D-Speaker 全家（ERes2Net / ERes2NetV2 / CAM++ / Base / Large） | **192** |
| `wespeaker` | 同上 | WeSpeaker（resnet34 等） | **256** |
| `nemo` | `SpeakerEmbeddingExtractorNeMoImpl` | NeMo TitaNet-S/L、SpeakerNet | TitaNet **192**、SpeakerNet **256**（按 NGC 卡） |
| （其他，含 **wav2vec2**） | ✗ 拒绝 | — | — |

> **wav2vec2 说话人模型不能跑**：1.13.x extractor 没有 wav2vec2 分支。想要 wav2vec2/XLSR 类说话人 embedding（如 SpeechBrain 系）只能走 sherpa 之外的推理栈（如 onnxruntime 直跑或 PyTorch），不在「换模型」清单里。

模型清单与官方文件（发布页 [speaker-recongition-models](https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-recongition-models)，同一批也挂在 [csukuangfj/speaker-embedding-models](https://huggingface.co/csukuangfj/speaker-embedding-models)）：

| ONNX 文件（sherpa release） | 大小 | 维度 | 训练数据 | 中文 EER（官方卡） | sherpa 1.13.6 可跑 |
|---|---|---|---|---|---|
| **`3dspeaker_speech_eres2netv2_sv_zh-cn_16k-common.onnx`** ← **当前使用** | 71.4MB | 192 | 中英通用 ~200k 说话人 | **CN-Celeb test 3.81%**（全模型最低） | ✅ |
| `3dspeaker_speech_eres2net_base_200k_sv_zh-cn_16k-common.onnx` | 39.6MB | 192 | 同上 | CN-Celeb 5.66% | ✅ |
| `3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx` | 39.6MB | 192 | 3D-Speaker zh（~10k 说话人） | 未公布 CN-Celeb（diarization 示例默认模型） | ✅ |
| `3dspeaker_speech_eres2net_large_sv_zh-cn_3dspeaker_16k.onnx` | 116.1MB | 192 | 3D-Speaker zh（18.3M 参数） | 3D-Speaker zh 测试集：Cross-Device 6.89% / Cross-Distance 10.36% / Cross-Dialect 11.97% | ✅ |
| `3dspeaker_speech_eres2net_sv_zh-cn_16k-common.onnx` | 220.6MB | 192 | 中英通用 | （V1，已被 V2 取代） | ✅ |
| `3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx` | 28.3MB | 192 | 中英通用 ~200k | **CN-Celeb 4.32%** | ✅ |
| `3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx` | 28.3MB | 192 | 中英 advanced | 未公布 | ✅ |
| `3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx` | 29.6MB | 192 | VoxCeleb（英文） | —（英文） | ✅ |
| `3dspeaker_speech_eres2net_sv_en_voxceleb_16k.onnx` | 26.5MB | 192 | VoxCeleb（英文） | —（英文） | ✅ |
| `nemo_en_titanet_small.onnx` | 40.3MB | 192 | VoxCeleb+SRE+Swbd…（**英文**） | —（英文） | ✅ |
| `nemo_en_titanet_large.onnx` | 101.4MB | 192 | 同上（**英文**） | —（英文） | ✅ |
| `nemo_en_speakerverification_speakernet.onnx` | 23.4MB | 256 | VoxCeleb（**英文**） | —（英文） | ✅ |
| `wespeaker_zh_cnceleb_resnet34.onnx` / `_LM.onnx` | 26.5MB | 256 | CN-Celeb（中文） | 官方卡 gated 未公开；同架构 ResNet34 参考 **CN-Celeb 6.97%**（ModelScope 卡） | ✅ |
| `wespeaker_en_voxceleb_*.onnx`（CAM++/resnet34/152/221/293 ±LM） | 26–114MB | 256 | VoxCeleb（**英文**） | —（英文） | ✅ |

EER 来源：ModelScope 官方模型卡（[eres2netv2](https://www.modelscope.cn/models/iic/speech_eres2netV2_sv_zh-cn_16k-common)、[campplus](https://www.modelscope.cn/models/iic/speech_campplus_sv_zh-cn_16k-common)、[eres2net-base 200k](https://www.modelscope.cn/models/iic/speech_eres2net_base_200k_sv_zh-cn_16k-common)、[eres2net-large](https://www.modelscope.cn/models/iic/speech_eres2net_large_sv_zh-cn_3dspeaker_16k)）与 3D-Speaker GitHub [benchmark 表](https://github.com/modelscope/3D-Speaker)。注意各卡评测协议（训练集规模 ~3k vs ~200k）不同，**纵向比看同一张卡内**、横向只做量级参考。

### 2.2 结论：当前模型已是中文最优，短句性能也有专门优化

- **CN-Celeb（中文权威测试集）官方卡对比**：ERes2NetV2 `3.81%` < CAM++ `4.32%` < ERes2Net-base `5.66%` < ResNet34 `6.97%`。**你正在用的就是最好的**。
- **短句（1–2s）正是 ERes2NetV2 的卖点**：[ERes2NetV2 论文](https://arxiv.org/abs/2406.02167) 在 VoxCeleb1-O 上：全时长 EER 0.61% → 3s 截断 0.98% → **2s 截断 1.48%**；相对老 ERes2Net，3s trial 相对降幅 48.1%/41.7%/39.7%，**2s trial 相对降幅 54.8%/51.6%/48.1%**（三个 VoxCeleb1 测试集）。换更老/更小的模型只会更差。
- **唯一「更强」候选 `eres2net_large` 不值得换**：只在 3D-Speaker 中文测试集上略胜 ERes2NetV2（6.34% vs 6.52%，训练集不同，量级差异），且大 1.6 倍、CPU 更慢。
- **TitaNet 换不换**：社区实测 TitaNet 比 3D-Speaker 快约 2.2–2.7×（[sherpa-onnx 讨论 #3233](https://github.com/k2-fsa/sherpa-onnx/discussions/3233)：3DSpeaker embedding RTF≈0.241，TitaNet≈0.110）——但 sherpa 列表里 TitaNet/SpeakerNet **全是英文版（`nemo_en_*`）**，中文场景不可用，只作为「如果你哪天也做英文」的备注。
- 若只为提速：对现有 71MB ONNX 用 `onnxruntime.quantize_dynamic(..., weight_type=QInt8)` 出 int8（约一半体积、~2× CPU 加速、少量精度损失），sherpa 直接加载量化后的 path 即可（ORT 透明支持）。

**结论：模型不动，换模型这条线宣告结束。省下的力气放在 §3/§4。**

---

## 3. 每句多 embedding / 滑窗投票（Q2）

### 3.1 sherpa 流式 API 原生支持（源码级确认）

`SpeakerEmbeddingExtractor`（C++/Python 同构）：

```python
stream = extractor.create_stream()
stream.accept_waveform(sample_rate=16000, waveform=chunk)  # 可多次、增量喂
extractor.is_ready(stream)          # 有未处理的特征帧？
emb = extractor.compute(stream)     # 吃掉「全部未处理帧」并出 embedding
```

关键语义（`speaker-embedding-extractor-general-impl.h`，v1.13.6）：`compute()` 把**自上次 compute 以来新到的帧**做特征提取 → 全局均值归一化 → 推理 → **并把已处理帧计数前移**。即：

- 同一 stream 上**多次喂音频、多次 compute** = 天然的增量/滑窗 embedding（每次只算新增帧，总计算量≈全句一遍，不翻倍）；
- 每次 compute 是独立前馈（无状态），只对「本次窗口内的帧」做 pooling —— 短窗口 = 短时 embedding；
- 若要**重叠滑窗**或「头/尾/全句」精确定制窗口，直接从**已缓冲的句子音频裁剪**再喂给新 stream 即可，实现很直白。

### 3.2 推荐设计（三个用途都是同一套机制）

对每个「待判定的语音段」（建议改由 VAD 提供，见 §4）：

1. **多窗口投票（治 (a) 短句不稳）**：算
   - 头窗口（前 ~0.8–1.2s）
   - 尾窗口（后 ~0.8–1.2s，先 trim 尾静音）
   - 全句（现状）
   每个窗口 `manager.search(emb, threshold)` 得一个候选；**2:1 或 3:0 → 采纳多数**；三窗口三个不同人 / 得分都模糊 → 标「不确定」，用 `GetBestMatches` 取前 2 名 + 与两侧邻居的时序一致性兜底。
2. **头≠尾 → 边界污染检测 + 自愈切分（治 (b) 泄漏）**：若头归 A、尾归 B，且两者余弦 < 内部分裂阈值，判定该段混了两个说话人 → 在句内**二分搜索切点**（对 0.2s–1.5s 内每个候选切点算「左段←→右段」的 embedding 分离度，取间距最大者）→ 拆成两句分别指派。这样即使 VAD/endpoint 切晚了，也能把错误「事后拆开」，而不是把脏 embedding 归给 A 或 B。
3. **注册模板时也用多窗口**：enroll 时对同一人的多句音频分别取「首句头窗口 + 全句」入模板库（`manager.add(name, [emb1, emb2, ...])` 会自动取平均；也可存多条独立模板再 `GetBestMatches` 消歧），提高模板鲁棒性。

### 3.3 成本与配套 API

- 头+尾+全句 ≈ 1.5–2 × 单次全句计算。按社区 CPU RTF 数据（3DSpeaker 系 ≈0.24，见 [讨论 #3233](https://github.com/k2-fsa/sherpa-onnx/discussions/3233)）一个 2s 句子全句≈0.5s 计算——**只有得分模糊/句子较长时才启用多窗口**（先把全句算一次，模糊再补算头尾），常态开销不变。
- 模板管理 API（`speaker-embedding-manager.h`，v1.13.6）：`Add(name, emb|list)`、`Remove`、`Search(emb, threshold)`、`GetBestMatches(emb, threshold, n)`（**返回带得分的前 n 名**，消歧利器）、`Verify(name, emb, threshold)`、`Score(name, emb)`、`Contains`。全部可用。
- 增量聚类：sherpa 暴露了 `FastClustering(FastClusteringConfig(num_clusters=-1, threshold=0.5))`，可直接 `clustering(np.array(embeddings))` 拿簇标签（`python/csrc/fast-clustering.cc`）——适合对「最近 N 句 embedding」做小窗口聚类发现问题段，或在说话人数未知时做探索。
- 动态注册参考官方示例 [speaker-identification-with-vad-dynamic.py](https://github.com/k2-fsa/sherpa-onnx/blob/master/python-api-examples/speaker-identification-with-vad-dynamic.py)：未知名段默认阈值 0.4 → 连续出现/人工确认后再 `manager.add`（避免一次性噪声段污染模板）。

---

## 4. VAD / 分段边界处理（Q3）

### 4.1 根因回顾 + 两条路线

- 路线 A（现状）：**用 ASR endpoint 决定句子边界**。缺点见 §1.1（rule2 默认 1.2s 尾静音 → 换人无法及时切）。要救必须把 `endpoint_config` 调激进：`rule2.min_trailing_silence ≈ 0.3–0.6s`、可再开 `rule1`（纯静音超时）辅助。**有疗效但是「改装」**：ASR final 边界和语音边界本来就不是一回事（decoder 的静音判定与说话人无关）。
- 路线 B（推荐）：**VAD 切片先行**。在 ASR 前（或并行）跑专用 VAD，以 **VAD 段**为「转写 + embedding + 气泡」的统一单位；ASR 只做段内转写（段来一句起一句，或 Non-streaming 按段转）。sherpa 官方模板：[speaker-identification-with-vad.py](https://github.com/k2-fsa/sherpa-onnx/blob/master/python-api-examples/speaker-identification-with-vad.py)（VAD 段 → embedding → 识别）、[speaker-identification-with-vad-non-streaming-asr.py](https://github.com/k2-fsa/sherpa-onnx/blob/master/python-api-examples/speaker-identification-with-vad-non-streaming-asr.py)（VAD + ASR + 识别三段合一）、C++ 侧有 [sherpa-onnx-vad-with-online-asr.cc](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/sherpa-onnx-vad-with-online-asr.cc)。

### 4.2 sherpa 1.13.6 可用的 VAD（都是 v1.13.6 自带，无需升级）

| VAD | 模型文件 | 大小 | 窗口粒度 | 说明 |
|---|---|---|---|---|
| [Silero VAD](https://github.com/snakers4/silero-vad) | [`silero_vad.onnx`](https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx)（另附 `silero_vad.int8.onnx`） | 628KB / 207KB | 512/1024/1536 样本（32/64/96ms @16k） | 默认配置字段（源码 `silero-vad-model-config.h`）：`threshold=0.5`、`min_silence_duration=0.5s`、`min_speech_duration=0.25s`、`max_speech_duration=20s`（超时阈值自动抬到 0.9 强制切分）、**`neg_threshold`（退出门限，默认 `max(threshold-0.15, 0.01)`）** |
| [TEN VAD](https://github.com/TEN-framework/ten-vad)（1.12.6 起集成，1.13.6 可用） | 仓库内 `ten-vad/src/onnx_model/ten-vad.onnx` | 308KB | **160/256 样本（10/16ms）** | 更细的帧粒度 → 段边界更贴、尾静音判定更短；官方称相比 WebRTC/Silero 在标注测试集上 PR 更优；配 `threshold` / `min_silence_duration` / `min_speech_duration` / `max_speech_duration`（源码 `ten-vad-model-config.h`） |

对「换人交接」场景，关键就是 **`min_silence_duration`（段尾静音多久算结束）** 和 **入场/出场双门限**：

- 默认 0.5s 尾静音对快节奏对话仍偏长 → **压到 0.25–0.35s**（官方 speaker-identification 类示例就用 0.25s）；
- `threshold` 抬到 0.45–0.55 减少背景噪声误判为新说话人；出场用 `neg_threshold` 双侧门限防抖动；
- `min_speech_duration` 保持 0.25s，**<0.5s 的 VAD 段不参与说话人指派**（官方示例的护栏：embedding 太短不可信，直接 `vad.pop()` 跳过）——避免「半个气音」污染模板；
- 段间 padding（如每段前后 0.1–0.2s）喂 ASR，避免头尾字被截。

### 4.3 与 ASR 的配合方式

- **方案 1（改造成本最低）**：保留现有流式 ASR，但**用 VAD 段事件替代 endpoint** 来「落气泡」：VAD 说段结束 → 立刻把该段音频（连同对应 ASR 文本）送 embedding、指派、上屏；ASR 的 final 只是拿文字，不再当边界。若 ASR 与 VAD 段存在轻微时差，最多用 final 文本做「追加/修正」。
- **方案 2（推荐，最干净）**：VAD 段 → 段内 Non-streaming / streaming ASR（官方 `vad-with-non-streaming-asr.py` 即此结构）→ 段 embedding。每段一个独立的「转写 + 身份」单元，边界、文字、气泡三者天然对齐。
- **调 endpoint 作为保底**：即便留在路线 A，也把 `rule2.min_trailing_silence` 从 1.2s 收到 0.4–0.6s、开启 `rule3`（如 15s 最长句）防长句失控，能显著减少 (b) 发生率——但会引入「同一人句中停顿被切开」的新问题，需配合「相邻同人段合并」规则。

---

## 5. 更进一步方案（Q4）：真流式 diarization 与后台回补

### 5.1 sherpa 自带的离线 diarization → 「后台回补校正」（低成本，推荐）

sherpa 1.13.6 自带 `OfflineSpeakerDiarization`（[offline-speaker-diarization.py](https://github.com/k2-fsa/sherpa-onnx/blob/master/python-api-examples/offline-speaker-diarization.py)）：pyannote 分割模型（[sherpa-onnx-pyannote-segmentation-3-0](https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-segmentation-models)，7MB ONNX，重叠语音也可分割）+ 你的 ERes2NetV2 embedding + `FastClustering`（`num_speakers` 已知或 `threshold` 推断）。

用法建议：**不用它做实时主路径**，而是每积累一段（如 30–90s）或每次大停顿，把「最近一段窗口音频 + 已知说话人模板」在后台线程跑一次 diarization，得到高置信的段级指派，**回填修正**此前气泡身份（UI 上做「已订正」标记）。CPU 实测量级：embedding RTF≈0.24（[讨论 #3233](https://github.com/k2-fsa/sherpa-onnx/discussions/3233)），30–60s 窗口去重语音后通常几秒可完成，放后台不卡实时。**附带收益**：pyannote 分割对重叠语音（两人同时说）也能分开归属，能兜住 VAD/endpoint 都搞不定的场合。

另：sherpa 自 1.10.29 起支持 [Revai/reverb-diarization-v1](https://huggingface.co/Revai/reverb-diarization-v1)（`sherpa-onnx-reverb-diarization-v1.tar.bz2` 10.9MB；还有 254MB 的 v2）作分割模型，DER 更好但更重，适合离线批处理，实时路径用 pyannote-3-0 即可。

### 5.2 Diart（开源流式 diarization，Python 侧车，CPU 可真实时）

- 是什么：juanmc2005 出品的流式 diarization 框架，pyannote 分割 + embedding + **增量聚类**，默认延迟 **500ms**，可在 500ms–5s 间调（[Diart GitHub](https://github.com/juanmc2005/diart)，论文《Overlap-aware low-latency online speaker diarization based on end-to-end local segmentation》）；
- CPU 实测（AMD Ryzen 9，README 表）：segmentation `pyannote/segmentation-3.0` **11ms/5s 块**、embedding `pyannote/embedding` 26ms/5s 块、wespeaker-resnet34-LM(ONNX) 48ms、TitaNet-Large 91ms → **CPU 上真实时是成立的**；
- 但：① 默认 embedding 是 **VoxCeleb 英文**模型，中文得自写 `EmbeddingModel` loader，正好可以复用你的 ERes2NetV2 ONNX（onnxruntime）——工程量中等；② 要把 Python/PyTorch 侧车接进 Tauri 进程（WebSocket/stdio 边车），引入新的部署复杂度；③ 聚类标签是「SPEAKER_00/01…」匿名标签，仍需与你的人名模板做映射。
- **定位**：如果后面想彻底拥抱「未知人数 + 重叠语音 + 自动标签漂移跟踪」，Diart 是最务实的开源选项；**现阶段对比 §5.1 的 sherpa 自给自足，收益不足以抵消 PyTorch 依赖**，列为中期储备。

### 5.3 NVIDIA NeMo / 新模型（GPU 依赖，CPU-only 排除）

- **NeMo 经典 diarization**（VAD→segmentation→embedding→clustering/MSDD）：Python/GPU 导向，CPU 上慢（社区反馈 GPU 利用率 0% 时甚至 6s vs 112s 的量级差距，[讨论](https://github.com/NVIDIA-NeMo/Speech/discussions/7969)），且流水线重、依赖大——CPU-only 桌面基本排除。
- **Streaming Sortformer**（NVIDIA 2025-08 发布，[报道](https://www.marktechpost.com/2025/08/21/nvidia-ai-just-released-streaming-sortformer-a-real-time-speaker-diarization-that-figures-out-whos-talking-in-meetings-and-calls-instantly)）：帧级实时 diarization，2–4 说话人，**英文优化 + 普通话验证过**，官方明确需要 **NVIDIA GPU**（NeMo/Riva 集成）——本项目的「纯 CPU 桌面」约束下不可行，仅作架构参考。
- **pyannoteAI Live-1**（[官方博客](https://www.pyannote.ai/blog/introducing-live-1-streaming-diarization)）：商业流式 diarization，宣称端到端 **<300ms**、原生 streaming 架构、100ms 块处理——高质量参照物，但闭源商业服务，不满足本地离线要求。

### 5.4 总评：CPU-only 实时可行的方案谱系

| 方案 | 延迟 | CPU 可行性 | 新依赖 | 定位 |
|---|---|---|---|---|
| **现状：ASR endpoint 边界 + 每句 1 embedding** | ~0.5–1.5s | ✅ | 无 | 基线 (需修 (b)) |
| **VAD 切片 + 多窗口 embedding + 模板增强（§3+§4）** | ~0.2–0.5s 可感知 | ✅ 实测可行（官方模式） | 无（VAD 模型 <1MB，sherpa 自带 API） | **本期落地** |
| **sherpa OfflineSpeakerDiarization 后台回补** | 回补延迟（秒级） | ✅（几秒/窗口） | 无（7MB 分割 ONNX + FastClustering） | **本期落地（后台）** |
| Diart 侧车 | 0.5–5s 可调 | ✅（实测 RTF<1） | Python + PyTorch + 侧车通信 | 中期储备 |
| NVIDIA Sortformer | 帧级 | ❌（需 GPU） | GPU | 不符合约束，仅参考 |
| pyannoteAI Live-1 | <300ms | ❌（商业云） | 在线服务 | 仅对照基准 |

---

## 6. 建议落地清单（优先级 / 工作量 / 预期收益）

| 优先级 | 动作 | 工作量 | 预期收益 |
|---|---|---|---|
| **P0** | 引入 VAD（Silero 或 ten-vad）以 **VAD 段为切片单位**；`min_silence_duration≈0.3s`、`threshold≈0.5`、`neg_threshold` 双侧门限；<0.5s 段跳过指派。ASR 在段内转写（保留流式 partial 体验） | 中（管线重构） | **(b) 消除 ~80%+**；顺带大幅缓解 (a)（喂给 embedding 的不再是混合句） |
| **P0** | 模板/指派状态机增强：`GetBestMatches` 消歧、多模板（首句头窗口+全句）、未知名延迟注册+人工确认、按说话人统计得分分布做阈值自适应 | 中 | (a) 显著改善；长会话稳定性 |
| **P1** | 句内多 embedding：头/尾/全句投票；头≠尾 → 二分切点拆分句子并分别指派（自愈 (b) 泄漏） | 小-中 | 兜底 (b) 泄漏残留；短句指派更稳 |
| **P1** | 后台 `OfflineSpeakerDiarization` 滚动窗口回补，订正历史气泡身份（UI 标记） | 中 | 长会话终局准确率高；重叠语音兜底 |
| **P2** | embedding int8 量化提速（模型不变，`quantize_dynamic`） | 小 | 多窗口计算更宽裕 |
| **P2** | 建内部评测集：2–4 人中文对话录音 + 人工标注段边界/说话人，量化「指派准确率 & 边界泄漏率」两个指标，回归每次改动 | 小 | 让本文所有「经验值」变成你机器上的可度量指标 |

> 不做的：换 embedding 模型（现模型最优）、wav2vec2（跑不了）、NeMo/Sortformer（CPU 不可行）、Diart（中期再说）。

---

## 7. 参考资料

**sherpa-onnx（版本 = v1.13.6，git tag 源码）**
- 仓库：[k2-fsa/sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx)
- 说话人识别文档：[Speaker Identification](https://k2-fsa.github.io/sherpa/onnx/speaker-identification/index.html)；说话人模型发布页：[speaker-recongition-models](https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-recongition-models)
- 说话人分离文档：[Speaker Diarization](https://k2-fsa.github.io/sherpa/onnx/speaker-diarization/index.html)；分割模型发布页：[speaker-segmentation-models](https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-segmentation-models)
- VAD 文档：[VAD（silero-vad / ten-vad）](https://k2-fsa.github.io/sherpa/onnx/vad/index.html)
- 源码（v1.13.6）：endpoint 默认规则 [`endpoint.h`](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/endpoint.h)、extractor 类型分派 [`speaker-embedding-extractor-impl.cc`](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/speaker-embedding-extractor-impl.cc)、增量 compute 语义 [`speaker-embedding-extractor-general-impl.h`](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/speaker-embedding-extractor-general-impl.h)、manager API [`speaker-embedding-manager.h`](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/speaker-embedding-manager.h)、Silero 配置 [`silero-vad-model-config.h`](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/silero-vad-model-config.h)、TenVad 配置 [`ten-vad-model-config.h`](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/ten-vad-model-config.h)
- 示例：`speaker-identification.py`、`speaker-identification-with-vad.py`、`speaker-identification-with-vad-dynamic.py`、`speaker-identification-with-vad-non-streaming-asr.py`、`offline-speaker-diarization.py`（[python-api-examples](https://github.com/k2-fsa/sherpa-onnx/tree/master/python-api-examples)）
- 社区实测：Offline diarization 慢 / RTF 与 TitaNet 对比 [Discussion #3233](https://github.com/k2-fsa/sherpa-onnx/discussions/3233)

**模型卡 / 论文（EER 一手来源）**
- 3D-Speaker GitHub（VoxCeleb1-O / CNCeleb / 3D-Speaker benchmark 表）：[modelscope/3D-Speaker](https://github.com/modelscope/3D-Speaker)
- ERes2NetV2 论文（2s/3s 短句数据）：[ERes2NetV2: Boosting Short-Duration Speaker Verification Performance](https://arxiv.org/abs/2406.02167)
- ModelScope 官方卡：ERes2NetV2 [iic/speech_eres2netV2_sv_zh-cn_16k-common](https://www.modelscope.cn/models/iic/speech_eres2netV2_sv_zh-cn_16k-common)、CAM++ [iic/speech_campplus_sv_zh-cn_16k-common](https://www.modelscope.cn/models/iic/speech_campplus_sv_zh-cn_16k-common)、ERes2Net-base-200k、ERes2Net-large（3D-Speaker 中文测试集）
- TitaNet 论文：[arxiv 2110.04410](https://arxiv.org/abs/2110.04410)；NeMo 卡：TitaNet-L（192 维）[NGC](https://catalog.ngc.nvidia.com/orgs/nvidia/teams/nemo/models/titanet_large)、SpeakerNet（256 维）[NGC](https://catalog.ngc.nvidia.com/orgs/nvidia/teams/nemo/models/speakerverification_speakernet)
- WeSpeaker：[wenet-e2e/wespeaker](https://github.com/wenet-e2e/wespeaker)（resnet34 类，256 维；zh-cnceleb 卡 gated）

**VAD**
- Silero VAD：[snakers4/silero-vad](https://github.com/snakers4/silero-vad)
- TEN VAD：[TEN-framework/ten-vad](https://github.com/TEN-framework/ten-vad)
- sherpa 模型文件：`silero_vad.onnx` / `silero_vad.int8.onnx`（[asr-models release](https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx)）

**流式 diarization 备选**
- Diart：[juanmc2005/diart](https://github.com/juanmc2005/diart)（流式 pyannote，500ms–5s 延迟，CPU RTF 表见 README）
- NVIDIA Streaming Sortformer 报道：[MarkTechPost](https://www.marktechpost.com/2025/08/21/nvidia-ai-just-released-streaming-sortformer-a-real-time-speaker-diarization-that-figures-out-whos-talking-in-meetings-and-calls-instantly)
- NeMo diarization CPU 表现讨论：[NVIDIA-NeMo/Speech #7969](https://github.com/NVIDIA-NeMo/Speech/discussions/7969)
- pyannoteAI Live-1（商业流式）：[Introducing Live-1](https://www.pyannote.ai/blog/introducing-live-1-streaming-diarization)
- Revai reverb diarization：[Revai/reverb-diarization-v1](https://huggingface.co/Revai/reverb-diarization-v1)