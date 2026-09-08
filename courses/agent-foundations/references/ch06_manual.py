"""第 6 章 manual 参考示范；阅读或运行示范不代表独立完成。"""

from moye_lab.chapter_support import finish, load_case


def prepare(data):
    rows = []
    for sample in data["samples"]:
        checks = []
        for field in sorted(sample["gold"]):
            gold = sample["gold"][field]
            answer = sample["answer"][field]
            value, sources = answer["value"], answer["source_ids"]
            if value is None:
                evidence_ok = not sources
            else:
                evidence_ok = (
                    bool(sources)
                    and len(set(sources)) == len(sources)
                    and all(
                        sid in sample["read_facts"]
                        and field in sample["read_facts"][sid]
                        and sample["read_facts"][sid][field] == value
                        for sid in sources
                    )
                )
            checks.append(
                {
                    "field": field,
                    "value_ok": value == gold,
                    "evidence_ok": evidence_ok,
                    "unknown_ok": value is None and not sources if gold is None else True,
                }
            )
        rows.append({"id": sample["id"], "checks": checks})
    return rows


def perform(data, plan, state, tools):
    # Scoring one field and deciding whether the complete sample passes are separate steps.
    for row in plan:
        row["passed"] = all(
            all(check[name] for name in ("value_ok", "evidence_ok", "unknown_ok"))
            for check in row["checks"]
        )
    return {"rows": plan}


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
