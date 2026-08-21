# 更新日志模板 / Changelog Template

> 每次发布新版本时，在 [CHANGELOG.md](../CHANGELOG.md) 的**最新版本之上**插入一条新记录，按本模板填写（中英双语）。
> On every release, insert a new entry **above** the latest version in [CHANGELOG.md](../CHANGELOG.md), following this template (bilingual).
>
> 语义分类参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)：
> **Added（新增）/ Changed（变更）/ Fixed（修复）/ Security（安全）**。三者非全写，按当次实际落地填写。

---

## 标题头部 / Header

```markdown
## [版本号] — YYYY-MM-DD

### Added / 新增
- **功能 A（中文一句话）**：中文细节说明。
  - **Feature A**: English one-liner / detail.

### Changed / 变更
- **变更 B（中文一句话）**：中文细节说明。
  - **Change B**: English one-liner / detail.

### Fixed / 修复
- **修复 C（中文一句话）**：中文细节说明。
  - **Fix C**: English one-liner / detail.

### Security / 安全
- **安全项 D（中文一句话）**：中文细节说明。
  - **Security D**: English one-liner / detail.
```

- 版本号链接：在文末 `[...]: https://github.com/smallmj/talksee/releases/tag/vXXXX` 追加对应 tag。
- 若某类无改动，**整类省略**（不要留空标题）。
- 每条尽量一句话概括「做了什么 / 为什么」，面向用户与协作者，不写实现细节。
- 每条给中文 + 英文两行（英文可稍短）。

---

## 检查清单 / Checklist

发布新版本前，请确认：

1. [ ] CHANGELOG 顶部新增了本次版本的**中英双语**条目（放在最新版本之上）。
2. [ ] README（中文 + 英文）中与本次变更相关的功能描述已同步。
3. [ ] 版本号已同步：`src-tauri/tauri.conf.json` / `Cargo.toml` / `package.json` 三处一致。
4. [ ] 必要时更新 `docs/tickets.md` 或新增 ADR。
5. [ ] 打 tag 并推送（CI 自动出草稿 Release，人工确认发布）。

---

## 版本历史参考 / Version history reference

| 版本 | 日期 | 主要变更 |
|------|------|---------|
| 0.4.0 | 2026-08-21 | ASR 升级（paraformer/SenseVoice）、VAD 切片 SCD、滚动修复、自动更新 |
| 0.3.1 | 2026-08-21 | ASR 模型目录修复（移除不兼容 X-ASR） |
| 0.3.0 | 2026-08-19 | 发布管线（自包含运行时 + GitHub Release）、品牌 |
| 0.2.0 | 2026-08-17 | 首个完整 README、命名「语见 TalkSee」 |
| 0.1.0 | 2026-08（无 tag） | 功能积攒期：脚手架→冒烟→ASR→SCD→LLM→纪要→历史→设置→托盘→向导 |
