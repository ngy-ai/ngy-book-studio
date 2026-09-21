"""把 .ico 里的每个尺寸并排渲染出来，用于定稿前肉眼检查。

用法：

    python tools/preview_sizes.py assets/icon.ico target/icon_preview.png

输出三行：

  1. 放大版（NEAREST）——看像素结构，小尺寸下图形还剩多少细节
  2. 浅色底实际大小——看真实观感
  3. 深色底实际大小——看轮廓会不会被深色任务栏吃掉（约 #202020）

只有 256px 的成品好看没用。16×16 才是资源管理器和任务栏的真实渲染尺寸。
"""

from __future__ import annotations

import argparse
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ZOOM = 4  # 大尺寸的放大倍数
PAD = 14
GAP = 16
LABEL_H = 18
LIGHT_BG = (243, 243, 245)
DARK_BG = (32, 32, 32)  # 接近 Windows 深色任务栏
INK = (70, 70, 78)
INK_ON_DARK = (225, 225, 230)

# 检查这些尺寸就够：任务栏用 32、资源管理器小图标用 16、大图标用 256。
CHECK_SIZES = (16, 24, 32, 48, 64, 128, 256)


def zoom_for(size: int) -> int:
    """小尺寸放大更多倍，否则在最需要看清的 16px 上什么也看不出来。

    各列宽度按自身放大后的尺寸算，不统一取最大值——统一会让 16px 那列
    在一大片空白里孤零零挂着一个点。
    """
    if size <= 32:
        return ZOOM * 2
    if size <= 64:
        return ZOOM + 2
    return ZOOM

# 位图默认字体只认 ASCII，中文标签会变成方块/乱码，所以要显式找一套中文字体。
CJK_FONTS = (
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
    "/System/Library/Fonts/PingFang.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
)


def pick_font(size: int = 13) -> tuple[ImageFont.ImageFont, bool]:
    """返回 (字体, 是否支持中文)。找不到中文字体时调用方要用英文标签。"""
    for path in CJK_FONTS:
        if Path(path).is_file():
            try:
                return ImageFont.truetype(path, size), True
            except OSError:
                continue
    return ImageFont.load_default(), False


def largest(source: Image.Image) -> Image.Image:
    """取源图里最大的一帧作为缩放母版（ICO 内部帧可能乱序）。"""
    frames = getattr(source, "n_frames", 1)
    if frames <= 1:
        return source.convert("RGBA")
    best = None
    for index in range(frames):
        source.seek(index)
        frame = source.convert("RGBA")
        if best is None or frame.width > best.width:
            best = frame
    source.seek(0)
    return best


def main() -> None:
    parser = argparse.ArgumentParser(description="多尺寸图标预览")
    parser.add_argument("icon", type=Path, help="源文件（.ico 或 .png）")
    parser.add_argument("out", type=Path, help="输出 png 路径")
    args = parser.parse_args()

    with Image.open(args.icon) as opened:
        base = largest(opened)

    sizes = [s for s in CHECK_SIZES if s <= base.width]
    if not sizes:
        sizes = [base.width]

    # 每列宽度按自身放大后的尺寸算，横向排布。
    columns: list[tuple[int, int, int]] = []  # (左边界, 尺寸, 放大后的宽度)
    x = PAD
    for size in sizes:
        width_zoomed = size * zoom_for(size)
        columns.append((x, size, width_zoomed))
        x += width_zoomed + GAP
    width = x - GAP + PAD

    big_h = max(size * zoom_for(size) for size in sizes)
    scale_h = max(sizes)  # 实际大小那行按最大尺寸留高，各行才对齐

    # 行标题独占一行，图标行不与标题同 y——否则第一列的图标会压到标题文字上。
    y_big = PAD + LABEL_H
    y_px = y_big + big_h
    y_title_light = y_px + LABEL_H + GAP
    y_light = y_title_light + LABEL_H
    y_title_dark = y_light + scale_h + GAP
    y_dark = y_title_dark + LABEL_H
    height = y_dark + scale_h + PAD

    canvas = Image.new("RGB", (width, height), LIGHT_BG)
    draw = ImageDraw.Draw(canvas)
    font, cjk = pick_font()

    # 深色底那一条整行铺满，模拟任务栏
    draw.rectangle((0, y_title_dark, width, height), fill=DARK_BG)

    labels = (
        (f"放大（像素结构，{ZOOM}–{ZOOM * 2}x）", "浅底 · 实际大小",
         "深底 · 实际大小（轮廓能否分辨）")
        if cjk
        else (
            f"Zoom ({ZOOM}-{ZOOM * 2}x, pixel structure)",
            "Actual size · light",
            "Actual size · dark (outline visible?)",
        )
    )
    draw.text((PAD, PAD), labels[0], fill=INK, font=font)
    draw.text((PAD, y_title_light), labels[1], fill=INK, font=font)
    draw.text((PAD, y_title_dark), labels[2], fill=INK_ON_DARK, font=font)

    def paste_centered(image: Image.Image, left: int, column_w: int, top: int) -> None:
        canvas.paste(image, (left + (column_w - image.width) // 2, top), image)

    for left, size, width_zoomed in columns:
        small = base.resize((size, size), Image.Resampling.LANCZOS)
        zoom = zoom_for(size)
        enlarged = small.resize((size * zoom, size * zoom), Image.Resampling.NEAREST)

        # 放大版在行内底部对齐——星都落在同一条基线上，才好比细节
        canvas.paste(enlarged, (left, y_big + big_h - enlarged.height))
        draw.text((left, y_px + 2), f"{size}px", fill=INK, font=font)
        paste_centered(small, left, width_zoomed, y_light + (scale_h - size) // 2)
        paste_centered(small, left, width_zoomed, y_dark + (scale_h - size) // 2)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    canvas.save(args.out)
    zooms = ", ".join(f"{size}px×{zoom_for(size)}" for size in sizes)
    print(f"已写入 {args.out}（{canvas.width}x{canvas.height}）\n  检查尺寸：{zooms}")


if __name__ == "__main__":
    main()
