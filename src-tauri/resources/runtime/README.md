# 打包版 ASR 运行时

`runtime/` 在打包构建时由 `scripts/package-runtime.mjs` 生成并随 app 分发。

预期布局：

```text
runtime/
├── sherpa_streaming.py
└── venv/
    ├── bin/python3        （Windows: Scripts/python.exe）
    └── lib/...            （含 sherpa-onnx、numpy）
```

该目录只读使用；首次运行下载的 ASR / embedding 模型写入 app 数据目录。
正式分发建议用自包含 Python 替换 `venv/`，避免依赖构建机系统 Python。
