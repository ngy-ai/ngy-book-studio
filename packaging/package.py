#!/usr/bin/env python3
"""把 release 构建产物打成三平台发布包。

CI 与本地共用**这一份**实现 —— 在 workflow 里写只存在于 CI 的打包命令，那段代码
永远不会被本地跑过，它坏掉的方式你在发布当天才第一次看到。

用法（本地）:

    python packaging/package.py --target x86_64-pc-windows-msvc --version 0.1.0

参数:
    --target   Rust target triple，决定走哪条平台分支
    --version  版本号，必须与 Cargo.toml 一致（CI 里 tag 触发时额外校验与 tag 相同）
    --out      产物输出目录，默认 dist
"""

from __future__ import annotations

import argparse
import os
import plistlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

# 两个流先变成 UTF-8：CI runner 的 stdout 用的是控制台代码页（en-US → cp1252），
# 中文 print 会抛 UnicodeEncodeError —— 而且崩在 zip 已经写完之后的最后一行。
# 这是脚本自己的责任，不指望调用方记得设 PYTHONIOENCODING。
for _stream in (sys.stdout, sys.stderr):
    _stream.reconfigure(encoding="utf-8", errors="replace")

BIN_NAME = "ngy-book-studio"
APP_NAME = "墨页"
BUNDLE_ID = "com.ngy.moye"
REPO_ROOT = Path(__file__).resolve().parent.parent

# 每个包都带的文档。用户拿到的是一个压缩包，不是仓库。
DOC_FILES = ["LICENSE", "README.md"]


def log(message: str) -> None:
    print(message, flush=True)


def fail(message: str) -> "NoReturn":  # type: ignore[valid-type]
    print(f"错误：{message}", file=sys.stderr)
    raise SystemExit(1)


