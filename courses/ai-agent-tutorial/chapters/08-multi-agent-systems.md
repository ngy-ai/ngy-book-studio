# 第八章：多个角色怎样合作，而不是互相制造噪声

松果助手已经能检索、保留证据、按计划执行，并等待人的批准。现在资料变复杂了：一个角色需要核对启动日期，另一个角色需要梳理受众和允许操作。把工作拆开是否更好？答案取决于分工带来的收益是否超过交接和协调成本。本章先搭一个能检查责任归属的小系统，再和单角色处理同一资料的结果比较。

完整程序是 `courses/agent-foundations/tutorial_examples/ch08_multi_agent.py`，在 `courses/agent-foundations` 运行：

```powershell
uv run --locked python -m tutorial_examples.ch08_multi_agent
```

它只处理内存里的虚构事实。角色采用固定抽取函数，实际模型调用数为 0；这是多角色协调机制的演示，不是在评测多个大模型合作。手搓版串行调度，LangGraph 版组合子图并汇合结果。桌面点击“本章代码实验”，按“讲义”实现受约束的协作任务；该五参 `run` 使用本轮输入和宿主动作接口，不能用本地示例的固定汇总结果代替。

桌面任务的输入、工具和交卷规则见[本章练习讲义](../exercises/08-multi-agent-systems.md)。

## 1. 从一个问题拆出两份有边界的任务

**角色**描述职责；**Agent**通常还包含自己的上下文、决策策略、可用工具和停止条件。给同一个函数改两个名字，并不会自动得到两个有用的 Agent。本章先把角色接口固定下来，再留出以后将固定抽取函数替换为模型决策的空间。

我们的角色安排如下：

| 角色 | 要回答的字段 | 可读来源 | 返回给协调者的内容 |
| --- | --- | --- | --- |
| timeline | start_date | D1、D3 | 日期候选值及各自来源 |
| policy | audience、allowed_operations | D2 | 受众、允许操作及来源 |
| 协调者 | 组织任务和形成总答复 | 使用已登记的结果 | 有依据的答案、冲突、拒绝原因 |

**交接**指把一份任务及必要上下文交给另一个执行单元，等待结构化结果。任务要写清目标、允许来源、输出字段和预算。不要只发“你是世界顶级研究专家，请尽力”，因为接收方不知道范围，也不知道何时停止。

这次只有两个角色，交接上限为 2；每个工作者最多读取两份资料，团队总读取预算为 3。一个角色完成后不能自由创建更多角色。若下一步确实需要补充研究，由协调者检查剩余预算后决定，而不是让“请再检查一下”无限循环。

## 2. 手搓分工：返回结论时同时返回证据

先看一份完整小程序。它处理日期和受众两个字段，保留第三份资料造成的冲突。`read` 是宿主控制的读取边界；`ledger` 保存实际发生过的读取快照。

```python
documents = {
    "D1": {"start_date": "2026-10-12"},
    "D2": {"audience": "内部员工"},
    "D3": {"start_date": "2026-11-01"},
}
permissions = {"timeline": {"D1", "D3"}, "policy": {"D2"}}
fields = {"timeline": {"start_date"}, "policy": {"audience"}}
ledger = {}

def read(role, source_id):
    if source_id not in permissions[role]:
        raise PermissionError("超出角色资料范围")
    facts = dict(documents[source_id])
    ledger[role, source_id] = dict(facts)
    return facts

def worker(role, source_ids):
    claims = []
    for sid in source_ids:
        for field, value in read(role, sid).items():
            if field in fields[role]:
                claims.append({"field": field, "value": value, "source_id": sid})
    return {"claims": claims}

def merge(reports):
    accepted = []
    for assigned_role, report in reports.items():
        for claim in report["claims"]:
            sid, field = claim["source_id"], claim["field"]
            observed = ledger.get((assigned_role, sid), {})
            if (sid in permissions[assigned_role]
                    and field in fields[assigned_role]
                    and (assigned_role, sid) in ledger
                    and field in observed
                    and observed.get(field) == claim["value"]):
                accepted.append(claim)
    answer = {}
    for field in ("start_date", "audience"):
        related = [c for c in accepted if c["field"] == field]
        values = {c["value"] for c in related}
        answer[field] = {
            "value": next(iter(values)) if len(values) == 1 else None,
            "source_ids": sorted({c["source_id"] for c in related})
                          if len(values) == 1 else [],
        }
    return answer

reports = {
    "timeline": worker("timeline", ["D1", "D3"]),
    "policy": worker("policy", ["D2"]),
}
print(merge(reports))
```

