//! 冒烟管线：`MockAsrPort` + `Engine`。
//!
//! T2 的核心是证明「合成转写 → 带说话人的事件流」的垂直管道是通的：
//! - [MockAsrPort] 喂入预设的合成转写文本（逐个说话人逐条），模拟真实 ASR 输入；
//! - [Engine] 持有端口，把每条转写转化为两条事件：
//!   1. `SpeakerAssigned`（先发：登记说话人 + 分配颜色/性别）；
//!   2. `SegmentAppended`（再发：携带完整片段）。
//! - 说话人颜色从 [SPEAKER_PALETTE] 按 `speaker_id % len` 稳定映射，同一说话人恒定。

use std::collections::HashSet;
use std::sync::mpsc;

use crate::types::{AsrPort, EngineEvent, Gender, Segment, SegmentStatus, Utterance};

/// 说话人调色板：8 种高对比颜色（hex，6 位，可直接拼接 alpha 后缀）。
pub const SPEAKER_PALETTE: [&str; 8] = [
    "#4f8cff", "#34c759", "#ff9500", "#ff3b30", "#af52de", "#ff2d55", "#00c7be", "#a2845e",
];

/// 说话人 → 颜色索引映射（取模，保证稳定且可复现）。
pub fn speaker_color(speaker_id: u32) -> String {
    let idx = (speaker_id as usize) % SPEAKER_PALETTE.len();
    SPEAKER_PALETTE[idx].to_string()
}

/// 构造一条转写单元（ts 由 MockAsrPort 的时间钟自动填充）。
fn utterance(speaker_id: u32, gender: Gender, text: &str) -> Utterance {
    Utterance {
        speaker_id,
        gender,
        text: text.to_string(),
        ts: 0,
    }
}

/// 模拟 ASR 端口：按预设脚本逐条吐出转写文本。
///
/// `loop_forever` 为真时脚本播完从头再来（演示用，气泡持续追加）；
/// 为假时播完返回 `None`（测试用，断言确定性的事件流）。
pub struct MockAsrPort {
    script: Vec<Utterance>,
    cursor: usize,
    loop_forever: bool,
    /// 内部时间钟：每次产出自增 `step_ms`。
    next_ts: u64,
    step_ms: u64,
}

impl MockAsrPort {
    pub fn new(script: Vec<Utterance>, loop_forever: bool) -> Self {
        Self {
            script,
            cursor: 0,
            loop_forever,
            next_ts: 1_700_000_000_000,
            step_ms: 600,
        }
    }

    /// 自定义时间钟起点与步长。
    pub fn with_clock(mut self, start_ts: u64, step_ms: u64) -> Self {
        self.next_ts = start_ts;
        self.step_ms = step_ms;
        self
    }

    /// 4 位说话人轮流发言的演示脚本，无限循环（冒烟演示用）。
    pub fn demo() -> Self {
        let script = vec![
            utterance(1, Gender::Female, "好的，那我们今天主要讨论一下新项目的排期问题。"),
            utterance(2, Gender::Male, "嗯，我的想法是先把基础架构搭起来，再做功能，这样后面迭代会快一些。"),
            utterance(3, Gender::Male, "我觉得可以，不过预算方面还需要再确认一下。"),
            utterance(4, Gender::Female, "预算表我这边已经在整理了，下午应该能发给大家。"),
            utterance(1, Gender::Female, "那这个周五之前能出第一版吗？"),
            utterance(2, Gender::Male, "应该可以，我这边协调一下人力。"),
            utterance(3, Gender::Male, "接口文档我今晚补完，明天早上给你。"),
            utterance(4, Gender::Female, "好，那我们就按这个节奏推进，有问题随时群里同步。"),
        ];
        Self::new(script, true)
    }
}

impl AsrPort for MockAsrPort {
    fn start(&mut self) {}
    fn stop(&mut self) {}
    fn next_utterance(&mut self) -> Option<Utterance> {
        if self.script.is_empty() {
            return None;
        }
        if self.cursor >= self.script.len() {
            if !self.loop_forever {
                return None;
            }
            self.cursor = 0;
        }
        let mut utt = self.script[self.cursor].clone();
        utt.ts = self.next_ts;
        self.next_ts += self.step_ms;
        self.cursor += 1;
        Some(utt)
    }
}

/// 核心引擎：持有 [AsrPort]，把转写转化为对外事件流（channel 交付）。
///
/// 事件流通过 [std::sync::mpsc::channel] 暴露，驱动方式是外部周期调用
/// [Engine::step]（Tauri 壳在后台线程做这件事）。测试中直接 [Engine::drain]
/// 同步跑完，再断言收到的全部事件。
pub struct Engine {
    asr: Box<dyn AsrPort>,
    tx: mpsc::Sender<EngineEvent>,
    next_segment_id: u64,
    known_speakers: HashSet<u32>,
}

