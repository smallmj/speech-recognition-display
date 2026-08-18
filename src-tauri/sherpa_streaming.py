#!/usr/bin/env python3
"""
sherpa_streaming.py — sherpa-onnx 流式 ASR sidecar。

与 Rust (Tauri) 端通过 stdin/stdout 通信：

  Rust → Python (stdin)
    第 1 行    : JSON 配置行  {"type":"config","sample_rate":16000}
    之后       : 二进制 little-endian float32 PCM（单声道，16kHz）
    stdin EOF  : 触发优雅关闭（flush 当前 final 结果后退出）

  Python → Rust (stdout)  每行一条 NDJSON：
    {"type":"started","streaming":true,"model":"...","sample_rate":16000}
    {"type":"partial","text":"..."}   识别中间结果（边说边出）
    {"type":"final","text":"..."}     一句话定稿（端点/静音触发）
    {"type":"error","message":"..."}  错误
    {"type":"stopped"}

用法（独立运行，验证 ASR 链路）：
  python3 sherpa_streaming.py \
      --model-dir ./asr-models/sherpa-onnx-x-asr-960ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05 \
      --wav ./asr-models/.../test_wavs/0.wav \
      > /tmp/asr.out
"""

from __future__ import annotations

import argparse
import json
import re
import struct
import sys
import wave
from pathlib import Path

# 让 `python3 sherpa_streaming.py` 也能用上项目 venv 里的 sherpa_onnx（调试便利）。
_venv_pkgs = Path(__file__).resolve().parent / ".venv" / "lib" / "python3.11" / "site-packages"
if _venv_pkgs.is_dir() and str(_venv_pkgs) not in sys.path:
    sys.path.insert(0, str(_venv_pkgs))

import numpy as np  # noqa: E402
import sherpa_onnx  # noqa: E402

CJK = r"\u4e00-\u9fff\u3400-\u4dbf\uf900-\ufaff"
CJK_PUNCT = "，。！？；：、（）【】《》「」『』"

# ---------------------------------------------------------------------------
# 文本后处理：把 BPE 分词的空格合并成可读文本（中文不空格、英文保空格）
# ---------------------------------------------------------------------------


def normalize_bpe_text(text: str) -> str:
    """把 sherpa-onnx BPE 输出（如 "昨 天 是 Monday"）规范为可读文本。"""
    text = re.sub(r"\s+", " ", text).strip()
    # 中文-中文之间去空格
    text = re.sub(rf"(?<=[{CJK}]) (?=[{CJK}])", "", text)
    # 中文与中文标点之间去空格
    text = re.sub(rf"(?<=[{CJK}]) (?=[{CJK_PUNCT}])", "", text)
    text = re.sub(rf"(?<=[{CJK_PUNCT}]) (?=[{CJK}])", "", text)
    return text


# ---------------------------------------------------------------------------
# 流式识别器（OnlineRecognizer，真流式，边说边出）
# ---------------------------------------------------------------------------


class StreamingRecognizer:
    """基于 sherpa-onnx OnlineRecognizer 的真流式识别器。

    - 每次 feed 后对 ready 的 stream 做 decode，把中间结果作为 partial 输出；
    - 端点检测（enable_endpoint_detection）触发时输出 final，并 reset 开启新句。
    """

    def __init__(self, model_dir: str, num_threads: int = 2, sample_rate: int = 16000):
        self.model_dir = Path(model_dir)
        self.sample_rate = sample_rate

        # x-asr streaming zipformer transducer（BPE）文件布局
        encoder = self.model_dir / "encoder.int8.onnx"
        decoder = self.model_dir / "decoder.onnx"
        joiner = self.model_dir / "joiner.int8.onnx"
        tokens = self.model_dir / "tokens.txt"
        bpe = self.model_dir / "bpe.model"

        if not all(p.is_file() for p in (encoder, decoder, joiner, tokens, bpe)):
            raise FileNotFoundError(
                f"流式模型文件不完整，期望 {self.model_dir} 内含 "
                "encoder.int8.onnx / decoder.onnx / joiner.int8.onnx / tokens.txt / bpe.model"
            )

        self.recognizer = sherpa_onnx.OnlineRecognizer.from_transducer(
            tokens=str(tokens),
            encoder=str(encoder),
            decoder=str(decoder),
            joiner=str(joiner),
            num_threads=num_threads,
            sample_rate=int(sample_rate),
            feature_dim=80,
            decoding_method="greedy_search",
            modeling_unit="bpe",
            bpe_vocab=str(bpe),
            enable_endpoint_detection=True,
            rule1_min_trailing_silence=1.2,
            rule2_min_trailing_silence=0.6,
            rule3_min_utterance_length=12.0,
        )
        self.stream = self.recognizer.create_stream()

    # -- feed + decode ------------------------------------------------------

    def feed(self, samples: np.ndarray):
        self.stream.accept_waveform(self.sample_rate, samples)
        return self.decode()

    def decode(self) -> str:
        """decode ready 数据，返回当前 partial 文本（未定稿）。"""
        while self.recognizer.is_ready(self.stream):
            self.recognizer.decode_stream(self.stream)
        return normalize_bpe_text(self.recognizer.get_result(self.stream))

    def maybe_finalize(self) -> str | None:
        """若检测到端点（一句话说完），返回定稿文本并 reset 新句；否则 None。"""
        if self.recognizer.is_endpoint(self.stream):
            text = normalize_bpe_text(self.recognizer.get_result(self.stream))
            self.recognizer.reset(self.stream)
            return text
        return None

    def finish(self) -> str | None:
        """输入结束：补尾部静音强制出最终结果。返回定稿文本。"""
        # 补足一个完整解码块（模型 chunk=960ms）+ 余量，让尾部词完整出结果
        tail = np.zeros(int(1.5 * self.sample_rate), dtype=np.float32)
        self.stream.accept_waveform(self.sample_rate, tail)
        self.stream.input_finished()
        while self.recognizer.is_ready(self.stream):
            self.recognizer.decode_stream(self.stream)
        text = normalize_bpe_text(self.recognizer.get_result(self.stream))
        return text or None


