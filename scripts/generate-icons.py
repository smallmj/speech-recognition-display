#!/usr/bin/env python3
"""
TalkSee / 语见 — 图标资源批量生成器
源: brand/icon-source.svg (1024x1024)
输出:
  src-tauri/icons/                Tauri 标配图标套件（含桌面 + android/ios）
  brand/                          Logo 矢量资源

依赖（仅 macOS 开发机，一次性生成后产物入库，无需每次构建）:
  rsvg-convert（librsvg）· iconutil（macOS 自带）· ImageMagick（magick）· Pillow（python3 -m pip install Pillow）
"""
from pathlib import Path
import subprocess, shutil, sys
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
SRC_SVG = ROOT / "brand" / "icon-source.svg"
ICONS_DIR = ROOT / "src-tauri" / "icons"
BRAND_DIR = ROOT / "brand"

# --- 0. 准备工作 ------------------------------------------------------------
ICONS_DIR.mkdir(parents=True, exist_ok=True)
BRAND_DIR.mkdir(parents=True, exist_ok=True)

# Tauri 2 标配图标尺寸
TAURI_SIZES = [
    ("32x32.png", 32),
    ("128x128.png", 128),
    ("128x128@2x.png", 256),
    ("icon.png", 1024),
    ("Square30x30Logo.png", 30),
    ("Square44x44Logo.png", 44),
    ("Square71x71Logo.png", 71),
    ("Square89x89Logo.png", 89),
    ("Square107x107Logo.png", 107),
    ("Square142x142Logo.png", 142),
    ("Square150x150Logo.png", 150),
    ("Square284x284Logo.png", 284),
    ("Square310x310Logo.png", 310),
    ("StoreLogo.png", 50),
]

def rsvg(size: int, out: Path) -> None:
    """用 rsvg-convert 渲染指定尺寸 PNG(矢量保真)。"""
    subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(size), str(SRC_SVG), "-o", str(out)],
        check=True,
    )

# --- 1. 渲染所有 PNG --------------------------------------------------------
print("== 渲染 PNG 套件 ==")
for name, size in TAURI_SIZES:
    out = ICONS_DIR / name
    rsvg(size, out)
    print(f"  ✓ {name:30s} {size}x{size:<4d} ({out.stat().st_size:>6d} bytes)")

# --- 2. 渲染 1024 主源(供 .icns / .ico 打包) ------------------------------
MAIN_PNG = ROOT / "brand" / "icon-source-1024.png"
rsvg(1024, MAIN_PNG)
print(f"  ✓ main source 1024x1024      ({MAIN_PNG.stat().st_size} bytes)")

# --- 3. 打包 .icns(macOS,使用 iconutil 标准流程) -------------------------
print("\n== 打包 .icns ==")
ICNSET = ROOT / "build" / "talksee.iconset"
if ICNSET.exists():
    shutil.rmtree(ICNSET)
ICNSET.mkdir()

# iconutil 要求的 .iconset 命名约定
iconset_layout = [
    ("icon_16x16.png",          16),
    ("icon_16x16@2x.png",       32),
    ("icon_32x32.png",          32),
    ("icon_32x32@2x.png",       64),
    ("icon_128x128.png",       128),
    ("icon_128x128@2x.png",    256),
    ("icon_256x256.png",       256),
    ("icon_256x256@2x.png",    512),
    ("icon_512x512.png",       512),
    ("icon_512x512@2x.png",   1024),
]
base = Image.open(MAIN_PNG)
for name, size in iconset_layout:
    img = base.resize((size, size), Image.Resampling.LANCZOS)
    img.save(ICNSET / name)
ICNS_OUT = ICONS_DIR / "icon.icns"
subprocess.run(["iconutil", "-c", "icns", str(ICNSET), "-o", str(ICNS_OUT)], check=True)
shutil.rmtree(ICNSET)
print(f"  ✓ icon.icns (10 sizes)         ({ICNS_OUT.stat().st_size} bytes)")

# --- 4. 打包 .ico(Windows,多分辨率) ---------------------------------------
print("\n== 打包 .ico ==")
ICO_OUT = ICONS_DIR / "icon.ico"
# Pillow 12 已退化为只保存单尺寸,改用 ImageMagick
# 先生成 7 个尺寸的 PNG,然后用 magick 打包
ico_tmp = ROOT / "build" / "ico-tmp"
ico_tmp.mkdir(parents=True, exist_ok=True)
for f in ico_tmp.glob("*.png"):
    f.unlink()
for s in [16, 24, 32, 48, 64, 128, 256]:
    rsvg(s, ico_tmp / f"ico_{s}.png")
subprocess.run(
    ["magick", *sorted(str(p) for p in ico_tmp.glob("*.png")), str(ICO_OUT)],
    check=True, capture_output=True,
)
shutil.rmtree(ico_tmp)
print(f"  ✓ icon.ico (multi-size)        ({ICO_OUT.stat().st_size} bytes)")

# --- 5. 交付校验 ------------------------------------------------------------
print("\n== 交付校验 ==")
required = [
    "src-tauri/icons/32x32.png", "src-tauri/icons/128x128.png", "src-tauri/icons/128x128@2x.png",
    "src-tauri/icons/icon.png", "src-tauri/icons/icon.icns", "src-tauri/icons/icon.ico",
    "src-tauri/icons/StoreLogo.png",
]
for f in required:
    p = ROOT / f
    ok = p.exists() and p.stat().st_size > 0
    print(f"  {'✓' if ok else '✗'} {f:40s} {p.stat().st_size if p.exists() else 'MISSING'} bytes")

print("\n✓ 全部图标生成完毕")