"""Host-only randomized fixtures, authorization and grading for chapters 2–10.

This module and reference assets are NEVER copied into the LPAC worker. The
worker receives a fixed input through Model, and talks to these virtual tools by
RPC. No tool in this module reads books, opens a network connection, or writes a
real draft. Exact expected decisions are computed by the host, not student logs.
"""

import copy
import hashlib
import json
import random
import time
import uuid
from dataclasses import dataclass, field

from ngy_lab.contracts import BudgetExceeded, ProtocolError
from ngy_lab.implementations.common import (
    _finite_json_float,
    _reject_non_finite,
    _unique_json_object,
)

SCENARIOS = ("normal", "fault", "transfer")
TITLES = {
    2: "验证工具参数与授权范围",
    3: "检索、读取并合成有证据的结论",
    4: "按身份、项目与有效期召回记忆",
    5: "按依赖和预算执行计划",
    6: "分别评估答案值、来源与未知",
    7: "核验审批并只保存一次虚构草稿",
    8: "用角色授权和实际读取核验交接",
    9: "完成进程内协议握手与调用",
    10: "运行取证、复核、审批与保存门禁",
}


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def digest(value):
    return hashlib.sha256(canonical(value).encode("utf-8")).hexdigest()


def _opaque_ids(rng, count):
    # Labels must not encode an input's role, eligibility or expected verdict.
    return [f"id-{number:015x}" for number in rng.sample(range(1 << 60), count)]


def _ok(data):
    return {"ok": True, "data": data}


def _error(code, retryable=False):
    return {"ok": False, "error": {"code": code, "message": code, "retryable": retryable}}


def _classification(request, allowed):
    if request["name"] != "read_document":
        return "unknown_tool"
    args = request["arguments"]
    if not isinstance(args, dict) or set(args) != {"document_id"}:
        return "invalid_arguments"
    if not isinstance(args["document_id"], str) or not args["document_id"]:
        return "invalid_arguments"
    if args["document_id"] not in allowed:
        return "outside_scope"
    return None


def _aggregate(fields, evidence):
    result = {}
    for key in fields:
        entries = [(sid, facts[key]) for sid, facts in evidence.items() if key in facts]
        values = {canonical(value) for _, value in entries}
        status = "unknown" if not entries else "supported" if len(values) == 1 else "conflict"
        result[key] = {
            "status": status,
            "value": entries[0][1] if status == "supported" else None,
            "source_ids": sorted(sid for sid, _ in entries),
        }
    return result


def _plan(input_data):
    steps = {step["id"]: step for step in input_data["steps"]}
    order = []
    if len(steps) != len(input_data["steps"]):
        return {"status": "invalid_plan", "order": []}
    while len(order) < len(steps):
        ready = sorted(
            sid
            for sid, step in steps.items()
            if sid not in order and all(dep in order for dep in step["depends_on"])
        )
        if not ready:
            return {"status": "invalid_plan", "order": []}
        order.append(ready[0])
    budget = input_data["budget"]
    return {
        "status": "completed" if len(order) <= budget else "budget_exhausted",
        "order": order[:budget],
    }


def _score(samples):
    rows = []
    for sample in samples:
        checks = []
        for key in sorted(sample["gold"]):
            gold = sample["gold"][key]
            item = sample["answer"][key]
            value, sources = item["value"], item["source_ids"]
            if value is None:
                evidence_ok = not sources
            else:
                evidence_ok = (
                    bool(sources)
                    and len(set(sources)) == len(sources)
                    and all(
                        sid in sample["read_facts"]
                        and key in sample["read_facts"][sid]
                        and sample["read_facts"][sid][key] == value
                        for sid in sources
                    )
                )
            checks.append(
                {
                    "field": key,
                    "value_ok": value == gold,
                    "evidence_ok": evidence_ok,
                    "unknown_ok": value is None and not sources if gold is None else True,
                }
            )
        rows.append(
            {
                "id": sample["id"],
                "checks": checks,
                "passed": all(
                    all(check[name] for name in ("value_ok", "evidence_ok", "unknown_ok"))
                    for check in checks
                ),
            }
        )
    return {"rows": rows}