def cargo_version() -> str:
    """从 Cargo.toml 读版本号，作为唯一事实来源。"""
    for line in (REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("version") and "=" in stripped and not stripped.startswith("version_"):
            return stripped.split("=", 1)[1].strip().strip('"')
    fail("Cargo.toml 里找不到 version")


def staging_root(version: str, target: str) -> str:
    """顶层目录名与压缩包同名（去掉扩展名）：解压出来是一个有名字的文件夹。"""
    platform, arch = split_target(target)
    return f"{BIN_NAME}-{version}-{platform}-{arch}"


def split_target(target: str) -> tuple[str, str]:
    """`x86_64-pc-windows-msvc` → `("windows", "x86_64")`"""
    arch = target.split("-", 1)[0]
    if "windows" in target:
        platform = "windows"
    elif "apple-darwin" in target:
        platform = "macos"
    elif "linux" in target:
        platform = "linux"
    else:
        fail(f"不认识的 target：{target}")
    # Apple 生态里 aarch64 一律叫 arm64。产物名跟着用户熟悉的叫法走，
    # 否则 Release 说明里的下载表会和真实文件名对不上。
    if platform == "macos" and arch == "aarch64":
        arch = "arm64"
    return platform, arch


def release_binary(target: str) -> Path:
    name = f"{BIN_NAME}.exe" if "windows" in target else BIN_NAME
    path = REPO_ROOT / "target" / target / "release" / name
    if not path.is_file():
        fail(f"找不到构建产物 {path}；先跑 cargo build --release --target {target}")
    return path


def make_staging(version: str, target: str) -> Path:
    """建 staging 目录，返回其路径。调用方负责往里放东西。"""
    root = Path(tempfile.mkdtemp(prefix="pkg-")) / staging_root(version, target)
    root.mkdir(parents=True)
    for name in DOC_FILES:
        source = REPO_ROOT / name
        if source.is_file():
            shutil.copy2(source, root / name)
        else:
            log(f"警告：缺少 {name}，包里不会有它")
    return root


# --------------------------------------------------------------------------- #
# Windows
# --------------------------------------------------------------------------- #


def package_windows(version: str, target: str, out: Path) -> Path:
    binary = release_binary(target)
    top = make_staging(version, target)
    shutil.copy2(binary, top / binary.name)

    archive = out / f"{top.name}.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
        for path in sorted(top.rglob("*")):
            # archive 内路径一律 posix：Windows 上 rglob 给反斜杠，
            # 直接塞进去会得到一个在 macOS/Linux 上解不开的包。
            zf.write(path, path.relative_to(top.parent).as_posix())
    return archive


def check_crt_static(binary: Path) -> None:
    """确认 CRT 是静态链接的 —— 否则用户没装 VC++ 运行库就打不开。"""
    data = binary.read_bytes()
    leaked = [dll for dll in (b"VCRUNTIME140.dll", b"MSVCP140.dll") if dll in data]
    if leaked:
        log("警告：exe 仍依赖 " + "、".join(d.decode() for d in leaked) + "，未静态链接 CRT")
    else:
        log("CRT 静态链接：导入表里没有 VCRUNTIME140.dll / MSVCP140.dll")


# --------------------------------------------------------------------------- #
# macOS
# --------------------------------------------------------------------------- #


def package_macos(version: str, target: str, out: Path) -> Path:
    binary = release_binary(target)
    top = make_staging(version, target)
    app = top / f"{APP_NAME}.app"
    macos_dir = app / "Contents" / "MacOS"
    resources = app / "Contents" / "Resources"
    macos_dir.mkdir(parents=True)
    resources.mkdir(parents=True)

    shutil.copy2(binary, macos_dir / BIN_NAME)
    os.chmod(macos_dir / BIN_NAME, 0o755)

    icon = REPO_ROOT / "assets" / "icon.png"
    if icon.is_file():
        build_icns(icon, resources / "icon.icns")
    write_info_plist(app / "Contents" / "Info.plist", version)

    ad_hoc_sign(app)

    archive = out / f"{top.name}.zip"
    if archive.exists():
        archive.unlink()
    ditto = shutil.which("ditto")
    if ditto:
        # 只有 ditto 保留 .app 内部的可执行位。用 zipfile 打出来的是
        # 一个「双击没反应」的 app，而发布者本机上是好的 —— 这类问题在发布侧看不见。
        subprocess.run(
            [ditto, "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(archive)],
            check=True,
        )
        # ditto 只压了 .app 本身，把文档补进去。
        with zipfile.ZipFile(archive, "a", zipfile.ZIP_DEFLATED) as zf:
            for path in sorted(top.iterdir()):
                if path.name.endswith(".app"):
                    continue
                zf.write(path, f"{top.name}/{path.name}")
        # `--keepParent` 的语义是「把源路径的父目录名嵌进去」，所以 .app 会落在
        # `<顶层目录>/墨页.app/` 下、和我补进去的文档同级。这条只能真在 macOS 上跑才
        # 能确认，所以在脚本里断言，不要等到用户解压出一堆散文件才发现。
        names = list_names(archive)
        stray = [n for n in names if not n.startswith(f"{top.name}/")]
        if stray:
            fail(f"ditto 产出的顶层目录不是 {top.name}/，散落在外的条目：{stray[:5]}")
    else:
        log("警告：找不到 ditto（非 macOS 宿主？），退回 zipfile —— 可执行位可能丢失")
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
            for path in sorted(top.rglob("*")):
                zf.write(path, path.relative_to(top.parent).as_posix())
    return archive


def build_icns(source: Path, dest: Path) -> bool:
    """用 macOS 自带的 sips + iconutil 生成 .icns。缺工具就跳过，不静默降级。"""
    sips, iconutil = shutil.which("sips"), shutil.which("iconutil")
    if not (sips and iconutil):
        log("警告：缺 sips/iconutil（非 macOS 宿主），跳过 icns；Finder 里会没有图标")
        return False
    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "icon.iconset"
        iconset.mkdir()
        # 源图小于 1024 时 512/1024 两档是放大的，Retina Dock 里略糊 ——
        # 宁可略糊，也别留一个「图标不显示」的空位。
        for px in (16, 32, 128, 256, 512):
            for scale, suffix in ((1, ""), (2, "@2x")):
                subprocess.run(
                    [sips, "-z", str(px * scale), str(px * scale), str(source),
                     "--out", str(iconset / f"icon_{px}x{px}{suffix}.png")],
                    check=True, capture_output=True,
                )
        subprocess.run([iconutil, "-c", "icns", str(iconset), "-o", str(dest)], check=True)
    return True


def write_info_plist(dest: Path, version: str) -> None:
    plist = {
        "CFBundleName": APP_NAME,
        "CFBundleDisplayName": APP_NAME,
        "CFBundleIdentifier": BUNDLE_ID,
        # 必须等于 Contents/MacOS/ 下的文件名
        "CFBundleExecutable": BIN_NAME,
        "CFBundlePackageType": "APPL",
        "CFBundleIconFile": "icon.icns",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        # 不满足时系统直接说「需要更新系统」，而不是启动到一半崩掉
        "LSMinimumSystemVersion": "11.0",
        "NSHighResolutionCapable": True,
        # 没有它，Finder 的「打开方式」里根本不会出现本程序，Dock 拖放也不接收文件。
        "CFBundleDocumentTypes": [
            {
                "CFBundleTypeName": name,
                "CFBundleTypeRole": "Viewer",
                # Alternate = 不抢系统默认（Preview.app / Books.app）的位置
                "LSHandlerRank": "Alternate",
                "LSItemContentTypes": types,
            }
            for name, types in (
                ("EPUB", ["org.idpf.epub-container"]),
                ("PDF", ["com.adobe.pdf"]),
                ("DjVu", ["com.djvuzone.djvu", "public.djvu"]),
                ("Word", ["org.openxmlformats.wordprocessingml.document",
                          "com.microsoft.word.doc"]),
                ("PowerPoint", ["org.openxmlformats.presentationml.presentation"]),
                ("Excel", ["org.openxmlformats.spreadsheetml.sheet"]),
                ("Kindle", ["com.amazon.mobi8-ebook", "com.amazon.azw"]),
                ("网页归档", ["public.html", "public.xhtml"]),
            )
        ],
    }
    dest.write_bytes(plistlib.dumps(plist))


def ad_hoc_sign(app: Path) -> None:
    """ad-hoc 签名，**先内后外**。Apple Silicon 的内核拒绝运行完全没有签名的二进制。"""
    codesign = shutil.which("codesign")
    if not codesign:
        log("警告：找不到 codesign（非 macOS 宿主），跳过签名；Apple Silicon 上会无法运行")
        return
    inner = app / "Contents" / "MacOS" / BIN_NAME
    subprocess.run([codesign, "--force", "--sign", "-", str(inner)], check=True)
    subprocess.run([codesign, "--force", "--sign", "-", str(app)], check=True)


# --------------------------------------------------------------------------- #
# Linux
# --------------------------------------------------------------------------- #


def package_linux(version: str, target: str, out: Path) -> Path:
    binary = release_binary(target)
    top = make_staging(version, target)
    shutil.copy2(binary, top / BIN_NAME)
    (top / f"{BIN_NAME}.desktop").write_text(DESKTOP_ENTRY, encoding="utf-8")

    archive = out / f"{top.name}.tar.gz"
    with tarfile.open(archive, "w:gz") as tf:
        for path in sorted(top.rglob("*")):
            arcname = path.relative_to(top.parent).as_posix()
            info = tf.gettarinfo(str(path), arcname=arcname)
            # 权限位写死：chmod 在 Windows 上基本无效，读盘会得到 666 ——
            # 用户放进 PATH 后报「权限不够」，而打包侧看不出任何异常。
            info.mode = 0o755 if (path.is_dir() or path.name == BIN_NAME) else 0o644
            if path.is_dir():
                tf.addfile(info)
            else:
                with open(path, "rb") as handle:
                    tf.addfile(info, handle)
    return archive


# `%f` 不能省：它让文件管理器把「被双击的那个文件路径」作为参数传进来。
DESKTOP_ENTRY = f"""[Desktop Entry]
Type=Application
Name={APP_NAME}
Comment=本地图书库、阅读器与编辑器
Exec={BIN_NAME} %f
Icon={BIN_NAME}
Terminal=false
Categories=Office;Viewer;
MimeType=application/epub+zip;application/pdf;image/vnd.djvu;\
application/vnd.openxmlformats-officedocument.wordprocessingml.document;\
application/vnd.openxmlformats-officedocument.presentationml.presentation;\
application/vnd.openxmlformats-officedocument.spreadsheetml.sheet;\
application/x-mobipocket-ebook;
"""


# --------------------------------------------------------------------------- #
# 校验
# --------------------------------------------------------------------------- #


def report(archive: Path) -> None:
    """打完看包的大小与条目 —— 某个平台的包突然变小，通常是有东西没打进去。"""
    size_mb = archive.stat().st_size / 1024 / 1024
    names = list_names(archive)
    log(f"产物：{archive.name}（{size_mb:.1f} MiB，{len(names)} 个条目）")
    if len(names) < 3:
        fail(f"{archive.name} 里只有 {len(names)} 个条目，明显有东西没打进去")
    for name in names[:6]:
        log(f"  · {name}")


def list_names(archive: Path) -> list[str]:
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zf:
            return zf.namelist()
    with tarfile.open(archive, "r:gz") as tf:
        return tf.getnames()


# --------------------------------------------------------------------------- #


def main() -> None:
    parser = argparse.ArgumentParser()
    # --version 可选：不传就以 Cargo.toml 为准。tag 与版本是否一致由 CI 单独校验，
    # 传了就顺手对一遍，避免打出「文件名写着 A、资源段里写着 B」的包。
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", default=None)
    parser.add_argument("--out", default="dist")
    args = parser.parse_args()

    version = cargo_version()
    if args.version and args.version != version:
        fail(f"版本不一致：传入 {args.version}，Cargo.toml 是 {version}")

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    platform, _ = split_target(args.target)
    if platform == "windows":
        binary = release_binary(args.target)
        check_crt_static(binary)
        archive = package_windows(version, args.target, out)
    elif platform == "macos":
        archive = package_macos(version, args.target, out)
    else:
        archive = package_linux(version, args.target, out)

    report(archive)
    log(f"完成：{archive}")


if __name__ == "__main__":
    main()
