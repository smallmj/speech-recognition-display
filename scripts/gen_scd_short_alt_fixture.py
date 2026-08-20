#!/usr/bin/env python3
"""为 SCD 回归生成「两人快速交替短句 + 房间噪声」的 final NDJSON 流（真实 embedding）。

用法：`<venv>/bin/python scripts/gen_scd_short_alt_fixture.py > /tmp/fixture.ndjson`

每个 final 携带 `speech_duration`（有效语音秒数）与 `embedding`，供 Rust
`examples/scd_emit` 走真实 SCD 三段式判定。固定随机种子 → 输出确定性，
回归可稳定断言「2 个说话人 + 追溯修正」。
"""
import sys
import wave
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src-tauri"))
from sherpa_streaming import SpeakerEmbedder

MODEL_DIR = (
    Path(__file__).resolve().parent.parent
    / "src-tauri/asr-models/sherpa-onnx-x-asr-960ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05"
)
EMB_DIR = (
    Path(__file__).resolve().parent.parent
    / "src-tauri/asr-models/sherpa-onnx-3dspeaker-eres2netv2-base"
)


def load_wav(p):
    with wave.open(str(p), "rb") as w:
        sr = w.getframerate()
        data = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32768.0
    return sr, data


def add_noise(sig, snr_db, seed=0):
    sig_pow = np.mean(sig**2)
    noise_pow = sig_pow / (10 ** (snr_db / 10))
    rng = np.random.default_rng(seed)
    return sig + rng.normal(0, np.sqrt(noise_pow), size=sig.shape).astype(np.float32)


def circular_slice(wav, start, n):
    out = np.empty(n, dtype=np.float32)
    L = len(wav)
    for i in range(n):
        out[i] = wav[(start + i) % L]
    return out


def main():
    sr, w0 = load_wav(MODEL_DIR / "test_wavs/0.wav")  # 说话人 A
    sr, w1 = load_wav(MODEL_DIR / "test_wavs/1.wav")  # 说话人 B
    embd = SpeakerEmbedder(model_dir=str(EMB_DIR), sample_rate=sr)

    # 两人交替，1.5–2.5s 短句（与 PR 校准场景一致），SNR 20dB 房间噪声。
    rng = np.random.default_rng(7)
    turns = [("A" if i % 2 == 0 else "B", float(rng.uniform(1.5, 2.5))) for i in range(14)]
    cursors = {"A": 4000, "B": 4000}
    for i, (who, dur) in enumerate(turns):
        src = w0 if who == "A" else w1
        n = int(dur * sr)
        seg = circular_slice(src, cursors[who], n)
        cursors[who] += n
        seg = add_noise(seg, 20, seed=7 * 100 + cursors[who] // 1600)
        emb = embd.compute(seg)
        if emb is None:
            continue
        print(
            "{\"type\":\"final\",\"text\":\"%s\",\"speech_duration\":%.3f,\"embedding\":[%s]}"
            % (
                f"第{i + 1}句发言",
                dur,
                ",".join(repr(float(x)) for x in emb),
            )
        )


if __name__ == "__main__":
    main()
