"""Developer CLI. Run from the course directory using the locked environment."""

import argparse
import getpass
import json
import re
import shutil
import sys
import uuid
from pathlib import Path

from ngy_lab.contracts import LabError, RunLimits
from ngy_lab.runner import COURSE_ROOT, execute_run, save_report
from ngy_lab.scenarios import SCENARIOS


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description="墨页第一章：资料研究 Agent 开发实验")
    commands = root.add_subparsers(dest="command", required=True)
    for name in ("run", "compare"):
        command = commands.add_parser(
            name, help="运行一次" if name == "run" else "按相同条件对照两版"
        )
        command.add_argument("--scenario", choices=[*SCENARIOS, "all"], default="normal")
        command.add_argument("--mode", choices=["scripted", "live"], default="scripted")
        command.add_argument("--base-url", help="live 模式必填，不含密钥的 API 根端点")
        command.add_argument("--model", help="live 模式必填的模型名")
        command.add_argument(
            "--request-timeout-seconds",
            type=float,
            help="live 单次请求超时，默认30秒，总期限仍由 --timeout-seconds 限制",
        )
        command.add_argument(
            "--max-output-tokens", type=int, help="live 输出预算，默认2048，上限4096"
        )
        command.add_argument(
            "--token-limit-field",
            choices=["max_completion_tokens", "max_tokens"],
            help="端点支持的预算字段；Ollama 显式选择 max_tokens",
        )
        command.add_argument(
            "--json-mode",
            action="store_true",
            help="显式请求端点 JSON object 输出约束，仍需宿主校验内容",
        )
        command.add_argument(
            "--allow-remote",
            action="store_true",
            help="确认向本次指定的非回环端点发送虚构资料和消息",
        )
        command.add_argument(
            "--allow-insecure", action="store_true", help="另行允许指定远程端点使用非加密 HTTP"
        )
        command.add_argument("--max-model-decisions", type=int, default=8)
        command.add_argument("--max-tool-executions", type=int, default=6)
        command.add_argument("--timeout-seconds", type=float, default=180)
        if name == "run":
            command.add_argument(
                "--implementation",
                default="manual",
                help="manual、langgraph 或 workspaces/NAME/实现.py",
            )
        else:
            command.add_argument("--manual", default="manual")
            command.add_argument("--langgraph", default="langgraph")
    commands.add_parser("list", help="列出版本化场景")
    workspace = commands.add_parser(
        "new-workspace", help="复制 TODO 骨架与学习记录，不覆盖已有工作"
    )
    workspace.add_argument("name")
    credentials = commands.add_parser("credentials", help="配置本实验专用 Windows 凭据")
    credentials.add_argument("action", choices=["set", "delete"])
    credentials.add_argument("--base-url", required=True)
    return root


def new_workspace(name: str) -> Path:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,47}", name):
        raise ValueError("工作区名称使用 1..48 个英文字母、数字、下划线或连字符")
    if name.upper().split(".")[0] in {
        "CON",
        "PRN",
        "AUX",
        "NUL",
        *(f"{prefix}{number}" for prefix in ("COM", "LPT") for number in range(1, 10)),
    }:
        raise ValueError("工作区名称不能使用 Windows 保留设备名")
    destination = COURSE_ROOT / "workspaces" / name
    destination.mkdir(parents=True, exist_ok=False)
    for filename in ("manual.py", "langgraph_agent.py"):
        shutil.copyfile(COURSE_ROOT / "starters" / filename, destination / filename)
    shutil.copyfile(
        COURSE_ROOT / "worksheets" / "learning-record.md", destination / "learning-record.md"
    )
    return destination


def configure_credentials(args) -> None:
    from ngy_lab.credentials import delete_api_key, target_for_endpoint, write_api_key
    from ngy_lab.live import normalize_base_url

    endpoint = normalize_base_url(args.base_url)
    target = target_for_endpoint(endpoint)
    if args.action == "delete":
        delete_api_key(target)
        print("已删除本实验端点对应的凭据。")
    else:
        if not sys.stdin.isatty():
            raise ValueError("请在交互终端中配置凭据，禁止通过命令参数、管道或日志传入密钥")
        key = getpass.getpass("API key（隐藏输入，仅保存到 Windows Credential Manager）：")
        if not key.strip():
            raise ValueError("密钥不能为空")
        write_api_key(target, key)
        print("已写入本实验专用凭据。")