日期结果是 `None`，受众是“内部员工”且引用 D2。这里没有让两个角色投票。D1 与 D3 给出不同日期，增加一个赞同 D1 的角色并不会让 D1 变得更可靠。要消解冲突，需要能区分版本、适用范围或生效时间的新证据；没有这些证据，就保留冲突。

`ledger[role, source_id]` 使用一个二元组作为字典键，等价于 `ledger[(role, source_id)]`。因此 timeline 读过 D1，与 policy 读过 D1 是两条不同记录；不会因某个角色曾经读取，就自动算成所有角色都读取过。

完整文件对三个字段工作，还返回 `conflicts` 中的各候选及其来源，避免把“没读到”与“读到了互相冲突的资料”混为一谈。小程序为了聚焦汇总，只展示最终字段；复核时应同时查看完整文件的冲突记录。

## 3. 不能相信工作者自己写的“我已经查过”

假设 timeline 没有读取 D3，却在报告中写 `read_ids=["D3"]`，再给出碰巧正确的日期。若协调者仅检查“引用存在于报告自带的 read_ids”，伪造的证据链就通过了。**工作者自报的日志是待核对内容，不是读取事实。**

完整示例将读取记录保存在宿主 `EvidenceLedger` 中。它在执行 `read(role, sid)` 时校验范围、扣团队预算并保存事实快照。工作者只返回候选声明；协调者再拿宿主记录核对。团队读取数量也从 ledger 的实际执行计数得到，不把报告里的 `tool_reads` 当成权威指标。

身份同样不能从报告里相信。协调者发给 timeline 的任务，即使收到 `role="policy"`，也仍按 timeline 权限审核。程序用宿主建立的“分配角色 → 报告”映射调用 `reconcile`，忽略工作者自报身份，并重新校验允许字段、来源范围、是否真的读取，以及读取内容是否支持声明。

示例会故意提交三个坏结果：篡改已读日期、伪造未读 D3 的记录、把 policy 的结果冒充 timeline 提交。输出分别拒绝 1、1、2 条声明。它们说明汇总器不应因为结果来自“另一个 Agent”就放松校验。不过，整个程序仍在同一个 Python 进程，不能抵挡恶意代码直接修改宿主对象；本章演示的是逻辑权限边界，操作系统隔离属于另一层。

## 4. LangGraph：分叉、子图和汇合

先画执行关系：

```text
                 ┌→ timeline 子图 ─┐
START → 分配任务 ─┤                 ├→ review → END
                 └→ policy 子图 ───┘
```

**子图**是在父流程中使用的一段独立图。它可以拥有自己的状态和节点。本例子图负责读取并抽取，父图负责分配和汇总；两者不是一张图中的同名变量自动共享。调用子图时应明确传入所需字段，再将它的输出写到父状态指定位置。

以下代码可以接在第二节小程序后运行，用成熟框架组织相同工作：

