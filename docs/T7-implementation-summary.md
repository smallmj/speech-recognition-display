# T7 实现总结 - 云端 ASR 可切换

> Issue: [#8](https://github.com/smallmj/speech-recognition-display/issues/8)
> 决策：[ADR-0004](./adr/0004-switchable-cloud-asr.md)

## 实现内容

- **云端流式 ASR 客户端**：`src-tauri/src/cloud_asr.rs` 新增 Deepgram 兼容 WebSocket 客户端，实现 `engine::AsrPort`：
  - 麦克风音频沿用现有 16kHz f32 采集，转换为 little-endian `linear16` binary 帧；
  - URL 自动附加 `encoding=linear16&sample_rate=16000&channels=1&interim_results=true&endpointing=100&model=...&language=...`；
  - `Results.is_final=false` 进入 partial 缓冲，`is_final=true` 进入 final/`Utterance` 缓冲；
  - WebSocket 握手结果同步返回；识别失败/断开写入状态与错误信息。
- **配置与 UI**：`src-tauri/src/asr_config.rs` 持久化 `asr-config.json`，新增 `load_asr_config` / `save_asr_config` Tauri 命令；`src/components/AsrConfigPanel.tsx` 可在本地/云端间切换，并配置云端端点、API Key、模型、语言。
- **无缝热切换**：`src-tauri/src/pipeline.rs` 每秒轮询配置；来源变化时先排空旧 ASR final，再停止旧端口并启动新端口。`CleanupPipeline`、说话人映射与已显示气泡不重建。云端启动失败自动回退本地；本地也失败才回退演示模式。
- **状态契约**：`engine://status` 增加 `mode: "cloud"`，前端头部显示「云端 ASR（流式）」。

## 使用方式

1. 打开「🎙️ ASR 配置」；
2. 选择「云端 ASR」；
3. 填写 Deepgram 兼容 WebSocket 端点、API Key、模型（默认 `nova-3`）与语言（默认 `multi`，支持中英混合）；
4. 保存后由后台轮询配置并热切换，无需重启应用。

## 测试

- `cargo test -p speech-caption-display cloud_asr`：7 个协议/URL/鉴权/采样转换/队列测试通过；
- `cargo test -p speech-caption-display asr_config`：4 个配置默认值、序列化、降级与兼容测试通过；
- `cargo test -p speech-caption-display pipeline::tests`：4 个热切换决策测试通过；
- `cargo test`：workspace 全量测试通过（engine 41 个 + 壳层 27 个 + doctest 忽略项）。
- `pnpm build`：TypeScript 类型检查与 Vite 构建通过。
- `pnpm check:focus-exit`：通过。

## 已知边界

- 云端协议按 Deepgram 兼容接口实现；其他厂商需增加壳层协议适配。
- 云端 ASR 不提供本地 speaker embedding，因此云端转写归说话人 1，不伪造 SCD 结果。
- API Key 与 T9 LLM 配置一样以明文保存在本机 app config 目录。