def run_commands(args) -> int:
    limits = RunLimits(
        args.max_model_decisions, args.max_tool_executions, timeout_seconds=args.timeout_seconds
    )
    live_config = None
    if args.mode == "live":
        from ngy_lab.live import LiveConfig

        if not args.base_url or not args.model:
            raise ValueError("live 模式必须显式指定 --base-url 和 --model；不会自动切换端点")
        live_config = LiveConfig(
            base_url=args.base_url,
            model=args.model,
            allow_remote=args.allow_remote,
            allow_insecure=args.allow_insecure,
            timeout_seconds=(
                min(30, limits.timeout_seconds)
                if args.request_timeout_seconds is None
                else args.request_timeout_seconds
            ),
            max_output_tokens=2048 if args.max_output_tokens is None else args.max_output_tokens,
            token_limit_field=args.token_limit_field or "max_completion_tokens",
            json_mode=args.json_mode,
        )
    elif (
        args.base_url
        or args.model
        or args.allow_remote
        or args.allow_insecure
        or args.json_mode
        or args.request_timeout_seconds is not None
        or args.max_output_tokens is not None
        or args.token_limit_field
    ):
        raise ValueError("模型端点参数只用于 --mode live")
    names = list(SCENARIOS) if args.scenario == "all" else [args.scenario]
    if args.mode == "live" and args.scenario == "all":
        names = [name for name in names if SCENARIOS[name].expected_stop == "final_answer"]
    implementations = (
        [args.implementation] if args.command == "run" else [args.manual, args.langgraph]
    )
    comparisons = []
    successful = True
    for name in names:
        reports = []
        for implementation in implementations:
            report = execute_run(implementation, name, args.mode, limits, live_config)
            path = save_report(report)
            reports.append((report, path))
            successful = successful and report["passed"]
            metrics = report["metrics"]
            outcome = report["outcome"]
            print(
                f"{implementation} / {name}: {'PASS' if report['passed'] else 'FAIL'} | "
                f"{outcome['status']}:{outcome['stop_reason']} | "
                f"模型 {metrics['model_decisions']} / 工具 {metrics['actual_tool_executions']} / "
                f"框架步骤 {metrics['framework_steps']} | {metrics['elapsed_seconds']:.3f}s"
            )
            print(f"报告：{path}")
            failed = [check["id"] for check in report["checks"] if not check["passed"]]
            if failed:
                print("未通过：" + ", ".join(failed))
            if report["error"]:
                error = report["error"]
                print(f"错误：{error['type']}: {error['message']}")
                if error.get("frames"):
                    frame = error["frames"][-1]
                    print(f"定位：{frame['file']}:{frame['line']}")
            if outcome["status"] == "cancelled":
                return 130
        if args.command == "compare":
            left, right = reports[0][0], reports[1][0]
            comparisons.append(
                {
                    "scenario": name,
                    "reports": [str(path.relative_to(COURSE_ROOT / "runs")) for _, path in reports],
                    "same_answer": left["outcome"]["answer"] == right["outcome"]["answer"],
                    "same_stop_reason": left["outcome"]["stop_reason"]
                    == right["outcome"]["stop_reason"],
                    "metric_difference_langgraph_minus_manual": {
                        key: right["metrics"][key] - left["metrics"][key]
                        for key in (
                            "model_decisions",
                            "actual_tool_executions",
                            "elapsed_seconds",
                            "framework_steps",
                        )
                    },
                    "both_passed": left["passed"] and right["passed"],
                    "note": "两版独立顺序运行；耗时包含初始化，不能作为框架性能结论。"
                    "live 响应与调用量可能不同。内部步骤不是模型调用量。",
                }
            )
    if comparisons:
        path = COURSE_ROOT / "runs" / f"comparison-{uuid.uuid4().hex}.json"
        path.write_text(json.dumps(comparisons, ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"对照：{path}")
    return 0 if successful else 1


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "list":
            for scenario in SCENARIOS.values():
                print(f"{scenario.name}: {scenario.project} / {scenario.expected_stop}")
        elif args.command == "new-workspace":
            print(f"已创建：{new_workspace(args.name)}")
        elif args.command == "credentials":
            configure_credentials(args)
        else:
            return run_commands(args)
    except (ValueError, FileExistsError, LabError) as error:
        print(f"错误：{error}", file=sys.stderr)
        return 2
    except OSError:
        print("文件或环境操作失败，请检查课程目录权限与 Python 环境。", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