impl Engine {
    /// 创建引擎，返回 (engine, 事件流接收端)。
    pub fn new(asr: Box<dyn AsrPort>) -> (Self, mpsc::Receiver<EngineEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            Self {
                asr,
                tx,
                next_segment_id: 1,
                known_speakers: HashSet::new(),
            },
            rx,
        )
    }

    pub fn start(&mut self) {
        self.asr.start();
    }

    pub fn stop(&mut self) {
        self.asr.stop();
    }

    /// 处理下一条转写；有输入则产出事件并返回 `true`，暂时无输入返回 `false`。
    pub fn step(&mut self) -> bool {
        let Some(utt) = self.asr.next_utterance() else {
            return false;
        };
        self.emit_for_utterance(utt);
        true
    }

    /// 发布一条旁路事件（如实时 partial 结果），与转写事件共用同一事件流。
    pub fn publish(&self, evt: EngineEvent) {
        let _ = self.tx.send(evt);
    }

    /// 连续 [Engine::step] 直到端口暂时无输入（测试/冒烟用）。
    pub fn drain(&mut self) {
        while self.step() {}
    }

    /// 把一条转写转化为事件序列：先 `SpeakerAssigned`（含颜色/性别），再 `SegmentAppended`。
    fn emit_for_utterance(&mut self, utt: Utterance) {
        let segment_id = self.next_segment_id;
        self.next_segment_id += 1;

        let is_new_speaker = !self.known_speakers.contains(&utt.speaker_id);
        if is_new_speaker {
            self.known_speakers.insert(utt.speaker_id);
        }

        // 先发说话人归属：渲染层先登记说话人（颜色/性别），再渲染气泡。
        let _ = self.tx.send(EngineEvent::SpeakerAssigned {
            segment_id,
            speaker_id: utt.speaker_id,
            is_new_speaker,
            color: speaker_color(utt.speaker_id),
            gender: utt.gender,
        });

        let segment = Segment {
            id: segment_id,
            speaker_id: utt.speaker_id,
            raw: utt.text,
            status: SegmentStatus::Active,
            cleaned: None,
            edit_id: None,
            ts: utt.ts,
            retries: 0,
        };
        let _ = self.tx.send(EngineEvent::SegmentAppended { segment });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取走接收端里全部已缓冲事件（不阻塞）。
    fn collect(rx: &mpsc::Receiver<EngineEvent>) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            out.push(evt);
        }
        out
    }

    fn run_script(script: Vec<Utterance>) -> (Vec<EngineEvent>, Vec<Segment>, Vec<(u32, String, bool)>) {
        let (mut engine, rx) = Engine::new(Box::new(MockAsrPort::new(script, false)));
        engine.start();
        engine.drain();
        let events = collect(&rx);

        let segments: Vec<Segment> = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::SegmentAppended { segment } => Some(segment.clone()),
                _ => None,
            })
            .collect();

        let assignments: Vec<(u32, String, bool)> = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::SpeakerAssigned {
                    speaker_id,
                    color,
                    is_new_speaker,
                    ..
                } => Some((*speaker_id, color.clone(), *is_new_speaker)),
                _ => None,
            })
            .collect();

        (events, segments, assignments)
    }

    #[test]
    fn palette_has_distinct_colors() {
        assert!(SPEAKER_PALETTE.len() >= 5, "调色板至少 5 色");
        assert!(SPEAKER_PALETTE.len() <= 8, "调色板最多 8 色");
        let unique: HashSet<&str> = SPEAKER_PALETTE.iter().copied().collect();
        assert_eq!(unique.len(), SPEAKER_PALETTE.len(), "调色板颜色应互不相同");
    }

    #[test]
    fn speaker_colors_are_stable_and_distinct() {
        // 同一说话人颜色恒定
        assert_eq!(speaker_color(1), speaker_color(1));
        assert_eq!(speaker_color(2), speaker_color(2));
        // 不同说话人颜色不同（在调色板范围内）
        assert_ne!(speaker_color(1), speaker_color(2));
        assert_ne!(speaker_color(3), speaker_color(4));
        // 越界 id 取模回绕但仍稳定
        assert_eq!(speaker_color(1 + 8 * 3), speaker_color(1));
    }

    #[test]
    fn mock_loops_when_loop_forever() {
        let script = vec![
            utterance(1, Gender::Female, "第一句"),
            utterance(2, Gender::Male, "第二句"),
        ];
        let mut port = MockAsrPort::new(script, true).with_clock(100, 100);
        let a = port.next_utterance().unwrap();
        let b = port.next_utterance().unwrap();
        let c = port.next_utterance().unwrap(); // 回到脚本开头
        assert_eq!(a.text, "第一句");
        assert_eq!(b.text, "第二句");
        assert_eq!(c.text, "第一句");
        // 时间钟递增
        assert!(a.ts < b.ts && b.ts < c.ts);
    }

    /// 验收标准 #3：喂合成事件，断言输出事件流（片段追加 + 说话人归属）。
    #[test]
    fn feed_utterances_emits_ordered_events() {
        let script = vec![
            utterance(1, Gender::Female, "你好，请问会议几点开始？"),
            utterance(2, Gender::Male, "十点开始，在一号会议室。"),
            utterance(1, Gender::Female, "好的，我会准时到。"),
        ];
        let (events, segments, assignments) = run_script(script);

        // 每条转写产出两条事件：先 SpeakerAssigned，再 SegmentAppended
        assert_eq!(events.len(), 6, "3 条转写 → 6 条事件");
        for pair in events.chunks(2) {
            assert!(matches!(pair[0], EngineEvent::SpeakerAssigned { .. }));
            assert!(matches!(pair[1], EngineEvent::SegmentAppended { .. }));
        }

        // 片段 id 单调递增且说话人归属正确
        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            segments.iter().map(|s| s.speaker_id).collect::<Vec<_>>(),
            vec![1, 2, 1]
        );
        assert_eq!(
            segments.iter().map(|s| s.raw.as_str()).collect::<Vec<_>>(),
            vec!["你好，请问会议几点开始？", "十点开始，在一号会议室。", "好的，我会准时到。"]
        );
        assert_eq!(
            segments.iter().map(|s| s.status).collect::<Vec<_>>(),
            vec![SegmentStatus::Active, SegmentStatus::Active, SegmentStatus::Active]
        );

        // 说话人归属与片段一一对应
        assert_eq!(
            assignments.iter().map(|(sid, _, _)| *sid).collect::<Vec<_>>(),
            vec![1, 2, 1]
        );
    }

    /// 验收标准 #4：同一说话人颜色一致，不同说话人颜色不同。
    #[test]
    fn same_speaker_same_color_different_speaker_different_color() {
        let script = vec![
            utterance(1, Gender::Female, "a"),
            utterance(2, Gender::Male, "b"),
            utterance(3, Gender::Male, "c"),
            utterance(1, Gender::Female, "d"),
            utterance(2, Gender::Male, "e"),
        ];
        let (_, _, assignments) = run_script(script);

        // 颜色按说话人取模稳定：1→x, 2→y, 3→z，1 再来仍是 x
        let color_of = |sid: u32| assignments.iter().find(|(s, _, _)| *s == sid).unwrap().1.clone();
        assert_eq!(color_of(1), color_of(1));
        assert_eq!(color_of(2), color_of(2));
        assert_ne!(color_of(1), color_of(2));
        assert_ne!(color_of(1), color_of(3));
        assert_ne!(color_of(2), color_of(3));
    }

    /// is_new_speaker：首次出现为 true，之后为 false。
    #[test]
    fn is_new_speaker_flag() {
        let script = vec![
            utterance(1, Gender::Female, "a"),
            utterance(2, Gender::Male, "b"),
            utterance(1, Gender::Female, "c"),
        ];
        let (_, _, assignments) = run_script(script);
        assert_eq!(
            assignments.iter().map(|(_, _, n)| *n).collect::<Vec<_>>(),
            vec![true, true, false]
        );
    }

    /// 事件序列化契约：type 标签 + camelCase（前端解析依赖此形状）。
    #[test]
    fn event_serializes_with_type_tag_and_camel_case() {
        let (mut engine, rx) = Engine::new(Box::new(MockAsrPort::new(
            vec![utterance(1, Gender::Female, "测试")],
            false,
        )));
        engine.start();
        engine.drain();
        let events = collect(&rx);
        let json = serde_json::to_string(&events[0]).unwrap();
        assert!(json.contains("\"type\":\"speakerAssigned\""), "got: {json}");
        assert!(json.contains("\"isNewSpeaker\""), "got: {json}");
        let json2 = serde_json::to_string(&events[1]).unwrap();
        assert!(json2.contains("\"type\":\"segmentAppended\""), "got: {json2}");
        assert!(json2.contains("\"speakerId\""), "got: {json2}");
    }
}
