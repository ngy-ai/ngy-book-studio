# 第十章：完成一个可复核的工程，再走向独立研究

你已经分别接触工具契约、检索证据、记忆、计划、评测、审批、多角色和协议。最后一章把这些能力接到同一条研究流程中：取得松果项目资料，形成有依据的答复，经过核验和批准后保存草稿。重点是让每一条结论和每一次执行都有可追查的原因。

**综合项目 Capstone** 指用一项完整任务整合前面学过的能力。这里交付的是离线工程原型：固定策略、虚构资料、内存草稿、可注入失败。不包含真实模型服务、生产存储、部署、MCP 传输或任意代码隔离。它是走向真实工程的一个台阶。

运行 `courses/agent-foundations/tutorial_examples/ch10_capstone.py`：

```powershell
uv run --locked python -m tutorial_examples.ch10_capstone
```

在 `courses/agent-foundations` 执行。程序打印八种场景的手搓与 LangGraph 结果，以及一个消融对照。桌面点击“本章代码实验”，按“讲义”完成综合能力任务；它使用独立的五参 `run` 编辑区和运行时输入，不是把这份无参演示提交给第一章。两者都不访问真实书库。

桌面任务的输入、工具和交卷规则见[本章练习讲义](../exercises/10-capstone-and-research.md)。

## 1. 给综合任务一个明确的完成定义

输入任务是“说明松果项目何时启动、面向谁、允许哪些操作，并保存一份供我查看的草稿”。宿主范围固定为 D1、D2。D1 的事实是 `start_date=2026-10-12`，D2 包含 `audience=内部员工` 和 `allowed_operations=资料检索与阅读`。最终字段继续使用 `{value, source_ids}`。

这里的完成有多层含义。流程结束，只说明没有下一步；研究答案合规，说明结构和来源检查通过；草稿已保存，说明执行边界已经返回回执。用户拒绝批准时，研究内容仍可存在，但草稿写入数必须为 0。读取预算耗尽时，即使部分事实已经知道，也不能把中途状态装扮成完整交付。

本章固定策略只生成受控结构，短演算的 `review` 专注来源，不是通用的输入验证器。接收真实模型输出时，仍须先执行第二章的字段、类型、大小等契约校验，再进入这里的来源与审批流程。

```text
宿主固定范围 → retrieve 取得证据 → draft 形成候选
            → review 核验来源 → approval 审批并保存内存草稿
```

这条流程有意保持简单。它没有强行使用多个 Agent，也没有为了展示 MCP 而增加一个没有价值的本地传输层。第八章已证明小任务可以由单角色完成；第九章的协议适合真正需要连接独立组件时使用。综合工程首先组合必要能力，再用证据决定是否增加架构。

## 2. 手搓一条完整的研究到草稿流程

下面是可以独立执行的小程序。为把拼装关系看清楚，它省略重试与计时；这些故障处理在完整配套文件中实现。`approved` 是明确给定的虚构人工回复，不是模型替人批准。

```python
import json

docs = {
    "D1": {"start_date": "2026-10-12"},
    "D2": {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
}
fields = ("start_date", "audience", "allowed_operations")

def retrieve(state):
    allowed = ("D1", "D2")  # 由宿主确定，不能从资料正文扩大
    return {"evidence": {sid: dict(docs[sid]) for sid in allowed}}

def draft(state):
    answer = {}
    for field in fields:
        sources = [sid for sid, facts in state["evidence"].items() if field in facts]
        values = {state["evidence"][sid][field] for sid in sources}
        answer[field] = {"value": next(iter(values)) if len(values) == 1 else None,
                         "source_ids": sorted(sources) if len(values) == 1 else []}
    if state["fault"]:
        answer["start_date"]["source_ids"] = ["D404"]
    return {"answer": answer}

def review(state):
    errors = []
    for field, item in state["answer"].items():
        sources, value = item["source_ids"], item["value"]
        if value is None:
            if sources:
                errors.append(field)
        elif not sources or any(state["evidence"].get(sid, {}).get(field) != value
                                for sid in sources):
            errors.append(field)
    return {"errors": errors, "status": "blocked" if errors else "verified"}

def commit(state):
    if state["status"] != "verified":
        return {}
    if not state["approved"]:
        return {"status": "rejected"}
    payload = json.dumps(state["answer"], ensure_ascii=False, sort_keys=True)
    drafts = dict(state["drafts"])
    key = "draft-1"
    for _ in range(2):  # 模拟执行边界重复收到同一个动作
        if key in drafts and drafts[key] != payload:
            raise ValueError("动作标识与内容冲突")
        drafts.setdefault(key, payload)
    return {"drafts": drafts, "status": "completed"}

def initial(fault=False, approved=True):
    return {"fault": fault, "approved": approved, "evidence": {}, "answer": {},
            "errors": [], "drafts": {}, "status": "running"}

def run_case(fault=False, approved=True):
    state = initial(fault, approved)
    for step in (retrieve, draft, review, commit):
        state.update(step(state))
    return state

for fault, approved in ((False, True), (True, True), (False, False)):
    result = run_case(fault, approved)
    print(result["status"], len(result["drafts"]), result["errors"])
```

