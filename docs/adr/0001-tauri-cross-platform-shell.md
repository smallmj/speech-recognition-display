# 采用 Tauri 2.x 作为桌面跨平台壳

桌面应用需要持续音频采集、本地 ASR 对接、现代气泡 UI 与 macOS + Windows 双端打包，最终选定 Tauri 2.x（Rust 后端 + Web 前端）。相比 Electron，macOS 系统音频采集有官方限制（desktopCapturer 不支持音频）且打包体积差一个数量级；相比 Flutter Desktop，其系统音频 loopback 生态最弱。Tauri 下本地 ASR 可走 sherpa-onnx 官方 Rust API 直接嵌入，Web 前端拿满滚动气泡/主题/动画生态。代价是需维护 Rust 音频层与系统 WebView（WKWebView/WebView2）兼容性。

- **Considered Options**: Electron（macOS 系统音频受限 + 体积 100MB+）；Flutter Desktop（系统音频 loopback 与自动更新生态弱）；原生双套（工作量翻倍）。.NET MAUI / Qt Widgets 因桌面+音频表现力不达标被排除。
- **Consequences**: 音频采集、说话人处理、LLM 整理均在 Rust 层实现，通过 Tauri Event 推给 Web 前端；打包（DMG/MSI）与签名已延后决定。