# ---------------------------------------------------------------------------
# 主循环：stdin 协议
# ---------------------------------------------------------------------------


def emit(obj: dict):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def run_streaming(stdin, model_dir: str, sample_rate: int):
    # -- 读配置行：逐字节直到换行，避免把后续二进制音频吞进缓冲
    line = b""
    while True:
        b = stdin.read(1)
        if not b or b == b"\n":
            break
        line += b
    if line:
        try:
            cfg = json.loads(line.decode("utf-8"))
            sample_rate = int(cfg.get("sample_rate", sample_rate))
        except (ValueError, json.JSONDecodeError):
            emit({"type": "error", "message": f"无法解析配置行: {line!r}"})
            sys.exit(1)

    rec = StreamingRecognizer(model_dir=model_dir, sample_rate=sample_rate)
    emit(
        {
            "type": "started",
            "streaming": True,
            "model": rec.model_dir.name,
            "sample_rate": rec.sample_rate,
        }
    )

    bytes_per_sample = 4  # float32
    buf = b""
    last_partial = ""

    while True:
        chunk = stdin.read(1600 * bytes_per_sample)  # 100ms @16k
        if not chunk:
            break
        buf += chunk
        if len(buf) >= 1600 * bytes_per_sample:
            n = (len(buf) // (1600 * bytes_per_sample)) * (1600 * bytes_per_sample)
            samples = np.frombuffer(buf[:n], dtype=np.float32).copy()
            buf = buf[n:]
            partial = rec.feed(samples)
            if partial and partial != last_partial:
                emit({"type": "partial", "text": partial})
                last_partial = partial
            final = rec.maybe_finalize()
            if final:
                emit({"type": "final", "text": final})
                last_partial = ""

    # stdin EOF → 优雅关闭
    final = rec.finish()
    if final:
        emit({"type": "final", "text": final})
    emit({"type": "stopped"})


# ---------------------------------------------------------------------------
# 独立运行（--wav）：不依赖 Rust，直接把识别结果打印到 stdout，验证 ASR 链路
# ---------------------------------------------------------------------------


def run_wav(wav_path: str, model_dir: str, sample_rate: int):
    rec = StreamingRecognizer(model_dir=model_dir, sample_rate=sample_rate)
    with wave.open(wav_path, "rb") as w:
        sr = w.getframerate()
        data = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(
            np.float32
        ) / 32768.0

    print(f"[sherpa_streaming] wav={wav_path} sr={sr} dur={len(data)/sr:.2f}s", file=sys.stderr)
    chunk = int(0.1 * sr)
    last = ""
    for i in range(0, len(data), chunk):
        partial = rec.feed(data[i : i + chunk])
        if partial and partial != last:
            print(f"PARTIAL: {partial}", flush=True)
            last = partial
        final = rec.maybe_finalize()
        if final:
            print(f"FINAL: {final}", flush=True)
            last = ""
    final = rec.finish()
    if final:
        print(f"FINAL: {final}", flush=True)


def main():
    parser = argparse.ArgumentParser(description="sherpa-onnx 流式 ASR sidecar")
    parser.add_argument(
        "--model-dir",
        required=True,
        help="流式模型目录（含 encoder/decoder/joiner/tokens/bpe.model）",
    )
    parser.add_argument("--sample-rate", type=int, default=16000)
    parser.add_argument(
        "--wav",
        default="",
        help="独立验证模式：识别该 wav 并打印结果（不需要 Rust stdin）",
    )
    args = parser.parse_args()

    if args.wav:
        run_wav(args.wav, args.model_dir, args.sample_rate)
    else:
        try:
            run_streaming(sys.stdin.buffer, args.model_dir, args.sample_rate)
        except Exception as e:  # noqa: BLE001
            emit({"type": "error", "message": f"{type(e).__name__}: {e}"})
            sys.exit(1)


if __name__ == "__main__":
    main()
