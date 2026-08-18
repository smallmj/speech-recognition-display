# 「实时字幕 + 间隔 LLM 整理」技术模式可行性调研报告

> 调研目标：听障人士用的实时字幕软件（流式 ASR 出原文），每隔 2s/5s/10s 把「尚未整理的原始文字」发给 LLM API 做口语化、纠错、补标点、整理通顺，然后**原地替换**界面上刚显示的原文；停止后再把全文分批交给 LLM 汇总成会议纪要。
>
> 调研方式：web_search + 一手来源（学术论文、官方文档、GitHub README、实测数据站）。
> 调研日期：2026-08-18。

---

## TL;DR（结论先行）

| 问题 | 结论 |
|---|---|
| **模式是否可行** | ✅ **可行**，且是**有学术论文背书**的做法（CARTGPT 等），但**不是商业产品的主流做法**。商业会议笔记产品（Otter / Fireflies / 通义听悟 / 飞书妙记 / 讯飞听见）都是「实时显示**原始**转写 + 会后/旁路 LLM 整理」，**没有**在实时字幕上原地替换。 |
| **延迟是否现实** | ⚠️ **部分现实**。10s 间隔完全现实；5s 勉强；**2s 对「整段重写式整理」不现实**（端到端 1–6s）。流式输出（SSE）能把感知延迟降到首字 ~0.5s，是降低感知延迟的关键手段。 |
| **原地替换的风险** | ⚠️ **高风险，需专门设计**。Google CHI 2023 用 123 人实验证明「已显示文本被改写」会显著伤害阅读体验（flicker）。CARTGPT 的做法是**高亮修正词**而不是整段替换。并发竞态需「不可变片段 + 编辑 ID + 只清理已冻结片段」。 |
| **成本** | ✅ 可忽略。即使 5s 一次、连跑 1 小时，GPT-4o mini / DeepSeek / 豆包这类便宜模型也只要几毛钱人民币量级。 |
| **推荐做法** | 「**原始行 + 整理行**」双轨展示（或高亮修正），而不是盲目整段替换；间隔 5–10s；只整理已冻结（不再被 ASR 追加）的片段；LLM 失败保留原文。 |

---

## 1. 这个模式是否成熟？商业产品是不是这么做的？

### 1.1 直接回答：是「已发表的研究方向」，不是「商业产品的主流形态」

**学术上完全成立，且就是为「听障/聋哑（DHH）人群实时字幕」做的。**

- **CARTGPT: Real-Time Correction of CART Captions Using Large Language Models**（Liang-Yuan Wu, Andrea Kleiver, Dhruv Jain，密歇根大学，ACM SIGACCESS / ASSETS 2024 **最佳海报奖**、2025 最佳论文提名）
  - 输入：CART 人工速记字幕 + ASR 转写 → LLM 实时**纠错** → 输出修正字幕。
  - 39.7 小时语料（医疗/技术/对话）上，**比标准 CART 提升 5.6% 字准确率，比最先进 ASR 提升 17.3%**。
  - 16 名 DHH 用户实验：字幕「显著更易理解，同时保持实时响应性」。
  - **关键 UX 细节：修正词用粗体高亮，而不是悄悄整段替换。**
  - 源码：官网显示 "Code (Coming Soon)"，demo 页：https://binomial14.github.io/cartgpt-demo/ ；论文：https://dl.acm.org/doi/10.1145/3663547.3746326
- **EvolveCaptions**（同一研究组，ASSETS 2025）：实时协作式 ASR 个性化（让听力正常的参会者实时纠正 DHH 语音转写错误，再用 GPT-4 生成发音提示、微调 ASR）。方向是「调 ASR 模型」而非「整理文本」，但同属「实时转写 + LLM 参与」的可访问性研究前沿。arXiv: https://ar5iv.labs.arxiv.org/html/2510.02181

**商业产品结论：它们都做「实时原文」+「会后/旁路整理」，不做「原地替换」。** 证据链如下：

