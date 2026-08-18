# 本地 sherpa-onnx 流式 ASR + 自拼说话人切换检测，而非全自动在线 diarization

实时多说话人字幕需要流式识别（边说边出字）与按说话人分组。选定 sherpa-onnx 流式 Zipformer 作为默认本地 ASR（真流式、中英双语、CPU 实时、双端离线），说话人处理采用"VAD 切句 + speaker embedding 余弦匹配"自拼切换检测（SCD），而非追求全自动在线 diarization——后者（diart/NeMo online）有秒级延迟与标签漂移，2024 年系统评测论文证实其"用准确率换延迟"。云端（讯飞实时转写带说话人分离 / Azure 实时 diarization）作为可选增强，与本地走同一抽象接口。

- **Considered Options**: 全自动流式 diarization（diart / NeMo online / StreamingSpeakerDiarization，延迟+标签漂移，不作为 MVP 主力）；云端 diarization（讯飞/Azure，付费+联网，作增强）；完全不做说话人区分（不满足需求）。
- **Consequences**: 重叠说话时 ASR 与 SCD 均退化（已知短板）；系统只能分组不能自动认人，需用户在 UI 手动命名/重命名说话人。
