"""检查图标与版本信息是否真的进了 Windows exe。

用法：

    python tools/verify_embedded.py target/debug/myapp.exe \
        --ico assets/icon.ico --string "我的应用"

给出三份互相独立的证据，缺一不可：

  1. PE 资源段目录项非空（解析可选头 DataDirectory[2]，RVA 与 Size 都非 0）
  2. ICO 里每个尺寸的图像块都能在 exe 字节流里原样找到
     —— 资源段是原样拷贝，命中即证明嵌入生效
  3. VERSIONINFO 里的中文串按 UTF-16LE 编码命中（不是 UTF-8）

为什么不用 PowerShell：`Add-Type` / `Get-Process` 这类手段在受限环境下常被安全策略
拦掉，表现是没输出或者 COM 报错，容易误判成「图标没嵌进去」。直接读字节不依赖任何
系统组件。

退出码 0 = 全部通过，1 = 有检查未通过。
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

IMAGE_DIRECTORY_ENTRY_RESOURCE = 2


def read_resource_dir(exe: bytes) -> tuple[int, int] | None:
    """返回 PE 可选头里资源目录项的 (RVA, Size)。非 PE 或读不到返回 None。"""
    if len(exe) < 0x40 or exe[:2] != b"MZ":
        return None

    (pe_offset,) = struct.unpack_from("<I", exe, 0x3C)
    if exe[pe_offset : pe_offset + 4] != b"PE\0\0":
        return None

    optional = pe_offset + 24
    (magic,) = struct.unpack_from("<H", exe, optional)
    if magic == 0x10B:  # PE32
        dirs_at = optional + 96
    elif magic == 0x20B:  # PE32+
        dirs_at = optional + 112
    else:
        return None

    entry = dirs_at + IMAGE_DIRECTORY_ENTRY_RESOURCE * 8
    if entry + 8 > len(exe):
        return None
    return struct.unpack_from("<II", exe, entry)


def read_ico_entries(ico: bytes) -> list[tuple[int, int, int, int]]:
    """解析 ICONDIR，返回 [(宽, 高, 字节数, 偏移)]。"""
    if len(ico) < 6:
        raise ValueError("文件太短，不是 ICO")
    reserved, kind = struct.unpack_from("<HH", ico, 0)
    if reserved != 0 or kind != 1:
        # 传了 .png 之类的进来会走到这里。直接说不匹配，别让调用方
        # 以为「ICO 目录损坏」而去查编码问题。
        raise ValueError(f"不是 ICO 文件（reserved={reserved}, type={kind}，type 应为 1）")
    (count,) = struct.unpack_from("<H", ico, 4)
    if count == 0:
        raise ValueError("ICO 里没有任何尺寸")
    entries = []
    for index in range(count):
        offset = 6 + index * 16
        if offset + 16 > len(ico):
            raise ValueError("ICO 目录被截断，文件可能没写完")
        width, height, _, _, _, _, size, data_at = struct.unpack_from(
            "<BBBBHHII", ico, offset
        )
        if data_at + size > len(ico):
            raise ValueError(f"第 {index} 个尺寸（{width or 256}px）的数据越界")
        entries.append((width or 256, height or 256, size, data_at))
    return entries


def main() -> None:
    parser = argparse.ArgumentParser(description="校验 exe 资源段里的图标与版本信息")
    parser.add_argument("exe", type=Path)
    parser.add_argument("--ico", type=Path, default=Path("assets/icon.ico"))
    parser.add_argument(
        "--string",
        action="append",
        default=[],
        metavar="TEXT",
        help="期望出现在 VERSIONINFO 里的字符串，可重复",
    )
    args = parser.parse_args()

    failures: list[str] = []

    exe_path: Path = args.exe
    if not exe_path.is_file():
        print(f"✗ 找不到 {exe_path}", file=sys.stderr)
        raise SystemExit(1)
    exe = exe_path.read_bytes()
    print(f"exe  {exe_path}  {len(exe):,} 字节")

    # 证据 1：资源段目录项
    resource = read_resource_dir(exe)
    if resource is None:
        failures.append("exe 不是合法 PE，或可选头读不到资源目录项")
        print("✗ PE 资源段    无法解析（文件不是合法的 Windows 可执行文件？）")
    else:
        rva, size = resource
        if rva and size:
            print(f"✓ PE 资源段    RVA=0x{rva:X}  Size={size:,} 字节")
        else:
            failures.append("PE 资源段为空，说明没有任何资源被嵌入")
            print("✗ PE 资源段    为空（没有资源被嵌入）")

    # 证据 2：逐个尺寸的图像块
    if not args.ico.is_file():
        failures.append(f"找不到 {args.ico}")
        print(f"✗ 图标图像块   找不到 {args.ico}")
    else:
        ico = args.ico.read_bytes()
        try:
            entries = read_ico_entries(ico)
        except ValueError as error:
            failures.append(str(error))
            print(f"✗ 图标图像块   {error}")
            entries = []

        if entries:
            print(f"ico  {args.ico}  {len(ico):,} 字节  含 {len(entries)} 个尺寸")
            hits = 0
            for width, height, size, data_at in entries:
                blob = ico[data_at : data_at + size]
                found = exe.find(blob)
                if found != -1:
                    hits += 1
                    print(f"  ✓ {width:>3}x{height:<3} {size:>7,} 字节  →  exe 偏移 {found:,}")
                else:
                    print(f"  ✗ {width:>3}x{height:<3} {size:>7,} 字节  →  未找到")
            if hits != len(entries):
                failures.append(f"只有 {hits}/{len(entries)} 个图标尺寸进了 exe")
            print(f"结论：{hits}/{len(entries)} 个尺寸进入资源段")

    # 证据 3：VERSIONINFO 里的字符串（UTF-16LE）
    for text in args.string:
        if exe.find(text.encode("utf-16-le")) != -1:
            print(f'✓ VERSIONINFO  "{text}"  已嵌入')
        else:
            failures.append(f'VERSIONINFO 里没有 "{text}"')
            print(f'✗ VERSIONINFO  "{text}"  未找到')

    print()
    if failures:
        print("未通过：")
        for item in failures:
            print(f"  - {item}")
        raise SystemExit(1)
    print("全部通过。注意这只证明字节进了 exe，不证明 Windows 画得好——"
          "任务栏渲染效果需要肉眼确认。")


if __name__ == "__main__":
    main()