| 产品 | 实时阶段 | 整理/纪要时机 | 证据 |
|---|---|---|---|
| **Otter.ai** | 实时显示原始转写（含 "um"、口误、重复），可实时旁注 | AI 摘要/行动项**会后**生成；说话人标签会后手动修正 | [meetly.help 实测](https://meetly.help/otterai-real-world-test-live-transcripts-vs-post-call-summaries-7976/)：「实时字幕会显示每个 um 和口误，很分散注意力」「AI 摘要是会后生成的」 |
| **Fireflies.ai** | 实时原文转写 + 实时 AI 笔记/行动项**并排**显示（笔记是旁路摘要，不是替换原文） | 「会后收到全部行动项」；"Instant Takeaways" 在**会议结束前 5 分钟**才在聊天里推送 | [Fireflies 官方博客](https://fireflies.ai/blog/fireflies-ai-launches-real-time-meeting-notes/) |
| **通义听悟** | 实时转写/实时翻译；官方提供**「口语书面化」= 对转写结果做改写润色**的能力，但作为 API/离线功能，不是实时字幕的原地替换 | 全文摘要、章节速览、纪要均为**后处理/API 能力** | [通义听悟能力文档](https://help.aliyun.com/zh/tingwu/benefits) |
| **飞书妙记** | 实时字幕 + 智能纪要**分屏** | 智能纪要为独立产物 | [飞书帮助中心](https://www.feishu.cn/hc/zh-CN/articles/244959839578) |
| **讯飞听见** | 实时转写，**修改是手动的**：热词优化、插入笔记区、打点标记 | AI 总结（全文摘要/章节速览）在**录音结束后**生成；「规整结果」是**独立下载项** | [讯飞听见官方教程](https://www.iflyrec.com/zhuanxie/67d8e38a.html) |

> **核心判断：** 商业产品把「实时」和「整理」做成**两个并行的面**（左边实时原文、右边/会后 AI 整理），从未把「已显示的原文就地改掉」。理由不是技术做不到，而是**体验风险**（见第 3 节，Google 的 CHI 论文量化过这个风险）。所以你要做的「原地替换」是**差异化创新**，有研究背书，但没有成熟产品可直接参考 UI。

---

## 2. 延迟与成本

### 2.1 实测延迟数据（第三方测速，量级参考）

测速口径说明：不同站点/网关/网络环境差异很大，以下为量级参考，落地前应以你自己的网络到目标机房实测为准。

| 模型 | 首字延迟(TTFT) | 输出速度 | 数据来源 |
|---|---|---|---|
| **GPT-4o mini** | **~785ms**（OpenRouter 单 token 探测） | 快（便宜小型模型，通常数百 t/s 上限） | [AILatency](https://www.ailatency.com/models/openai-gpt-4o-mini.html) |
| **DeepSeek（官方）** | **~0.2s 量级** | 官方约 **67 tok/s**（deepseek-v4-flash 测到 116 tok/s） | [LMSpeed DeepSeek](https://lmspeed.net/zh/provider/deepseek) |
| **豆包 / 火山方舟** | ~0.3s 量级 | 全站基线 **40 tok/s**，单模型 22–69 tok/s | [LMSpeed Volcengine Ark](https://lmspeed.net/provider/volcengine-ark) |
| **Claude Haiku 4.5** | ~1.1–2.2s（经第三方中转） | ~88–112 tok/s | [LMSpeed 对比页](https://lmspeed.net/zh/compare/model/claude-haiku-4-5-vs-claude-sonnet-4-5) |
| **Claude Sonnet 4.5** | ~3.1–3.3s（经第三方中转） | ~41–45 tok/s | 同上 |

**一段「几十~几百字」文本整理的端到端延迟估算：**

```
端到端 ≈ TTFT（首字延迟）+ 生成时长（输出 token 数 ÷ 输出速度）
```

- 假设待整理片段 150 字（约 200–300 token 输入），整理后输出约 120–200 token。
- **GPT-4o mini / DeepSeek / 豆包（便宜档）**：TTFT 0.2–0.8s + 生成 200 token @60 tok/s ≈ 3.3s → **合计约 3.5–4s**。
- **Claude Haiku**：TTFT 1.1s + 200 token @90 tok/s ≈ 2.2s → 合计约 **3.3s**（第三方中转，官方直连更稳）。
- **Claude Sonnet / 大模型**：TTFT 3s + 200 token @45 tok/s ≈ 4.4s → 合计 **7s+**，不适合短间隔。

### 2.2 间隔可行性结论

| 间隔 | 可行性 | 说明 |
|---|---|---|
| **10s** | ✅ 推荐 | 端到端 ~4s < 10s，剩余 ~6s 余量，几乎不会「整理结果赶不上下一批」 |
| **5s** | ⚠️ 勉强 | 需用便宜快模型 + 流式 + 控制片段长度（≤300 字）；错峰/限流需做 |
| **2s** | ❌ 不现实（整段重写） | 2s 连一个请求的端到端（~3.5s）都放不下，除非只做**极短句**（单句 ~50 字）+ 流式，且放弃「整段重写」只做「轻纠错」 |

### 2.3 流式（SSE/streaming）能降多少感知延迟？

- **结论：能，且是必须项。** 流式输出让**第一个整理后的字 ~TTFT（0.2–1s）就出现**，随后逐字填入，用户看到「整理正在发生」而非「等了 4 秒整块替换」。
- 主流厂商（OpenAI / Anthropic / DeepSeek / 火山方舟）全部支持 `stream=true` 的 SSE 输出；LiveTranslate、cnblogs demo 等实时工具都用了流式显示。
- 额外收益：流式下可以在**首字出现前**先给用户一个「整理中…」占位，感知延迟进一步被掩盖。
- 参考：[Streaming API Implementation Guide (SSE)](https://crazyrouter.com/en/blog/streaming-api-implementation-guide) / [SSE vs WebSocket 延迟指南](https://crazyrouter.com/en/blog/streaming-ai-api-sse-websockets-2026-latency-guide)

### 2.4 成本（几乎可忽略）

以「每 5s 整理 200 字（约 300 输入 token、150 输出 token）、连续 1 小时 = 720 次请求」粗算：

| 模型 | 输入价/百万 token | 输出价/百万 token | 1 小时成本（约） |
|---|---|---|---|
| **GPT-4o mini** | $0.15 | $0.60 | ≈ 720×(300×0.15 + 150×0.60)/1e6 ≈ **$0.10（约 ¥0.7）** |
| **DeepSeek（官方，V 系）** | 极低（多次永久降价） | 极低 | 比上更低 |
| **豆包 Lite/Mini** | 低 | ~$0.76M / ~$0.56M | 同量级或更低 |

数据参考：GPT-4o mini 定价见 [AILatency](https://www.ailatency.com/models/openai-gpt-4o-mini.html)；豆包 Seed 2.0 定价见 [ofox 豆包全系测评](https://ofox.ai/zh/blog/doubao-seed-2-api-guide-2026/)；DeepSeek 有「永久降价」公告（2026-05，[21财经](https://www.21jingji.com/article/20260523/herald/d204563d76b827ed2cc59fadf3731a8e.html)）。**成本不是决策瓶颈。**

> ⚠️ 定价波动频繁，落地前以各平台控制台实时价为准。

---

## 3. 原地替换的技术风险与对策

### 3.1 UX：原文突然被替换成整理版，体验问题如何缓解？

**这是本模式最大的坑，而且有硬证据：**

- **Google Research / ACM CHI 2023《Modeling and Improving Text Stability in Live Captions》**（123 人用户研究）：
  - 实时字幕天然会「用 interim 预测覆盖已显示文本」，造成 **flicker（闪烁/跳动）**，损害阅读：**注意力分散、疲劳、跟不上对话**。
  - Google 用「基于亮度的闪烁度量 + DFT」量化了 flicker，并提出**稳定化算法**（token 对齐 + 语义合并 + 平滑动画）——核心理念是**「对已显示文本，用户偏好稳定性 > 准确性」**，只在「不影响换行布局」时才改旧词。
  - 来源：https://research.google/blog/modeling-and-improving-text-stability-in-live-captions/
- 你的方案（LLM 整段改写）比 ASR 自我修正**改得更多、更晚**（整句重写），flicker 风险放大。而听障用户**对字幕的依赖是唯一的、连续的**，flicker 伤害比普通人更大。

**UX 缓解选项（按推荐度排序）：**

1. ✅ **双轨展示（推荐）**：顶部/当前行为「实时原文行」（不动的最近 1–2 句），下方是「已整理历史」（滚动的整理版）。原文永远不变，整理版持续刷新——无 flicker，还能对照。
2. ✅ **高亮修正（学术背书）**：像 CARTGPT 那样，在整理版里**只高亮有改动的词**（粗体/下划线/底色），其余不动。既给用户「哪里被改了」的透明感，又减少视觉跳动。
3. ⚠️ **原地替换但「只改已滚出视线的旧行」**：当前读到的最近 1–2 句绝不替换；只有滚出屏幕的句子才在后台替换为整理版。视觉上几乎无感。
4. ❌ **直接整段替换当前行**：不推荐——正对 Google 论文里被否定的行为。
5. 通用加分项：替换时加**平滑过渡动画**（fade/滑动），像 Google 稳定化算法那样；给「整理中…」的轻量占位。

### 3.2 并发/竞态：不丢字、不乱序

核心矛盾：ASR 新原文**持续追加**，LLM 整理结果**异步乱序回来**。

推荐模型（与你思路一致，细化如下）：

```
不可变原文片段 + 编辑ID + 防抖定时器 + 只整理已冻结片段
```

- **不可变片段（immutable segment）**：ASR 每产出一个「已确认片段」即分配 `segmentId`，原文**永不修改**。ASR 的 interim（进行中）部分单独标记 `status=active`，**不送 LLM**。
- **只整理已冻结片段**：`status=frozen`（一段时间无新追加 / 遇 VAD 停顿 / 到达整理节奏）的片段才进 LLM 队列。这天然避免「整理进行中原文又变长」的竞态。
- **编辑 ID 单调递增**：每次整理请求携带 `editId`（全局递增）。前端/渲染层**只接受比当前更大的 editId**，防止旧结果覆盖新结果（丢字）。
- **防抖定时器**：以「最后一段追加时间」为基准的 debounce（如 2s 无新内容才触发），叠加「固定节奏」（每 5–10s 至少触发一次），兼顾「少打扰」与「不卡顿」。
- **顺序保证**：整理队列**按 segmentId 有序提交**，结果按 segmentId 有序写入；若只做「追加式整理」（新整理结果只接在已整理内容后面，不回改更早的内容），从根上消除乱序。
- **并发上限**：同一时间只允许 1 个在途 LLM 请求（或小并发 2 个但保证写入串行化），用单一 writer 线程/协程消费结果队列。

参考实现范例：cnblogs 的 Whisper+GPT demo 就是用「audio_queue + transcribe_lock + display_queue + 单 writer」保证不丢不乱：https://www.cnblogs.com/gccbuaa/p/19291919

### 3.3 token / 上下文窗口控制

- **单次输入上限**：把一次发给 LLM 的片段控制在 **200–500 字**（约 300–700 token）。超过就切段。
- **全局上下文策略**（两种，二选一）：
  - **无状态（推荐起步）**：每次只带本片段 + 极简系统提示（如「你是字幕整理助手，去口语化、补标点、纠错，只输出整理后文本」），不累积历史——**省钱、快、无窗口溢出**。
  - **滚动窗口（进阶）**：带最近 5–10 句作为「上文」提升一致性（人名/术语），但设硬上限（如 ≤2000 token），超了截断。CARTGPT 是「CART + ASR 双输入」式，可类比「原文 + 上下文」。
- **防止无限累积**：整理成功的片段从「待整理池」移除；待整理池设上限（如 3000 字），溢出优先丢最老的（或跳过）。
- **处理失败时清空**：LLM 报 429/超时后，该片段标记 `retry`，重试 2–3 次后退避并**直接展示原文**，避免堆积。

---

## 4. 现成的开源工具/项目盘点

### 4.1 与「实时转写 + LLM 参与」最接近的现成项目

| 项目 | 干什么 | 与你的模式的关系 | 链接 |
|---|---|---|---|
| **CARTGPT**（学术 demo） | 实时 LLM 纠正 CART 字幕，高亮修正词 | **最接近**：正是「实时字幕 + LLM 整理 + 高亮」，源码待发布 | [demo](https://binomial14.github.io/cartgpt-demo/) / [论文](https://dl.acm.org/doi/10.1145/3663547.3746326) |
| **LiveTranslate**（TheDeathDragon） | 系统音频 → VAD → ASR（faster-whisper/SenseVoice）→ **LLM 流式翻译** → 悬浮字幕 | 管线一模一样（ASR→LLM→流式字幕），只是任务是翻译；**每句都走 LLM API 且流式显示**，可复用作「整理」 | https://github.com/TheDeathDragon/LiveTranslate |
| **Whisper+GPT+Streamlit demo**（cnblogs/gccbuaa） | 麦克风 → 1–2s 音频块 → whisper → **GPT-4o-mini 润色翻译** → 增量显示；含 `transcribe_lock`、队列、断句、缓存 | **正是「每 N 秒把原文发给 LLM 整理」的公开 demo**，单 writer 防竞态，可直接抄架构 | https://www.cnblogs.com/gccbuaa/p/19291919 |
| **stream-translator-gpt-deepseek**（codenan42） | VAD 切片 + GPT/DeepSeek 翻译的实时字幕 | 同管线，注意它是翻译不是整理 | https://github.com/codenan42/stream-translator-gpt-deepseek |
| **jt-live-whisper** | 全本地实时转写/翻译 + 离线 LLM 摘要/逐字稿校正 | 实时部分不做「校正替换」；**「逐字稿校正」在离线批次**里做（印证商业逻辑） | https://github.com/jasoncheng7115/jt-live-whisper |
| **DeLive**（XimilalaXiang） | 12 种 ASR 后端实时悬浮字幕 + **AI Review Desk（会后校正/摘要/思维导图）** | AI 校正在「复盘工作台」，**不是实时原地替换**；架构值得参考 | https://github.com/XimilalaXiang/DeLive |
| **collabora/WhisperLive** | 近实时 Whisper 实现（语音直播字幕） | 提供实时 ASR 底座 | https://github.com/collabora/WhisperLive |
| **VideoCaptioner / LLM-SUBS / oil-subtitle** | 视频字幕生成后 **LLM 校对/断句/润色**（离线批处理） | 证明「LLM 整理转写文本」成熟可靠，只是**离线** | [VideoCaptioner](https://github.com/WEIFENG2333/VideoCaptioner) / [LLM-SUBS](https://github.com/xy2yp/LLM-SUBS) |

### 4.2 「本地小模型做整理」的低延迟选项

- **FlowScribe（Qwen2.5-0.5B 微调，语音转写格式化器）**：专门把「ASR 原始文本」格式化为通顺文本的 **0.5B 小模型**，可本地跑、毫秒级、零成本、隐私好——适合做「轻整理 + 兜底」，质量不如大 LLM 但足够实时。https://huggingface.co/Abdullahu5mani/flowscribe-qwen2.5-0.5b-v2
- 思路：**本地小模型做实时兜底整理（2s 间隔），云端大 LLM 做「沉淀整理/纪要」（10s 间隔或会后）** —— 两级流水线可覆盖所有间隔需求。

### 4.3 搜索词命中情况

你给的搜索词方向都对，实测命中的有效检索式：
- `realtime transcript LLM polish` / `live transcript LLM cleanup` → 命中 DeLive、LinguaVox、speak/ai-polishing
- `whisper live LLM summary` / `whisper live transcription LLM` → 命中 LiveTranslate、jt-live-whisper、WhisperLiveKit
- 中文 `实时转写 LLM 整理` → 命中 WhisperLiveKit 纪要文章、CSDN 会议纪要两种思路
- `实时字幕 LLM 校正 替换` → 命中 CARTGPT（学术）
- `live captions LLM` → 命中 CARTGPT、EvolveCaptions、Snaption Live（均为 DHH 可访问性方向）

**确实有人做过「定时把 whisper 原文发给 LLM 整理」**：最直接的公开实现就是 cnblogs 的 Whisper+GPT+Streamlit demo（每 1–2s 一块去润色），以及 LiveTranslate 的「每句 LLM 处理 + 流式显示」。但注意：它们展示/追加为主，**没有做「已显示原文原地替换」**——这一步目前没有现成开源实现，属于你要自己做的部分。

---

## 5. 推荐实现方案（具体设计）

### 5.1 总体架构

```
[流式 ASR 原文流]
     │ 每个已确认片段 → appendSegment()
     ▼
[不可变片段存储]  (Map<segmentId, {raw, status: active|frozen, cleaned?, editId}>)
     │ debounce(2s 无追加) ∪ 固定节奏(5s/10s) → freeze() 最老的 active 段
     ▼
[整理队列]  (按 segmentId 有序，单在途请求)
     │ stream=true 调 LLM API（系统提示：去口语化/纠错/补标点/只输出整理文本）
     ▼
[结果写入器]  (单一 writer；editId 单调递增校验，旧结果丢弃)
     ▼
[渲染层]  (三选一：双轨展示 / 高亮修正 / 只替换已滚出屏幕的旧行)
```

### 5.2 数据结构

```ts
type SegmentStatus = 'active' | 'frozen' | 'cleaned' | 'failed';

interface Segment {
  id: number;            // 全局单调递增，作排序与去重键
  raw: string;           // 不可变原文
  status: SegmentStatus;
  cleaned?: string;      // 整理结果（可空=未整理/失败）
  editId?: number;       // 写入时的单调 id，渲染层只接受更大的
  ts: number;            // 最后追加时间（防抖基准）
  retries: number;
}
```

- 原文字符串**只写一次**，一切替换都以 `editId` 版本化。
- `editId` 全局递增由结果写入器统一发号，从根上杜绝「旧结果盖新结果」。

### 5.3 定时器 / 触发策略

```ts
// 防抖：距最后一段追加 2s 无新内容 → 触发一次整理
// 兜底节奏：无论有无停顿，每 5~10s 至少触发一次
// 触发动作：把最老的 active 段 freeze() 后入队
// 并发：inFlight 标志，同一时间只有 1 个在途 LLM 请求
```

- **间隔选 5s 起步、10s 为准**；2s 只在「本地小模型（FlowScribe）」或「单句轻纠错 + 流式」下尝试。
- 用 **VAD 停顿**作为「自然边界」优先（句边界整理质量远好于截断）。

### 5.4 错误处理（LLM 失败保留原文）

```ts
onResult(seg, text, ok) {
  if (ok) { seg.cleaned = text; seg.status = 'cleaned'; }
  else {
    seg.retries++;
    if (seg.retries <= 3) 重入队(指数退避 1s/2s/4s);
    else { seg.status = 'failed'; /* 界面展示原文 */ }
  }
  // 无论成败，把该段从待整理池移除，避免无限堆积
}
```

### 5.5 「整理后替换」与「用户正在读」的 UX 权衡（最终建议）

| 场景 | 建议行为 |
|---|---|
| **正在朗读/刚显示的最近 1–2 句** | **绝不替换**。保持原文，旁边给「整理中…」或直接不碰 |
| **已滚出视线的历史行** | 后台替换为整理版（fade 过渡），或双轨显示 |
| **默认 UI** | **推荐：原文在上（实时、稳定），整理版在下（滚动、可对照、差异高亮）**；提供「仅显示整理版」开关（对追求可读性的听障用户） |
| **切换提示** | 整理版出现差异时，用**颜色高亮改动词**（借鉴 CARTGPT 粗体、Google 稳定化动画） |
| **会后纪要** | 停止后：按时间窗口把整理版文本**分批**（每批 ~500 字 + 滚动上文窗口）交给 LLM 生成结构化纪要（要点/行动项/待办），复用同一套 LLM 管线 |

---

## 6. 结论与风险清单

### 结论
1. **可行**：有学术论文（CARTGPT）直接做了「实时字幕 + LLM 整理」并证明对 DHH 用户有效；技术管线（ASR→LLM→流式字幕）在多个开源项目（LiveTranslate、cnblogs demo）里已被验证可跑通。
2. **商业模式不等于你的模式**：主流产品（Otter/Fireflies/通义听悟/飞书妙记/讯飞听见）都做「实时原文 + 会后/旁路整理」，**没有**「实时原地替换」——这说明要么是差异化机会，要么是产品不认可体验。
3. **延迟可控但别贪快**：10s 间隔稳、5s 可行、2s 不现实；**流式 SSE + 便宜快模型 + 短片段**是把感知延迟压下来的三件套。
4. **成本不是问题**：一小时连续整理也就几毛钱人民币量级。
5. **最大风险是 UX（原地替换的 flicker）**：Google CHI 2023 有直接证据；对策是**双轨展示 / 高亮修正 / 只替换滚出视线的旧行**，并做**只整理冻结片段 + editId + 单 writer** 的并发设计。

### 风险清单（按严重度）
| 风险 | 等级 | 缓解 |
|---|---|---|
| 原地替换导致 flicker，损害听障用户阅读 | 🔴 高 | 双轨展示 / 高亮修正 / 只改旧行；平滑动画 |
| 2s 间隔延迟不达标 | 🟠 中 | 改 5–10s；本地小模型兜底；流式 |
| 并发丢字/乱序 | 🟠 中 | 不可变片段 + 冻结 + editId + 单 writer + 有序队列 |
| LLM 失败/超时 | 🟡 低 | 保留原文 + 重试退避 + 待整理池上限 |
| 上下文溢出 | 🟡 低 | 单次 ≤500 字，滚动窗口 ≤2000 token |
| 成本失控（长会） | 🟢 低 | 5s×1h ≈ ¥0.7 量级；可降频 |

### 建议下一步
1. 先做 **10s 间隔 + 双轨展示 + 高亮差异** 的 MVP，验证「整理质量」和「用户是否反感双轨」。
2. 拿 cnblogs demo / LiveTranslate 作工程骨架，替换「翻译」为「整理」，加 editId 与冻结逻辑。
3. 若想要 2s 体验，评估 FlowScribe 这类本地 0.5B 模型做轻整理。

---

## 附：主要来源链接

- CARTGPT 论文（ACM ASSETS 2024/2025）：https://dl.acm.org/doi/10.1145/3663547.3746326 ；demo：https://binomial14.github.io/cartgpt-demo/
- EvolveCaptions（arXiv 2510.02181）：https://ar5iv.labs.arxiv.org/html/2510.02181
- Google Research《Modeling and improving text stability in live captions》（CHI 2023）：https://research.google/blog/modeling-and-improving-text-stability-in-live-captions/
- Fireflies 实时笔记官方博客：https://fireflies.ai/blog/fireflies-ai-launches-real-time-meeting-notes/
- Otter.ai 实时 vs 会后实测：https://meetly.help/otterai-real-world-test-live-transcripts-vs-post-call-summaries-7976/
- 通义听悟能力文档（口语书面化）：https://help.aliyun.com/zh/tingwu/benefits
- 讯飞听见实时转写修改教程：https://www.iflyrec.com/zhuanxie/67d8e38a.html
- 飞书智能会议纪要帮助：https://www.feishu.cn/hc/zh-CN/articles/244959839578
- Whisper+GPT+Streamlit 实时字幕润色 demo：https://www.cnblogs.com/gccbuaa/p/19291919
- LiveTranslate（GitHub）：https://github.com/TheDeathDragon/LiveTranslate
- DeLive（GitHub）：https://github.com/XimilalaXiang/DeLive
- jt-live-whisper（GitHub）：https://github.com/jasoncheng7115/jt-live-whisper
- WhisperLive（collabora，GitHub）：https://github.com/collabora/WhisperLive
- FlowScribe（HF）：https://huggingface.co/Abdullahu5mani/flowscribe-qwen2.5-0.5b-v2
- GPT-4o mini 延迟（AILatency）：https://www.ailatency.com/models/openai-gpt-4o-mini.html
- DeepSeek 速度（LMSpeed）：https://lmspeed.net/zh/provider/deepseek
- 豆包/火山方舟速度（LMSpeed）：https://lmspeed.net/provider/volcengine-ark
- Claude 速度（LMSpeed 对比）：https://lmspeed.net/zh/compare/model/claude-haiku-4-5-vs-claude-sonnet-4-5
- 豆包 Seed 2.0 定价：https://ofox.ai/zh/blog/doubao-seed-2-api-guide-2026/
- DeepSeek 降价新闻：https://www.21jingji.com/article/20260523/herald/d204563d76b827ed2cc59fadf3731a8e.html
- Streaming SSE 延迟指南：https://crazyrouter.com/en/blog/streaming-ai-api-sse-websockets-2026-latency-guide