def _approval(input_data):
    if input_data["cancelled"]:
        return "cancelled"
    approval = input_data["approval"]
    if approval["decision"] != "approve":
        return "rejected"
    if approval["expires_at"] <= input_data["now"]:
        return "expired"
    if approval["digest"] != digest(input_data["proposal"]):
        return "proposal_changed"
    return "completed"


def _candidate_supported(fields, answer, observed):
    """Check the complete claim against actual read results, including false unknowns."""
    if not isinstance(answer, dict) or set(answer) != set(fields):
        return False
    for key in fields:
        item = answer[key]
        if not isinstance(item, dict) or set(item) != {"value", "source_ids"}:
            return False
        value, sources = item["value"], item["source_ids"]
        if not isinstance(sources, list) or not all(isinstance(sid, str) for sid in sources):
            return False
        if value is None:
            if sources or any(key in facts for facts in observed.values()):
                return False
        elif (
            not sources
            or len(set(sources)) != len(sources)
            or not all(
                sid in observed
                and key in observed[sid]
                and canonical(observed[sid][key]) == canonical(value)
                for sid in sources
            )
        ):
            return False
    return True


@dataclass
class ChapterCase:
    chapter: int
    name: str
    case_id: str
    input: dict
    private: dict = field(default_factory=dict)
    expected: dict = field(default_factory=dict)
    required: list = field(default_factory=list)
    version: str = "1.0.0"

    @property
    def task(self):
        return f"第 {self.chapter} 章固定练习：{TITLES[self.chapter]}。通过 model.complete 取得本次输入；不得硬编码资料或预期答案。"


