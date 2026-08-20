# 听障实时字幕气泡展示系统 — Ticket 索引

> 父规格：[Issue #1](https://github.com/smallmj/talksee/issues/1)（✅ 已关闭）
> 生成：`/to-tickets`（tracer-bullet 垂直切片，blockers 先行）
> 状态标注：✅ 已完成（含合并 PR）· 🟢 frontier · 🔴 blocked

| 票 | Issue | 标题 | Blocked by | 状态 |
|----|-------|------|-----------|------|
| T1 | [#2](https://github.com/smallmj/talksee/issues/2) | 工程脚手架 | 无 | ✅ 已完成 (98aef4a) |
| T2 | [#3](https://github.com/smallmj/talksee/issues/3) | 冒烟管线：合成转写 → 气泡流 | T1 | ✅ 已完成 (835ec12) |
| T3 | [#4](https://github.com/smallmj/talksee/issues/4) | 显示定制：主题 + 置顶大字模式 + 文字样式 | T2 | ✅ 已完成 (916ca17) |
| T4 | [#5](https://github.com/smallmj/talksee/issues/5) | 真实麦克风 + 本地流式 ASR（sherpa-onnx） | T2 | ✅ 已完成 (6a51aef) |
| T5 | [#6](https://github.com/smallmj/talksee/issues/6) | 说话人切换检测（SCD） | T4 | ✅ 已合并 (PR #16 + #22) |
| T6 | [#7](https://github.com/smallmj/talksee/issues/7) | 手动命名与头像管理 | T5 | ✅ 已完成 (abc2250) |
| T7 | [#8](https://github.com/smallmj/talksee/issues/8) | 云端 ASR 可切换 | T4 | ✅ 已完成（本地提交） |
| T8 | [#9](https://github.com/smallmj/talksee/issues/9) | LLM 整理管线（间隔 + 双轨 + 高亮） | T2 | ✅ 已完成 (835ec12) |
| T9 | [#10](https://github.com/smallmj/talksee/issues/10) | 真实 LLM 接入（OpenAI 兼容 + SSE + 重试） | T8 | ✅ 已合并 (PR #15) |
| T10 | [#11](https://github.com/smallmj/talksee/issues/11) | 会议纪要（停止后分批汇总） | T9 | ✅ 已完成（本地提交） |
| T11 | [#12](https://github.com/smallmj/talksee/issues/12) | 会话历史与导出 | T10 | ✅ 已完成（本地提交） |
| T12 | [#13](https://github.com/smallmj/talksee/issues/13) | 设置系统（标签页 + 操作提示 + 持久化） | T3, T7, T9, T11 | ✅ 已完成（本地提交） |
| T13 | [#14](https://github.com/smallmj/talksee/issues/14) | 托盘常驻 + 全局热键 + 会话状态 | T2 | ✅ 已合并 (PR #29 + #30) |
| T14 | — | 首次运行初始化向导 + 手动开始识别 | T13 | ✅ 已合并 (PR #23) |
| T15 | — | SCD 幻影说话人修复（短句/噪声每条 final 都新建）| T5 | ✅ 已合并 (PR #25) |
| T16 | [#27](https://github.com/smallmj/talksee/issues/27) | ASR/Embedding 模型选择 + 双端配置 + LLM 整理开关 | T7, T9 | ✅ 已合并 (PR #28) |
| T17 | — | 发布管线：自包含运行时 + GitHub Release（DMG/NSIS） | T14, T16 | ✅ 已合并 (PR #38) |

## 🔴 Blocked（等前置票）

全部子票已完成，无阻塞项。

## 依赖图（简）

```
T1 ✅
└─ T2 ✅ ─┬─ T3 ✅ ──────────────┐
          ├─ T4 ✅ ─┬─ T5 ✅ ─ T6 ✅ │
          │         └─ T7 ✅       │
          ├─ T8 ✅ ─ T9 ✅ ─ T10 ✅ ─ T11 ✅ ─┐
          └─ T13 ✅ ─ T14 ✅      │
          T12 ✅ ◀────────────────┘
          T15 ✅ ─（SCD 修复，接 T5）
          T16 ✅ ─（模型选择，接 T7/T9）
          T17 ✅ ─（发布管线，接 T14/T16）
```