```python
from typing import TypedDict
from langgraph.graph import START, END, StateGraph
from langsmith import tracing_context

class Team(TypedDict):
    timeline: dict
    policy: dict
    answer: dict

graph = StateGraph(Team)
graph.add_node("timeline_task", lambda _: {
    "timeline": worker("timeline", ["D1", "D3"])
})
graph.add_node("policy_task", lambda _: {"policy": worker("policy", ["D2"])})
graph.add_node("review", lambda s: {
    "answer": merge({"timeline": s["timeline"], "policy": s["policy"]})
})
graph.add_edge(START, "timeline_task")
graph.add_edge(START, "policy_task")
graph.add_edge(["timeline_task", "policy_task"], "review")
graph.add_edge("review", END)
ledger.clear()
with tracing_context(enabled=False):
    output = graph.compile().invoke({"timeline": {}, "policy": {}, "answer": {}})
print(output["answer"])
```

列表形式的汇合边表示 review 等待两个前驱完成。两条分支写入不同字段，避免同时覆盖同一个 `reports` 列表；如果需要共同追加，必须设计合并规则，并在最终输出中按稳定键排序。完成先后不应改变事实结论。

正文短例子的 ledger 每个角色写不同键，只用来说明数据流。完整模块的宿主账本还使用锁，保证并发分支检查并扣减同一个总预算时不会超发。真实并发通常更需要关注共享计数和外部资源，而不是只给函数加一个 `async`。

配套模块进一步把每个角色编译成单独子图，并由父节点调用。图、分叉与汇合接口依据 [官方 Graph API](https://docs.langchain.com/oss/python/langgraph/graph-api)；分工工作流可延伸阅读 [官方 Workflows and agents](https://docs.langchain.com/oss/python/langgraph/workflows-agents)。本例不测吞吐量，不能据此断言框架版更快。

## 5. 什么时候值得拆成多个 Agent

在正常场景，两个角色读取 D1、D2 共两次；单角色读取同样资料也能得到完全相同的答案。多角色版新增两次交接，因此本例没有展示质量收益。这是合理的对照结果，而不是需要隐藏的缺点。拆分有价值的条件通常是任务能独立推进、所需上下文不同、工具权限确实需要分开，或者独立核对能提供不同证据。

如果两个角色使用同一个模型、同一段上下文、同一种错误检索结果，它们可能共享同一个盲点。把相同回答重复三遍不能当作三份独立证据。审阅角色也应指出具体来源和失败规则，而不是只输出“我同意”。

比较方案时，为单角色和多角色提供相同可用资料、相同总工具预算及明确的总模型费用上限。分别报告任务通过率、冲突处理、实际调用、交接量和端到端延迟。否则多角色只是花了更多资源，不能证明架构更好。角色越多，也越容易出现责任不清、上下文被压缩丢失、循环转交和版本不同步。

## 6. 练习与自查

先保存第一次的解释和修改。H1：寻找谁是证据的记录者。H2：分别追踪“任务分配身份”“报告自称身份”“宿主读取记录”。H3：让一个没读 D3 的工作者只修改 `read_ids`，查看它是否能改变 ledger。

1. timeline 和 policy 同时向一个默认覆盖字段 `reports` 返回列表，为什么设计不完整？给出两种修法。
2. 两份日期互相冲突，三个角色赞同一份、一个角色赞同另一份，能据票数选日期吗？
3. 工作报告写了正确事实和真实 D3 标识，但本轮没有读取 D3，协调者应当接受吗？
4. 移除一个角色以后结果完全相同、成本更低，你该如何解释实验结果？

**自查答案：** 第一题缺少并发合并语义，可用不同角色字段后统一汇总，或使用显式追加合并并稳定排序。第二题不能，角色意见数量不是独立来源可靠度。第三题不能，应依宿主本轮读取记录核验，必要时在剩余预算内安排实际读取。第四题说明这批任务尚未证明该角色的价值；保留更简单方案，再用能暴露职责差异的新任务评估，而不是为了保留架构改变评分规则。

下一章将把一个工具从“本进程里的函数”变成“按协议交换消息的能力”，继续保留这里的宿主授权和证据核验原则。

下一章：[MCP 与外部集成](09-mcp-and-integration.md)。API 核对日期：2026-09-07；程序使用课程锁定的 LangGraph 1.2.11。
