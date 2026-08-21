# 说话人边界改为 VAD 切片先行 + 保留流式 ASR（而非按 ASR final 切句或离线逐句转写）

SCD 的两个真实故障（新说话人开头被并进上一人的 final、多人分不清人）根因在于：sherpa endpoint 默认需 **1.2s 尾静音**才产出 final，交接 <1.2s 的下一位开头必然被吞进上一条 final —— 混合句文字串人、混合句 embedding 谁也不像。v0.4 改为**在 ASR 前跑 Silero VAD，以 VAD 段为「转写文本、speaker embedding、气泡」的统一边界单位**：VAD 判定段结束（尾静音 ≈0.3s）即定稿该段并 reset 流，ASR 端点检测不再是句子边界（调激进仅作兜底）。转写仍由**连续流式 ASR** 负责（保留边说边出的 partial 与上下文上下文，文本质量最优），仅「何时落一个气泡/取哪段音频做 embedding」由 VAD 决定；段刚结束时留 400ms settle 窗口消化解码滞后，保证文本落点 ≈ VAD 边界。另加两层增强：头/尾窗口 embedding 投票（治短句指派不稳）、头≠尾→段内二分拆句自愈（兜底残留泄漏）。

- **Considered Options**: 保留 ASR final 边界（调研 §4.1，1.2s 尾静音结构性问题，调参只是「改装」）；VAD 段→离线逐句转写（§4.3 方案 2，边界最干净但丢失流式 partial 体验，与「低延迟优先」冲突）；全自动在线 diarization（延迟 + 标签漂移，ADR-0002 已否决）。
- **Consequences**: sidecar 以 VAD 段为事件单位（`final` 携带整段 embedding + head/tail 窗口 embedding + speech_duration + utt_seq）；VAD 模型（silero_vad.onnx ≈628KB）作为固定运行时资产随包分发，而非用户可选模型；无 VAD 时降级为端点定稿（行为与 v0.3 一致）；Scd 判定接口升级为多窗口投票（`process_utterance_multi`），单窗口路径保持兼容。