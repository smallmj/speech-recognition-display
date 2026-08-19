# 云端 ASR 采用 Deepgram 兼容流式 WebSocket，而非每厂商专用适配

T7 要求本地/云端 ASR 可切换且云端支持流式中英识别。多云厂商（Azure、讯飞、火山、腾讯、阿里等）实时协议互不兼容；若在 T7 直接实现多个专用客户端，会把鉴权、音频帧、partial/final 事件和错误语义都复制多份。因此 MVP 选定 Deepgram 流式 WebSocket 作为第一个云端协议：binary linear16 音频 + JSON `Results` 事件，天然支持 interim results 与多语言/中英配置，并可通过自定义 endpoint/model/language 适配兼容服务。云端客户端只存在于 Tauri 壳层，继续以 `AsrPort` 对 engine 暴露统一的拉取接口；来源切换只替换端口，不重建整理管线。

- **Considered Options**: 每厂商专用适配（T7 范围过大）；HTTP 分块转写伪流式（延迟与断句体验差）；把协议放进 engine（破坏 engine 无网络依赖的边界）。
- **Consequences**: 云端说话人先降级为单说话人（厂商无统一 speaker embedding 输出，不伪造 SCD）；API Key 明文本地存储沿 T9 取舍；后续其他厂商可在壳层增加协议适配并复用同一设置/切换机制。
