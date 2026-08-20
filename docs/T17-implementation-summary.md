# T17 实现总结：发布管线（自包含运行时 + GitHub Release）

> 目标：打 tag 自动构建 macOS/Windows 安装包（DMG / NSIS）并挂到 GitHub Releases，
> 别人能直接下载安装；正式分发不再依赖构建机上的系统 Python。

## 实现内容

- **自包含 Python 运行时**（`scripts/package-runtime.mjs`）：
  - 新增正式分发模式（`TALKSEE_STANDALONE=1` 或 `TALKSEE_PYTHON_BASE`）：
    按平台/架构下载 [python-build-standalone](https://github.com/astral-sh/python-build-standalone)
    （固定 `cpython-3.12.14+20260814`），解压进 `resources/runtime/venv`，
    pip 安装 `sherpa-onnx==1.13.6` + `numpy` 后随包分发，换机可跑；
  - 下载/解压缓存到 `target/.python-standalone`（幂等，重复构建跳过）；
  - `.talksee-standalone` 标记区分"自包含正式运行时"与"本地开发 venv 拷贝"，
    避免误把软链到系统 Python 的旧产物当正式运行时复用；
  - 复制时 `verbatimSymlinks: true` 保留相对软链（Node 默认会把软链解析成
    绝对路径，换机必挂）。
- **Windows 解释器路径**（`src-tauri/src/model_paths.rs`）：
  - `venv_python` 增加回退：无 `Scripts/python.exe` 时用根目录 `python.exe`
    （python-build-standalone 的 Windows 布局）；测试同步更新。
- **resources 打包修正**（`src-tauri/tauri.conf.json`）：
  - 原 map + glob（`resources/runtime/**/*`）会被 Tauri 拍平成单层目录；
    改为目录映射 `"resources/runtime": "runtime"`，保留 `venv/` 层级；
  - 目标限定 `["dmg", "nsis"]`（去掉 MSI，后续按需加回）。
- **发布工作流**（`.github/workflows/release.yml`）：
  - `push tags v*` + `workflow_dispatch` 触发；
  - 矩阵：macOS arm64（macos-14）/ x86_64（macos-13）+ Windows x64；
    每台跑 `cargo test --workspace` → `pnpm tauri build`（`TALKSEE_STANDALONE=1`）
    → 上传 dmg/exe 产物；
  - `publish` 汇总产物，用 `softprops/action-gh-release` 创建**草稿** Release
    （含安装说明与免签名放行指引），人工确认后发布。
- **版本同步**：`tauri.conf.json` / `Cargo.toml` / `package.json` → v0.3.0。

## 验证

- `TALKSEE_STANDALONE=1 node scripts/package-runtime.mjs`：
  下载 3.12.14 + 安装 sherpa-onnx/numpy 成功；`sys.prefix` 指向 venv 自身（可移植）。
- `cargo test --workspace` 全绿（含新增 `venv_python_prefers_existing_layout`）。
- `pnpm build` 通过（tsc + vite）。
- `TALKSEE_STANDALONE=1 pnpm tauri build` 产出 `TalkSee_0.3.0_aarch64.dmg`；
  挂载后确认 `Resources/runtime/venv/bin/python3` 为真实文件、无坏软链，
  且**用 bundle 内 python 直接跑 sidecar 对真实模型转写成功**（"昨天是 MONDAY …"）。

## 后续（follow-up）

- 代码签名/公证（macOS Developer ID + 公证、Windows 证书），消除安装安全提示；
- 应用内自动更新（Tauri updater，依赖本发布管线）；
- 可选：MSI（winget 分发）、Linux 支持、release 产物缓存（actions/cache）。