输出依次是 `completed 1 []`、`blocked 0 ['start_date']`、`rejected 0 []`。这三个结果说明：有依据且批准才保存；日期文字正确但引用伪造时不保存；答案正确但用户拒绝时也不保存。`setdefault` 只在键不存在时插入，前面的内容比较防止同一个键被偷偷用于不同内容。这个状态内去重演算仍不代表跨系统事务保证。

每个函数只负责一种转换，手搓调度器用 `state.update(...)` 合并返回字段。旧字段保持不变，新字段覆盖旧值。你可以在每次 update 前后打印状态，观察证据如何先进入 `evidence`，再成为候选 `answer`，最后由 `review` 决定是否允许进入保存阶段。

## 3. 保留同一规则，换成成熟框架

下面这段接在上一段后执行。它复用相同四个步骤，因此能把业务规则变化和调度框架变化分开检查：

```python
from typing import TypedDict
from langgraph.graph import START, END, StateGraph
from langsmith import tracing_context

class State(TypedDict):
    fault: bool
    approved: bool
    evidence: dict
    answer: dict
    errors: list[str]
    drafts: dict
    status: str

graph = StateGraph(State)
for name, step in (("retrieve", retrieve), ("draft", draft),
                   ("review", review), ("commit", commit)):
    graph.add_node(name, step)
graph.add_edge(START, "retrieve")
graph.add_edge("retrieve", "draft")
graph.add_edge("draft", "review")
graph.add_edge("review", "commit")
graph.add_edge("commit", END)
with tracing_context(enabled=False):
    result = graph.compile().invoke(initial())
assert result == run_case()
```

