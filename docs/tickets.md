# 听障实时字幕气泡展示系统 — Ticket 索引

> 父规格：[Issue #1](https://github.com/smallmj/speech-recognition-display/issues/1)（`ready-for-agent`）
> 生成：`/to-tickets`（tracer-bullet 垂直切片，blockers 先行）

| 票 | Issue | 标题 | Blocked by | 状态 |
|----|-------|------|-----------|------|
| T1 | [#2](https://github.com/smallmj/speech-recognition-display/issues/2) | 工程脚手架 | 无 | ready-for-agent |
| T2 | [#3](https://github.com/smallmj/speech-recognition-display/issues/3) | 冒烟管线：合成转写 → 气泡流 | T1 | ready-for-agent |
| T3 | [#4](https://github.com/smallmj/speech-recognition-display/issues/4) | 显示定制：主题 + 置顶大字模式 + 文字样式 | T2 | ready-for-agent |
| T4 | [#5](https://github.com/smallmj/speech-recognition-display/issues/5) | 真实麦克风 + 本地流式 ASR（sherpa-onnx） | T2 | ready-for-agent |
| T5 | [#6](https://github.com/smallmj/speech-recognition-display/issues/6) | 说话人切换检测（SCD） | T4 | ready-for-agent |
| T6 | [#7](https://github.com/smallmj/speech-recognition-display/issues/7) | 手动命名与头像管理 | T5 | ready-for-agent |
| T7 | [#8](https://github.com/smallmj/speech-recognition-display/issues/8) | 云端 ASR 可切换 | T4 | ready-for-agent |
| T8 | [#9](https://github.com/smallmj/speech-recognition-display/issues/9) | LLM 整理管线（间隔 + 双轨 + 高亮） | T2 | ready-for-agent |
| T9 | [#10](https://github.com/smallmj/speech-recognition-display/issues/10) | 真实 LLM 接入（OpenAI 兼容 + SSE + 重试） | T8 | ready-for-agent |
| T10 | [#11](https://github.com/smallmj/speech-recognition-display/issues/11) | 会议纪要（停止后分批汇总） | T9 | ready-for-agent |
| T11 | [#12](https://github.com/smallmj/speech-recognition-display/issues/12) | 会话历史与导出 | T10 | ready-for-agent |
| T12 | [#13](https://github.com/smallmj/speech-recognition-display/issues/13) | 设置系统（标签页 + 操作提示 + 持久化） | T3, T7, T9, T11 | ready-for-agent |
| T13 | [#14](https://github.com/smallmj/speech-recognition-display/issues/14) | 托盘常驻 + 全局热键 + 会话状态 | T2 | ready-for-agent |

## Frontier（可立即开始）

- **T1**（无阻塞）—— 唯一无阻塞票，是整个实施链的起点。

## 依赖图（简）

```
T1
└─ T2 ─┬─ T3 ──────────────┐
       ├─ T4 ─┬─ T5 ─ T6    │
       │      └─ T7         │
       ├─ T8 ─ T9 ─ T10 ─ T11 ─┐
       └─ T13               │
       T12 ◀───────────────┘ (T3,T7,T9,T11)
```
