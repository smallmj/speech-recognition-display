# T11 实现总结：会话历史与导出

> Issue: [#12](https://github.com/smallmj/talksee/issues/12)
> 依赖：T10（会议纪要）已完成。

## 实现内容

- **会话自动保存**（`src-tauri/src/sessions.rs` + `pipeline.rs`）：
  - 「停止并生成纪要」完成、`MinutesReady` 前，Rust 驱动把会话（原文/整理版、
    说话人、时间戳 + 会议纪要）写入 app data 目录 `sessions/session-<id>.json`；
  - 重启应用后历史仍在。
- **历史列表 + 重新打开**（`src/components/SessionHistoryPanel.tsx`）：
  - 新增「📚 历史会话」面板：列出历史会话（时间 / 条数），点「重新打开」加载
    并展示该会话的字幕记录与会议纪要。
- **导出 Markdown / TXT / SRT**：
  - `sessions::export_session_file(app, id, format)`：从本地记录生成并写入系统文档
    目录 `TalkSee-导出/会话记录-<id>.<ext>`；
  - Markdown：字幕记录（整理版优先，无整理版回退原文）+ 会议纪要；
  - TXT：纯文本同构导出；
  - SRT：逐条字幕带时间码（结束时间取下一条开始时间，最后一条 +5s）。
  - 当前会话的「💾 导出 .md」沿用之前的 `export_session`（实时构建字幕 + 纪要）。

## 验收标准逐条对照

- [x] **会话自动保存到本地**：停止后写入 `app_data/sessions/*.json`。
- [x] **历史列表查看 + 重新打开**：`list_sessions` / `load_session` 命令 + 历史面板。
- [x] **导出 Markdown / TXT / SRT**：`export_session_file` 支持三种格式。
- [x] **重启后历史仍在**：持久化在 app data 目录，不依赖内存。

## 验证

- `cargo test`：engine 59 个测试 + 壳层 35 个测试全部通过
  （新增 sessions 格式/保存/列表 5 个测试，另含导出字幕+纪要回归）。
- `pnpm build`：TypeScript 检查 + Vite 构建通过。
- `pnpm check:focus-exit`：通过。

## 已知限制

- SRT 时间轴为近似：只有每条发言的开始时间点，结束时间取下一条开始 / +5s。
- 「重新打开」在历史面板内查看（会话内容只读），不并入实时气泡流。