生产框架节点仍然需要读取预算、校验来源并遵守批准结果。本例在终止状态下让后续节点返回空更新，避免后续动作；更大的图也可以用条件边直接转向 `END`。无论选哪种写法，都要用测试证明“失败后没有效果”，不能只看图画得是否漂亮。接口依据 [LangGraph 官方 Graph API](https://docs.langchain.com/oss/python/langgraph/graph-api)，实际依赖固定为课程 `uv.lock` 中的 1.2.11。

## 4. 阅读完整程序的八种失败轨迹

配套模块为每种场景创建新状态和新 `DraftStore`，沿用第七章的内容摘要与幂等回执。它在宿主固定范围内读取，D2 暂时失败最多额外重试一次；真实读取尝试由程序自己计数，不从结果文本猜测。`virtual_ticks` 是每次读取累加 10 的教学逻辑时钟，不是毫秒或测得延迟。

| 场景 | 最终状态 | 实际资料读取次数 | 草稿写入数 |
| --- | --- | --- | --- |
| normal | completed | 2 | 1 |
| transient | completed | 3 | 1 |
| missing | completed，D2 对应字段未知 | 2 | 1 |
| forged_citation | review_failed | 2 | 0 |
| rejected | rejected | 2 | 0 |
| cancelled | cancelled | 0 | 0 |
| tool_budget | tool_budget | 1 | 0 |
| deadline | deadline | 2 | 0 |

missing 可以完成，是因为本任务允许在草稿中明确保留未知；若业务要求三个字段必须齐全，应修改完成规则和对应样本，而不是临时猜答案。deadline 场景在读取返回后也检查截止值，因此已经取得正文仍可能不能继续保存。取消场景发生在第一项读取前，不证明代码覆盖了运行中每一个取消竞争窗口。

正常轨迹是 `scope:D1,D2 → read:D1:ok → read:D2:ok → draft → verify → approval:approved → memory_draft:saved`。暂时失败在两次 D2 读取之间多出错误记录。先比较最早差异，再解释最后状态；否则很容易把来源故障、用户拒绝和资源限额混成一个“Agent 失败”。

## 5. 用消融实验检查一项设计是否有用

**消融实验 Ablation** 是在其它条件保持一致时移除一项机制，观察结果变化。本例把候选日期的来源换成不存在的 D404，再比较是否保留来源核验：保留时 `review_failed`、草稿数 0；移除时 `completed`、错误草稿数 1。它证明这条校验在此故障样本上有必要，不证明系统因此对所有攻击都安全。

做你自己的实验时，先写假设，例如“增加独立来源核对能减少错引，但会增加工具调用”。准备原始方案、改进方案和移除核对的方案，在相同样本与预算上比较来源错误率、任务完成率和资源消耗。一次只改一个关键因素；若同时换模型、改检索、加角色，就难以解释收益来自哪里。

另外准备未参与调参的新任务：换项目名称、资料编号、字段顺序、冲突资料数量和缺失位置。不能只把 D1 改名 D7，却让代码仍靠固定答案通过。记录失败样本、修改版本和再次验证结果，保留失败证据而不是只挑成功演示。引入真实模型后，再补多次试验、实际 usage、耗时和人工量规，不要把本章固定策略的结果当成模型质量结论。

## 6. 从离线作品走到可交付工程

**部署**是把程序放到目标运行环境并让真实用户使用；**观测**是借助日志、指标和轨迹知道它实际上在做什么；**回滚**是发现问题后回到已知可用的软件或配置版本。三者都需要明确对象，不能只写“上线后观察一下”。

一次可复现交付应连接代码版本、依赖锁、数据版本、提示词与模型标识、权限配置、评测样本和运行报告。配套程序只有 `pipeline_version`、`data_version` 等教学标记，没有构建真实发布系统。换代码后保留旧草稿回执和审批记录，避免软件回滚时误以为旧动作从未发生而再次执行。

逐步接入真实环境时，可以先只读、少量任务和固定权限，再观察失败分布与用户等待时间。上线门槛应包括取消是否阻止后续动作、超限是否真正停止、恢复是否重复写入、引用是否越界，以及故障时能否解释给用户。外部模型调用、MCP 服务、持久存储和隔离执行器分别有自己的契约，任意一个组件通过测试都不能替其他组件背书。

## 7. 学完之后，怎样积累走向专家的证据

“会运行教程”是一种起点证据；“能独立解释、改错、迁移和复现”是更强的证据。保存当前版本后关闭教程和 AI，独立画出状态流，再重写来源核验与提交门禁。隔几天用不同资料编号和故障位置再做一次，比较是否还需要同样提示。完整示范可以帮助理解，但不能写成独立完成记录。

**练习一：** 错误来源场景里的日期文字已经正确，为什么仍应拦截？**答案：** 日期值正确与本轮来源有效是两个条件，D404 未在宿主读取证据中登记。

**练习二：** 缺失资料场景的未知字段使用户无法继续工作，直接改为猜测是否更好？**答案：** 先修改任务完成规则，例如转人工补资料或明确请求新来源；不能在资料不足时制造事实。重新定义规则后重跑评测。

**练习三：** 多角色方案通过率相同，但多花一倍工具调用，能称为改进吗？**答案：** 需要事先定义的收益证据；如果没有更好的质量、延迟或必要的权限分工，应优先保留更简单的方案。

需要提示时先定位最早错误阶段（H1），再画输入与输出字段（H2），最后只查看对应函数的局部实现（H3）。迁移练习必须记录首次尝试和所用提示，而不是覆盖成一份完美成品。

长期能力还需要更复杂的真实任务、可复现的实验、清楚解释取舍的设计文档，以及他人能够检查和质疑的作品。请同行尝试复现、提交失败案例，再说明你如何修正。教程结束并不授予“专家”资格；它给出了可以不断积累证据的工程与研究方法。

本章 API 核对日期：2026-09-07。建议回到[第六章的评测方法](06-evaluation-and-debugging.md)，为你下一项真实任务设计自己的验收样本。
