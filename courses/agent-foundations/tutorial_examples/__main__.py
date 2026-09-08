"""Run deterministic examples without invoking a provider or the desktop runner."""

import argparse
import importlib
import json

from tutorial_examples import CHAPTER_MODULES


def compare_chapter(chapter: int) -> dict:
    module = importlib.import_module(f"tutorial_examples.{CHAPTER_MODULES[chapter]}")
    manual = module.run_manual()
    framework = module.run_framework()
    return {
        "chapter": chapter,
        "manual": manual,
        "framework": framework,
        "same_output": manual == framework,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="AI Agent 教程：离线示例对照（不评定掌握程度）")
    parser.add_argument("--chapter", type=int, choices=CHAPTER_MODULES)
    args = parser.parse_args()
    chapters = [args.chapter] if args.chapter is not None else list(CHAPTER_MODULES)
    all_matched = True
    for chapter in chapters:
        comparison = compare_chapter(chapter)
        print(json.dumps(comparison, ensure_ascii=False, indent=2))
        matched = comparison["same_output"]
        print(f"第 {chapter} 章：{'两版输出一致' if matched else '两版输出不一致，请检查差异'}")
        all_matched = all_matched and matched
    return 0 if all_matched else 1


if __name__ == "__main__":
    raise SystemExit(main())
