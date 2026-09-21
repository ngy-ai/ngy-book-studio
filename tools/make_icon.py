"""生成应用图标：多尺寸 .ico（供 Windows exe 嵌入）+ png（预览）。

用法：

    python tools/make_icon.py                    # 默认写 ./assets/
    python tools/make_icon.py --assets path/dir

图形画在 4 倍尺寸上再降采样，所以边缘自带抗锯齿，不必手工描边。
改图标 = 改下面那几组常量再跑一次。
"""

from __future__ import annotations

import argparse
from pathlib import Path

from PIL import Image, ImageDraw

BASE = 256  # 最终基准边长
SUPERSAMPLE = 4
CANVAS = BASE * SUPERSAMPLE  # 绘制用画布

# 底色比 Windows 深色任务栏（约 #1F1F1F）亮一档。用纯黑会和任务栏糊在一起，轮廓就没了。
BG_TOP = (56, 68, 92)
BG_BOTTOM = (30, 40, 58)
# 书页用暖金色，在深底上辨识度最高。
PAGE_TOP = (255, 214, 102)
PAGE_BOTTOM = (243, 158, 44)

CORNER_RATIO = 0.22
BOOK_WIDTH_RATIO = 0.80  # 书占画布宽度的比例（摊开的书是横宽的）
BOOK_HEIGHT_RATIO = 0.56  # 书占画布高度的比例。16px 下再小就认不出来了。
SPINE_GAP_RATIO = 0.012  # 书脊处留缝，左右两页才分得开

ICO_SIZES = [(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (16, 16)]


def vertical_gradient(
    size: int, top: tuple[int, int, int], bottom: tuple[int, int, int]
) -> Image.Image:
    """竖直渐变。逐行生成再放大，比逐像素赋值快三个数量级。"""
    column = Image.new("RGB", (1, size))
    pixels = column.load()
    for y in range(size):
        t = y / max(size - 1, 1)
        pixels[0, y] = tuple(round(a + (b - a) * t) for a, b in zip(top, bottom))
    return column.resize((size, size), Image.Resampling.NEAREST)


def rounded_mask(size: int, radius: int) -> Image.Image:
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, size - 1, size - 1), radius=radius, fill=255
    )
    return mask


def book_polygons(
    canvas: int, width_ratio: float, height_ratio: float
) -> list[list[tuple[float, float]]]:
    """一本摊开的书：书脊在正中，左右两页自书脊向外侧倾斜。

    和五角星一样，图形重心不在整体包围盒中心时视觉会下坠，所以这里按
    包围盒（宽 = 书宽，高 = 书高）对称布置，保证落在画布正中。
    书脊一侧的上下沿都比外侧高，才读得出「摊开」而不是两块方块。
    """
    center = canvas / 2
    half_w = canvas * width_ratio / 2
    top = center - canvas * height_ratio / 2
    bottom = center + canvas * height_ratio / 2
    height = bottom - top

    left = center - half_w
    right = center + half_w
    gap = canvas * SPINE_GAP_RATIO

    spine_top = top
    spine_bottom = bottom - height * 0.10
    outer_top = top + height * 0.24
    outer_bottom = bottom

    left_page = [
        (left, outer_top),
        (center - gap, spine_top),
        (center - gap, spine_bottom),
        (left, outer_bottom),
    ]
    right_page = [
        (right, outer_top),
        (center + gap, spine_top),
        (center + gap, spine_bottom),
        (right, outer_bottom),
    ]
    return [left_page, right_page]


def render(canvas: int) -> Image.Image:
    icon = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    icon.paste(
        vertical_gradient(canvas, BG_TOP, BG_BOTTOM),
        (0, 0),
        rounded_mask(canvas, round(canvas * CORNER_RATIO)),
    )

    book_mask = Image.new("L", (canvas, canvas), 0)
    book = ImageDraw.Draw(book_mask)
    for page in book_polygons(canvas, BOOK_WIDTH_RATIO, BOOK_HEIGHT_RATIO):
        book.polygon(page, fill=255)
    icon.paste(vertical_gradient(canvas, PAGE_TOP, PAGE_BOTTOM), (0, 0), book_mask)
    return icon


def build(assets: Path) -> tuple[Path, Path]:
    assets.mkdir(parents=True, exist_ok=True)
    base = render(CANVAS).resize((BASE, BASE), Image.Resampling.LANCZOS)

    png = assets / "icon.png"
    ico = assets / "icon.ico"
    base.save(png)
    # Pillow 会为每个尺寸单独重采样，不要自己先缩好再塞进去。
    base.save(ico, format="ICO", sizes=ICO_SIZES)
    return ico, png


def main() -> None:
    parser = argparse.ArgumentParser(description="生成应用图标")
    parser.add_argument(
        "--assets",
        type=Path,
        default=Path("assets"),
        help="输出目录（默认 ./assets）",
    )
    args = parser.parse_args()

    ico, png = build(args.assets)
    print(f"已写入 {ico}（{len(ICO_SIZES)} 个尺寸）与 {png}")
    print("定稿前跑 tools/preview_sizes.py 看一眼小尺寸效果。")


if __name__ == "__main__":
    main()