def make_case(chapter, scenario="normal", *, seed=None):
    if type(chapter) is not int or not 2 <= chapter <= 10:
        raise ValueError("练习章节必须为 2..10 整数")
    if scenario not in SCENARIOS:
        raise ValueError("新章场景必须为 normal、fault 或 transfer")
    rng = random.Random(seed) if seed is not None else random.SystemRandom()
    case_id = uuid.uuid4().hex
    token = f"{rng.randrange(100000, 999999)}"
    ids = _opaque_ids(rng, 4)
    fields = ["date", "audience", "operations"]
    facts = {
        ids[0]: {"date": f"2027-{rng.randrange(1, 13):02d}-{rng.randrange(1, 29):02d}"},
        ids[1]: {
            "audience": rng.choice(["内部研究员", "测试组成员", "受邀员工"]),
            "operations": "检索与阅读",
        },
    }
    case = ChapterCase(chapter, scenario, case_id, {})
    case.private = {"facts": facts, "receipt": f"draft-{uuid.uuid4().hex}"}
    inp = case.input
    if chapter == 2:
        request_ids = _opaque_ids(rng, 6)
        requests = [
            {"id": request_ids[0], "name": "read_document", "arguments": {"document_id": ids[0]}},
            {"id": request_ids[1], "name": "erase_document", "arguments": {"document_id": ids[0]}},
            {"id": request_ids[2], "name": "read_document", "arguments": {"document_id": ids[3]}},
        ]
        if scenario != "normal":
            requests += [
                {
                    "id": request_ids[3],
                    "name": "read_document",
                    "arguments": {"document_id": ids[1], "force": True},
                },
                {"id": request_ids[4], "name": "read_document", "arguments": {}},
            ]
        if scenario == "transfer":
            requests.append(
                {
                    "id": request_ids[5],
                    "name": "read_document",
                    "arguments": {"document_id": ids[1]},
                }
            )
        rng.shuffle(requests)
        inp.update(allowed_ids=ids[:2], requests=requests)
        decisions = []
        for req in requests:
            error = _classification(req, inp["allowed_ids"])
            data = None
            if error is None:
                sid = req["arguments"]["document_id"]
                data = {"document_id": sid, "facts": facts[sid]}
                case.required.append(("read_document", {"document_id": sid}))
            decisions.append(
                {
                    "request_id": req["id"],
                    "accepted": error is None,
                    "error_code": error,
                    "data": data,
                }
            )
        case.expected = {"decisions": decisions}
    elif chapter == 3:
        docs = [{"id": sid, "terms": ["试运行", token]} for sid in ids[:2]]
        if scenario == "fault":
            facts[ids[2]] = {"date": "2099-01-01"}
            docs.append({"id": ids[2], "terms": ["试运行", token]})
        if scenario == "transfer":
            facts.pop(ids[1])
            docs[1]["terms"] = ["无关归档"]
        docs.append({"id": ids[3], "terms": ["试运行", token]})
        rng.shuffle(docs)
        inp.update(
            fields=fields, query_terms=["试运行", token], allowed_ids=ids[:3], documents=docs
        )
        selected = sorted(
            d["id"]
            for d in docs
            if d["id"] in inp["allowed_ids"] and set(d["terms"]) & set(inp["query_terms"])
        )
        case.required = [("read_document", {"document_id": sid}) for sid in selected]
        case.expected = {"fields": _aggregate(fields, {sid: facts[sid] for sid in selected})}
    elif chapter == 4:
        now = rng.randrange(100, 501)
        inp.update(user_id=f"user-{token}", project_id=f"project-{token}", now=now)
        eligible_count = rng.randint(
            *{"normal": (1, 2), "fault": (0, 2), "transfer": (1, 3)}[scenario]
        )
        invalid_kinds = ["user", "project", "expired", "unverified", "superseded"]
        kinds = ["eligible"] * eligible_count
        kinds += rng.sample(invalid_kinds, min(6 - eligible_count, len(invalid_kinds)))
        if len(kinds) < 6:
            kinds.append(rng.choice(invalid_kinds))
        rng.shuffle(kinds)
        records = []
        values = {}
        # IDs identify records; they encode neither eligibility nor the failure reason.
        memory_ids = _opaque_ids(rng, 6)
        for mid, kind in zip(memory_ids, kinds, strict=True):
            record = {
                "id": mid,
                "user_id": inp["user_id"],
                "project_id": inp["project_id"],
                "expires_at": now + rng.randrange(1, 101),
                "verified": True,
                "status": "active",
                "field": rng.choice(["preference", "practice_mode", "answer_format"]),
            }
            if kind == "user":
                record["user_id"] = f"another-user-{rng.randrange(100, 999)}"
            elif kind == "project":
                record["project_id"] = f"another-project-{rng.randrange(100, 999)}"
            elif kind == "expired":
                record["expires_at"] = now - rng.choice([0, 1, 10])
            elif kind == "unverified":
                record["verified"] = False
            elif kind == "superseded":
                record["status"] = "superseded"
            records.append(record)
            values[mid] = f"{record['field']}-{rng.randrange(10, 99)}"
        rng.shuffle(records)
        inp["records"] = records
        selected = sorted(
            (
                r
                for r in records
                if r["user_id"] == inp["user_id"]
                and r["project_id"] == inp["project_id"]
                and r["expires_at"] > inp["now"]
                and r["verified"] is True
                and r["status"] == "active"
            ),
            key=lambda r: r["id"],
        )
        case.private["memories"] = values
        case.private["allowed_memories"] = [r["id"] for r in selected]
        case.required = [("read_memory", {"memory_id": r["id"]}) for r in selected]
        case.expected = {
            "selected": [
                {"id": r["id"], "field": r["field"], "value": values[r["id"]]} for r in selected
            ]
        }
    elif chapter == 5:
        names = _opaque_ids(rng, 4)
        steps = [
            {"id": names[0], "kind": "search", "depends_on": []},
            {"id": names[1], "kind": "read", "depends_on": [names[0]]},
            {"id": names[2], "kind": "read", "depends_on": [names[0]]},
            {"id": names[3], "kind": "answer", "depends_on": names[1:3]},
        ]
        if scenario == "fault":
            steps[0]["depends_on"] = [names[3]]
        rng.shuffle(steps)
        inp.update(steps=steps, budget=2 if scenario == "transfer" else 4)
        case.expected = _plan(inp)
        case.required = [("execute_step", {"step_id": sid}) for sid in case.expected["order"]]
    elif chapter == 6:
        samples = []
        fact_values = {key: value for record in facts.values() for key, value in record.items()}
        variants = [
            "correct",
            "unread",
            "wrong_value",
            "guess_unknown",
            "duplicate",
            "false_unknown",
            "unknown_source",
        ]
        kinds = ["correct" if scenario == "normal" else rng.choice(variants[1:])]
        kinds += [rng.choice(variants) for _ in range(4)]
        rng.shuffle(kinds)
        sample_ids = _opaque_ids(rng, 5)
        for sample_id, kind in zip(sample_ids, kinds, strict=True):
            field_count = rng.choice([2, 3]) if scenario == "transfer" else 2
            selected_fields = rng.sample(fields, field_count)
            known, unknown = selected_fields[:2]
            gold = {key: fact_values[key] for key in selected_fields}
            gold[unknown] = None
            read_facts = {ids[0]: {key: value for key, value in gold.items() if value is not None}}
            answer = {
                key: {"value": value, "source_ids": [ids[0]] if value is not None else []}
                for key, value in gold.items()
            }
            if kind == "unread":
                answer[known]["source_ids"] = [ids[3]]
            elif kind == "wrong_value":
                answer[known]["value"] = f"错误值-{rng.randrange(100, 999)}"
            elif kind == "guess_unknown":
                answer[unknown] = {"value": "猜测结果", "source_ids": []}
            elif kind == "duplicate":
                answer[known]["source_ids"] *= 2
            elif kind == "false_unknown":
                answer[known] = {"value": None, "source_ids": []}
            elif kind == "unknown_source":
                answer[unknown]["source_ids"] = [ids[0]]
            samples.append(
                {
                    "id": sample_id,
                    "gold": gold,
                    "answer": answer,
                    "read_facts": read_facts,
                }
            )
        rng.shuffle(samples)
        inp["samples"] = samples
        case.expected = _score(samples)
    elif chapter == 7:
        proposal = {"draft_id": f"{token}-draft", "revision": 2, "body": f"只读研究草稿 {token}"}
        inp.update(
            proposal=proposal,
            approval={"digest": digest(proposal), "decision": "approve", "expires_at": 150},
            now=100,
            cancelled=False,
            deliveries=["event-a", "event-a", "event-a"],
        )
        # Scenario labels describe the exercise family, never the expected answer.
        variants = {
            "normal": ["completed", "rejected"],
            "fault": ["expired", "proposal_changed"],
            "transfer": ["completed", "rejected", "expired", "proposal_changed", "cancelled"],
        }
        variant = rng.choice(variants[scenario])
        if variant == "expired":
            inp["approval"]["expires_at"] = rng.choice([99, 100])
        elif variant == "proposal_changed":
            inp["proposal"][rng.choice(["revision", "body"])] = f"changed-{token}"
        elif variant == "rejected":
            inp["approval"]["decision"] = "reject"
        elif variant == "cancelled":
            inp["cancelled"] = True
            # Exercise precedence when cancellation and a stale approval coexist.
            inp["approval"]["expires_at"] = 99
        status = _approval(inp)
        case.expected = {
            "status": status,
            "writes": int(status == "completed"),
            "receipt": case.private["receipt"] if status == "completed" else None,
        }
        if status == "completed":
            case.required = [("save_draft", {"proposal": proposal, "digest": digest(proposal)})]
    elif chapter == 8:
        report_ids = _opaque_ids(rng, 3)
        assignments = {
            "schedule": {"source_ids": [ids[0]], "fields": ["date"]},
            "policy": {"source_ids": [ids[1]], "fields": ["audience", "operations"]},
        }
        reports = [
            {
                "id": report_ids[0],
                "assigned_role": "schedule",
                "claimed_role": "schedule",
                "source_id": ids[0],
                "field": "date",
                "value": facts[ids[0]]["date"],
                "read_ids": [],
            },
            {
                "id": report_ids[1],
                "assigned_role": "policy",
                "claimed_role": "policy",
                "source_id": ids[1],
                "field": "audience",
                "value": facts[ids[1]]["audience"],
                "read_ids": [ids[1]],
            },
        ]
        if scenario == "fault":
            reports[rng.randrange(2)]["value"] = f"错误声明-{rng.randrange(100, 999)}"
            reports.append(
                {
                    "id": report_ids[2],
                    "assigned_role": "policy",
                    "claimed_role": "schedule",
                    "source_id": ids[0],
                    "field": "date",
                    "value": facts[ids[0]]["date"],
                    "read_ids": [ids[0]],
                }
            )
        if scenario == "transfer":
            facts[ids[2]] = {"date": "2099-01-01"}
            assignments["schedule"]["source_ids"].append(ids[2])
            reports.append(
                {
                    "id": report_ids[2],
                    "assigned_role": "schedule",
                    "claimed_role": "policy",
                    "source_id": ids[2],
                    "field": "date",
                    "value": "2099-01-01",
                    "read_ids": [],
                }
            )
        rng.shuffle(reports)
        inp.update(fields=fields, assignments=assignments, reports=reports)
        pairs = sorted(
            {
                (r["assigned_role"], r["source_id"])
                for r in reports
                if r["source_id"] in assignments[r["assigned_role"]]["source_ids"]
                and r["field"] in assignments[r["assigned_role"]]["fields"]
            }
        )
        case.required = [
            ("read_document", {"role": role, "document_id": sid}) for role, sid in pairs
        ]
        accepted = {}
        rejected = []
        for report in reports:
            role, sid, key = report["assigned_role"], report["source_id"], report["field"]
            if (
                (role, sid) in pairs
                and key in assignments[role]["fields"]
                and key in facts[sid]
                and facts[sid][key] == report["value"]
            ):
                accepted.setdefault(sid, {})[key] = report["value"]
            else:
                rejected.append(report["id"])
        case.expected = {
            "fields": _aggregate(fields, accepted),
            "rejected_claims": sorted(rejected),
        }
    elif chapter == 9:
        inp.update(protocol_version="2025-11-25", document_id=ids[0], preflight=scenario == "fault")
        case.private["server_version"] = (
            "2099-01-01" if scenario == "transfer" else inp["protocol_version"]
        )
        messages = []
        if inp["preflight"]:
            messages.append({"jsonrpc": "2.0", "id": 0, "method": "tools/list", "params": {}})
        messages.append(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": inp["protocol_version"],
                    "capabilities": {},
                    "clientInfo": {"name": "ngy-practice", "version": "1.0.0"},
                },
            }
        )
        ready = case.private["server_version"] == inp["protocol_version"]
        if ready:
            messages.extend(
                [
                    {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
                    {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
                    {
                        "jsonrpc": "2.0",
                        "id": 3,
                        "method": "tools/call",
                        "params": {"name": "read_document", "arguments": {"document_id": ids[0]}},
                    },
                ]
            )
        case.required = [("exchange", {"message": message}) for message in messages]
        case.expected = {
            "status": "ready" if ready else "unsupported_protocol_version",
            "version": case.private["server_version"],
            "tools": ["read_document"] if ready else [],
            "facts": facts[ids[0]] if ready else None,
            "preflight_error": "not_initialized" if inp["preflight"] else None,
        }
    else:
        inp.update(fields=fields, document_ids=ids[:2], approved=rng.choice([True, False]))
        case.private["read_fault"] = {
            ids[1]: "timeout"
            if scenario == "fault"
            else "not_found"
            if scenario == "transfer"
            else None
        }
        observed = facts if scenario != "transfer" else {ids[0]: facts[ids[0]]}
        answer = {
            key: {
                "value": next((values[key] for values in observed.values() if key in values), None),
                "source_ids": sorted(sid for sid, values in observed.items() if key in values),
            }
            for key in fields
        }
        candidate = copy.deepcopy(answer)
        # Drafts are untrusted in every scenario, independently of I/O failures.
        mutation = rng.choice(["valid", "valid", "source", "value", "duplicate", "false_unknown"])
        if mutation == "source":
            candidate["date"]["source_ids"] = [ids[3]]
        elif mutation == "value":
            candidate["date"]["value"] = "2099-12-31"
        elif mutation == "duplicate":
            candidate["date"]["source_ids"] *= 2
        elif mutation == "false_unknown":
            candidate["date"] = {"value": None, "source_ids": []}
        case.private["candidate"] = candidate
        status = (
            "review_failed"
            if not _candidate_supported(fields, candidate, observed)
            else "rejected"
            if not inp["approved"]
            else "completed"
        )
        case.expected = {
            "status": status,
            "answer": candidate,
            "receipt": case.private["receipt"] if status == "completed" else None,
        }
        for sid in ids[:2]:
            case.required.append(("read_document", {"document_id": sid}))
            if scenario == "fault" and sid == ids[1]:
                case.required.append(("read_document", {"document_id": sid}))
        case.required.append(("draft_candidate", {}))
        if status == "completed":
            case.required.append(("save_draft", {"answer": candidate}))
    return case


class ChapterModel:
    """One fixed input response; not a generative model or student grade oracle."""

    def __init__(self, case, tools, limits):
        self.case, self.tools, self.limits = case, tools, limits
        self.records = []
        self.previous = []
        self.metadata = {
            "mode": "scripted",
            "model": "chapter-fixed-input-v1",
            "chapter": case.chapter,
        }

    def complete(self, messages, schemas):
        self.tools.check_deadline()
        if len(self.records) >= self.limits.max_model_decisions:
            self.tools.exhaustion_events.append("model_decisions")
            raise BudgetExceeded("model_decisions")
        if (
            self.records
            or messages != [{"role": "user", "content": self.case.task}]
            or schemas != self.tools.schemas
        ):
            self.tools.violations.append("invalid_case_request")
            raise ProtocolError("固定练习只接受一次初始输入请求，必须保留任务与工具 schema")
        reply = {
            "role": "assistant",
            "content": canonical(
                {
                    "chapter": self.case.chapter,
                    "case_id": self.case.case_id,
                    "scenario": self.case.name,
                    "input": self.case.input,
                }
            ),
        }
        self.records.append(
            {"messages": copy.deepcopy(messages), "response": copy.deepcopy(reply), "usage": None}
        )
        self.previous = copy.deepcopy(messages + [reply])
        self.tools.history = copy.deepcopy(self.previous)
        return copy.deepcopy(reply)


class ChapterTools:
    """Independent host validator and virtual effects, with the existing budgets."""

    def __init__(self, case, limits, started=None):
        self.case, self.limits = case, limits
        self.started = time.monotonic() if started is None else started
        self.records, self.successful_reads, self.exhaustion_events, self.violations = (
            [],
            {},
            [],
            [],
        )
        self.executions = self.dispatch_attempts = self.writes = 0
        self.history, self.pending, self.results = [], [], {}
        self.attempts, self.last_errors, self.executed_steps = {}, {}, []
        self.phase, self.protocol_ids = "new", set()
        args = {
            2: {"read_document": ["document_id"]},
            3: {"read_document": ["document_id"]},
            4: {"read_memory": ["memory_id"]},
            5: {"execute_step": ["step_id"]},
            6: {},
            7: {"save_draft": ["proposal", "digest"]},
            8: {"read_document": ["role", "document_id"]},
            9: {"exchange": ["message"]},
            10: {"read_document": ["document_id"], "draft_candidate": [], "save_draft": ["answer"]},
        }[case.chapter]
        self.argument_keys = args
        self.schemas = [
            {
                "type": "function",
                "function": {
                    "name": name,
                    "description": f"本章受控虚构工具 {name}",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            key: {
                                "type": "object"
                                if key in {"proposal", "message", "answer"}
                                else "string"
                            }
                            for key in keys
                        },
                        "required": keys,
                        "additionalProperties": False,
                    },
                },
            }
            for name, keys in args.items()
        ]

    def check_deadline(self):
        if time.monotonic() - self.started >= self.limits.timeout_seconds:
            self.exhaustion_events.append("deadline")
            raise BudgetExceeded("deadline")

    def deny(self, reason):
        self.violations.append(reason)
        raise ProtocolError(reason)

    def execute(self, call):
        self.check_deadline()
        self.dispatch_attempts += 1
        if not self.history:
            self.deny("必须先取得本轮输入")
        if (
            not isinstance(call, dict)
            or set(call) != {"id", "type", "function"}
            or call["type"] != "function"
            or call["id"] != f"{self.case.case_id}:{self.dispatch_attempts}"
        ):
            self.deny("工具调用身份或顺序无效")
        function = call["function"]
        if (
            not isinstance(function, dict)
            or set(function) != {"name", "arguments"}
            or not isinstance(function["name"], str)
            or function["name"] not in self.argument_keys
            or not isinstance(function["arguments"], str)
        ):
            self.deny("工具未授权或调用形状无效")
        name = function["name"]
        try:
            arguments = json.loads(
                function["arguments"],
                object_pairs_hook=_unique_json_object,
                parse_constant=_reject_non_finite,
                parse_float=_finite_json_float,
            )
        except (ValueError, RecursionError, ProtocolError):
            self.deny("工具参数必须为 JSON 对象")
        if not isinstance(arguments, dict) or set(arguments) != set(self.argument_keys[name]):
            self.deny("工具参数必须精确符合 schema")
        for key, value in arguments.items():
            if key in {"proposal", "message", "answer"}:
                if not isinstance(value, dict):
                    self.deny("工具对象参数类型无效")
            elif not isinstance(value, str) or not value:
                self.deny("工具字符串参数类型无效")
        signature = canonical([name, arguments])
        previous_attempts = self.attempts.get(signature, 0)
        if previous_attempts and (
            previous_attempts > self.limits.max_retries
            or not self.last_errors.get(signature, {}).get("retryable", False)
        ):
            self.deny("仅明确可重试失败允许额外尝试一次")
        if self.executions >= self.limits.max_tool_executions:
            self.exhaustion_events.append("tool_executions")
            raise BudgetExceeded("tool_executions")
        # Permission and state validation happens before counting an actual execution.
        payload = self._perform(name, arguments, previous_attempts)
        self.executions += 1
        self.attempts[signature] = previous_attempts + 1
        self.last_errors[signature] = payload.get("error", {})
        reply = {"role": "tool", "tool_call_id": call["id"], "content": canonical(payload)}
        self.records.append(
            {
                "call": copy.deepcopy(call),
                "name": name,
                "arguments": copy.deepcopy(arguments),
                "result": copy.deepcopy(payload),
            }
        )
        self.results[call["id"]] = copy.deepcopy(reply)
        self.history.extend(
            [
                {"role": "assistant", "content": None, "tool_calls": [copy.deepcopy(call)]},
                copy.deepcopy(reply),
            ]
        )
        return reply

    def _perform(self, name, args, previous_attempts):
        chapter, inp, private = self.case.chapter, self.case.input, self.case.private
        if name == "read_document":
            sid = args["document_id"]
            allowed = inp.get("allowed_ids", inp.get("document_ids", []))
            if chapter == 8:
                role = args["role"]
                allowed = inp["assignments"].get(role, {}).get("source_ids", [])
            if sid not in allowed:
                self.deny("资料超出宿主授权范围")
            failure = private.get("read_fault", {}).get(sid)
            if failure == "not_found":
                return _error("not_found")
            if failure == "timeout" and previous_attempts == 0:
                return _error("timeout", True)
            if sid not in private["facts"]:
                return _error("not_found")
            facts = copy.deepcopy(private["facts"][sid])
            ledger_key = f"{args['role']}:{sid}" if chapter == 8 else sid
            self.successful_reads[ledger_key] = facts
            return _ok({"document_id": sid, "facts": facts})
        if name == "read_memory":
            mid = args["memory_id"]
            if mid not in private["allowed_memories"]:
                self.deny("记忆未通过身份、项目、有效期及核验过滤")
            value = private["memories"][mid]
            self.successful_reads[mid] = {"value": value}
            return _ok({"memory_id": mid, "value": value})
        if name == "execute_step":
            step = next((step for step in inp["steps"] if step["id"] == args["step_id"]), None)
            if step is None or not all(dep in self.executed_steps for dep in step["depends_on"]):
                self.deny("计划步骤不存在或依赖未完成")
            if len(self.executed_steps) >= inp["budget"]:
                self.deny("计划局部预算耗尽")
            self.executed_steps.append(step["id"])
            return _ok({"step_id": step["id"], "status": "completed"})
        if name == "draft_candidate":
            if not all(
                any(
                    record["name"] == "read_document" and record["arguments"]["document_id"] == sid
                    for record in self.records
                )
                for sid in inp["document_ids"]
            ):
                self.deny("必须先完成所有指定资料的读取尝试")
            return _ok({"answer": copy.deepcopy(private["candidate"]), "approved": inp["approved"]})
        if name == "save_draft":
            if self.writes:
                self.deny("草稿已经保存，禁止重复副作用")
            if chapter == 7:
                if (
                    _approval(inp) != "completed"
                    or canonical(args["proposal"]) != canonical(inp["proposal"])
                    or args["digest"] != inp["approval"]["digest"]
                    or digest(args["proposal"]) != inp["approval"]["digest"]
                ):
                    self.deny("审批未绑定当前提案或已经失效")
            else:
                if (
                    not inp["approved"]
                    or not any(record["name"] == "draft_candidate" for record in self.records)
                    or canonical(args["answer"]) != canonical(private["candidate"])
                    or not self._supported(args["answer"])
                ):
                    self.deny("草稿未通过证据与审批门禁")
            self.writes += 1
            return _ok({"receipt": private["receipt"]})
        if name == "exchange":
            return _ok(self._exchange(args["message"]))
        self.deny("工具未授权")

    def _supported(self, answer):
        return _candidate_supported(self.case.input["fields"], answer, self.successful_reads)

    def _exchange(self, message):
        # Deliberately a small in-process protocol exercise, not an MCP server.
        if (
            set(message)
            not in ({"jsonrpc", "method", "params"}, {"jsonrpc", "id", "method", "params"})
            or message.get("jsonrpc") != "2.0"
            or not isinstance(message.get("params"), dict)
        ):
            self.deny("协议消息格式无效")
        method, params = message["method"], message["params"]
        if "id" in message:
            rid = message["id"]
            if type(rid) is not int or rid in self.protocol_ids:
                self.deny("协议请求 ID 必须为本轮未使用的整数")
            self.protocol_ids.add(rid)
        elif method != "notifications/initialized":
            self.deny("只有 initialized 是此练习的通知")
        if method == "initialize":
            if self.phase != "new" or params != {
                "protocolVersion": self.case.input["protocol_version"],
                "capabilities": {},
                "clientInfo": {"name": "ngy-practice", "version": "1.0.0"},
            }:
                self.deny("初始化消息或会话状态无效")
            self.phase = "initializing"
            return {
                "jsonrpc": "2.0",
                "id": message["id"],
                "result": {"protocolVersion": self.case.private["server_version"]},
            }
        if method == "notifications/initialized":
            if (
                self.phase != "initializing"
                or params
                or "id" in message
                or self.case.private["server_version"] != self.case.input["protocol_version"]
            ):
                self.deny("initialized 通知不符合协商状态")
            self.phase = "ready"
            return None
        if self.phase != "ready":
            return {
                "jsonrpc": "2.0",
                "id": message["id"],
                "error": {"code": -32000, "message": "not_initialized"},
            }
        if method == "tools/list" and not params:
            return {
                "jsonrpc": "2.0",
                "id": message["id"],
                "result": {"tools": [{"name": "read_document"}]},
            }
        if (
            method == "tools/call"
            and set(params) == {"name", "arguments"}
            and params["name"] == "read_document"
            and params["arguments"] == {"document_id": self.case.input["document_id"]}
        ):
            sid = params["arguments"]["document_id"]
            self.successful_reads[sid] = copy.deepcopy(self.case.private["facts"][sid])
            return {
                "jsonrpc": "2.0",
                "id": message["id"],
                "result": {"facts": copy.deepcopy(self.case.private["facts"][sid])},
            }
        self.deny("协议方法、参数或资料授权无效")


def evaluate_chapter(case, outcome, model, tools, limits):
    actual = [(record["name"], record["arguments"]) for record in tools.records]
    # Canonical JSON comparison keeps booleans distinct from integers and rejects
    # unexpected keys. Case inputs never contain this private expected result.
    expected_answer = {"case_id": case.case_id, "result": case.expected}
    try:
        answer_matches = canonical(outcome.answer) == canonical(expected_answer)
    except (TypeError, ValueError, RecursionError):
        answer_matches = False
    return [
        {
            "id": "completed",
            "passed": outcome.status == "completed" and outcome.stop_reason == "final_answer",
            "detail": "程序明确完成；业务拒绝状态由本章结果单独表达",
        },
        {
            "id": "runtime_input",
            "passed": len(model.records) == 1,
            "detail": "必须取得本轮动态练习输入",
        },
        {
            "id": "chapter_result",
            "passed": answer_matches,
            "detail": "候选结果逐项符合宿主独立计算的本章预期",
        },
        {
            "id": "observed_actions",
            "passed": canonical(actual) == canonical(case.required),
            "detail": "实际宿主工具记录符合本章要求的动作、顺序及重试",
        },
        {
            "id": "message_history",
            "passed": canonical(outcome.messages) == canonical(tools.history),
            "detail": "回填消息与宿主实际观察逐项一致",
        },
        {
            "id": "host_policy",
            "passed": not tools.violations,
            "detail": "所有动作通过宿主参数、授权及状态检查",
        },
        {
            "id": "budgets",
            "passed": not tools.exhaustion_events
            and tools.executions <= limits.max_tool_executions
            and len(model.records) <= limits.max_model_decisions,
            "detail": "沿用最多 8 次模型决策、6 次工具执行与一次明确可重试失败的重试",
        },
    ]
