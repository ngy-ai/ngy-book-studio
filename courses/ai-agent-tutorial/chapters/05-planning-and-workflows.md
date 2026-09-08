# 第五章：先做什么、何时停止——规划与工作流

前几章的研究助手已经会校验工具、检索证据、处理信息冲突，也会在使用历史记忆前
重新核验。现在要把这些能力组织成一次完整任务：先搜索资料，读取搜索发现的每一份
资料，最后根据已经取得的证据作答。如果只把这些要求写在提示词里，助手可能先回答
再补读，也可能在一个读取步骤失败后反复搜索。我们需要让“下一步为什么可以做”
成为程序能够检查的问题。

示例位于 `courses/agent-foundations/tutorial_examples/ch05_planning.py`。
在课程目录执行 `uv run --locked python -m tutorial_examples.ch05_planning`。
本例使用虚构松果项目资料、固定本地搜索与读取，不调用模型或网络。它是独立的教程
示例，不是桌面编辑器的受控 `run` 实现。桌面点击“本章代码实验”，在“讲义”中
查看本章调度任务，再补全本章编辑区的五参入口；演示中的三次预算不替代运行时限额。

桌面任务的输入、工具和交卷规则见[本章练习讲义](../exercises/05-planning-and-workflows.md)。

## 1. 工作流与计划是两层不同的决定

**工作流**规定任务的总体执行规则，例如“搜索 → 生成读取计划 → 按依赖执行 → 停止”。
这些阶段由开发者事先确定，运行时按规则流转。确定工作流适合流程清楚、验收标准明确
的任务，不需要每一步都询问模型下一步怎么办。

**计划**描述某次任务准备执行的具体动作及其依赖。本次搜索找到 D1 和 D2，计划便有
两个读取动作；另一次找到三份资料，计划便应有三个读取动作。计划可以根据运行时数据
生成，也可以由模型提出，但“模型提出”不会让计划自动获得执行权限。

本章先用普通 Python 根据搜索结果生成计划。这样你可以直接观察依赖与预算，避免
把模型行为的不确定性和调度程序的错误混在一起。这已经是一种动态计划：读取数量在
搜索返回后才确定；它还不是自由拆题、反思和重规划的完整模型规划器。

先预测一个顺序问题：如果 D1 只说明日期，D2 才说明对象与操作范围，读取 D1 后马上
执行回答，程序能否得到完整答案？即使某个模型猜对了 D2 的内容，证据也仍然不完整。
所以“回答必须等待全部计划读取结束”应当成为可验证的依赖规则。

## 2. 输入资料与计划的完整形状

这里把前三章已经解释的正文提取压缩为结构化虚构资料，便于集中观察调度。
真实系统可以把读取动作换成第三章的正文读取与证据提取，调度规则不因此消失。

```python
DOCUMENTS = {
    "D1": {"start_date": "2026-10-12"},
    "D2": {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
}
MAX_TOOLS = 3

def search() -> list[str]:
    return sorted(DOCUMENTS)

def build_plan(document_ids: list[str]) -> list[dict]:
    if len(document_ids) != len(set(document_ids)):
        raise ValueError("搜索结果包含重复 ID")
    reads = [
        {"id": f"read:{document_id}", "kind": "read", "document_id": document_id, "after": []}
        for document_id in document_ids
    ]
    return reads + [{"id": "answer", "kind": "answer", "after": [item["id"] for item in reads]}]
```

`search()` 返回 `['D1', 'D2']`。`build_plan()` 按这个返回值建立计划，并没有把
“一定读取 D1、D2”写进控制循环。返回的完整计划如下：

```json
[
  {"id":"read:D1","kind":"read","document_id":"D1","after":[]},
  {"id":"read:D2","kind":"read","document_id":"D2","after":[]},
  {"id":"answer","kind":"answer","after":["read:D1","read:D2"]}
]
```

`id` 标识执行步骤；`kind` 选择允许的动作类型；`document_id` 是读取参数。
`after` 是必须已经完成的步骤 ID 列表。空列表表示没有先行依赖，不表示“这个步骤
已经完成”。两个读取都没有依赖，因此从依赖关系看可以同时开始；回答需要等待二者。

注意资料 ID 与步骤 ID 的区别。`D1` 指资料，`read:D1` 指读取它的动作。如果以后
有一次重新核验读取，应给那次动作不同的步骤身份，并说明为何允许重新读取，不能
只靠向完成列表重复添加同一个 ID 来表示两次不同的执行。

## 3. 执行之前先检查计划

设想模型提出一份计划，其中一个步骤的 `kind` 是 `delete`。把整份计划打印出来让
用户看到，并不会阻止它被执行。执行器必须把计划限制在允许的动作集合内。下面的
检查还会拒绝未知资料、缺失依赖、重复步骤、回答提前发生和循环依赖。

```python
def validate_plan(plan: list[dict], allowed_ids: frozenset[str]) -> None:
    ids = [item["id"] for item in plan]
    if len(ids) != len(set(ids)):
        raise ValueError("步骤 ID 不得重复")
    if sum(item["kind"] == "answer" for item in plan) != 1:
        raise ValueError("必须恰好有一个回答步骤")
    for item in plan:
        if item["kind"] not in {"read", "answer"}:
            raise ValueError("计划包含未授权动作")
        if item["kind"] == "read" and item.get("document_id") not in allowed_ids:
            raise ValueError("计划包含未授权资料")
        if not set(item["after"]) <= set(ids):
            raise ValueError("依赖了不存在的步骤")
    reads = {item["id"] for item in plan if item["kind"] == "read"}
    answer = next(item for item in plan if item["kind"] == "answer")
    if set(answer["after"]) != reads:
        raise ValueError("回答必须等待全部读取步骤")
    completed = set()
    while len(completed) != len(ids):
        ready = {
            item["id"]
            for item in plan
            if item["id"] not in completed and set(item["after"]) <= completed
        }
        if not ready:
            raise ValueError("计划存在循环依赖")
        completed.update(ready)
```

前半段是局部检查：每一项是否合法。最后的 `while` 是整体检查：从空的
`completed` 集合出发，反复找出依赖已经满足的步骤，把它们标为“可以完成”。
这只是检查顺序是否存在，不执行任何工具，因此不会消耗工具预算。

对上面的正常计划，第一轮 `ready` 是两个读取，第二轮是回答，第三轮前循环结束。
如果把 D1 的依赖写成 D2、D2 的依赖写成 D1，第一轮就找不到任何可执行步骤，程序
报告循环依赖。这个过程称为按依赖进行拓扑检查：判断能否找到不违背先后要求的顺序。

这里检查的是我们自己生成的受控字典，并不是一个能直接接收任意模型 JSON 的解析器。
如果接入模型规划器，应先用第二章的严格输入契约限制字段、类型、长度和步骤数量，再
进行本节的语义检查。资料权限也必须来自宿主授权；本例搜索只会返回本地允许的资料，
所以可以用其结果构造允许集合。不要把模型随手提供的一串 ID 当成授权集合。

## 4. 状态让“准备做”与“确实做完”分开

**状态机**是依据当前状态和规则决定下一次转换的程序。本例状态包含计划、已完成
步骤、真实证据、实际调用数和终止原因。计划表中出现 D2，不代表已经读到 D2；
只有读取成功后，D2 才能进入 `evidence`，对应步骤才进入 `done`。

```python
def initial(max_tools: int) -> dict:
    if type(max_tools) is not int or not 1 <= max_tools <= MAX_TOOLS:
        raise ValueError("预算必须为 1..3")
    return {"calls": 0, "max_tools": max_tools, "done": [], "evidence": {}, "audit": []}

def search_step(state: dict) -> dict:
    state = {**state, "calls": state["calls"] + 1, "audit": ["search"]}
    state["document_ids"] = search()
    return state

def plan_step(state: dict) -> dict:
    plan = build_plan(state["document_ids"])
    validate_plan(plan, frozenset(state["document_ids"]))
    return {**state, "plan": plan}

def ready_steps(plan: list[dict], done: list[str]) -> list[dict]:
    return sorted(
        [item for item in plan if item["id"] not in done and set(item["after"]) <= set(done)],
        key=lambda item: item["id"],
    )
```

`initial(3)` 把工具预算设为三次。本例一次搜索算一次，读取每份资料各算一次；生成
计划、检查依赖和整理已经得到的字段不算工具调用。这个计数口径专属于本例，不能用
来替代第一章模型决策与实际工具执行分别计量的规则。

预算先要求至少为一，因此开始阶段的一次搜索有预留额度。`search_step()` 只会在
工作流入口执行一次，之后进入读取调度。它不是可以任意重复调用而仍自动检查预算的
通用工具客户端；接入更自由的模型循环时，实际动作必须统一通过有预算守卫的宿主。

`ready_steps()` 同时看两个条件：步骤尚未完成，而且全部依赖已完成。结果按 ID 排序，
让相同输入具有稳定顺序。它返回两个读取时，仅说明二者均已就绪。接下来我们有意每次
只取一个，实现顺序执行，先把“执行一次、记一次”写正确。

## 5. 一次状态转换到底发生了什么

```python
def next_step(state: dict) -> dict:
    ready = ready_steps(state["plan"], state["done"])
    if not ready:
        raise ValueError("尚未完成，但没有可执行步骤")
    task = ready[0]  # 独立读取具备并行条件；本实现明确按稳定顺序执行。
    if task["kind"] == "read":
        if state["calls"] >= state["max_tools"]:
            return {**state, "status": "budget_exhausted"}
        source = task["document_id"]
        return {
            **state,
            "calls": state["calls"] + 1,
            "done": [*state["done"], task["id"]],
            "evidence": {**state["evidence"], source: dict(DOCUMENTS[source])},
            "audit": [*state["audit"], task["id"]],
        }
    answer = {
        field: {"value": value, "source_ids": [source]}
        for source, facts in sorted(state["evidence"].items())
        for field, value in facts.items()
    }
    return {
        **state,
        "answer": answer,
        "status": "completed",
        "done": [*state["done"], "answer"],
        "audit": [*state["audit"], "answer"],
    }

def outcome(state: dict) -> dict:
    return {
        "status": state["status"],
        "tool_calls": state["calls"],
        "plan": state["plan"],
        "completed_steps": state["done"],
        "evidence": state["evidence"],
        "answer": state.get("answer"),
        "audit": state["audit"],
    }

def run_manual(max_tools: int = MAX_TOOLS) -> dict:
    state = plan_step(search_step(initial(max_tools)))
    while "status" not in state:
        state = next_step(state)
    return outcome(state)
```

读 `next_step()` 时，可以按四个动作理解：找到可执行步骤，检查当前动作的预算，
取得真实结果，返回新状态。`{**state, ...}` 保留旧状态中的其余字段；`evidence`
同样先展开旧证据再加入新资料，所以读取 D2 不会抹掉先前的 D1。

如果预算已经耗尽，函数返回带 `budget_exhausted` 的状态，没有读取资料，也没有
把尚未执行的步骤写入完成列表。`while` 下一次检查发现 `status` 已存在，就结束。
终止原因是业务状态的一部分，不能让循环无声退出后仍显示“已完成”。

当就绪步骤是回答时，两个读取已完成，回答从 `evidence` 逐项收集值和资料来源。
这段字典推导式只适用于本例字段互不冲突的两份结构化资料。若多份资料给同一字段不同
取值，不能依赖遍历后覆盖来解决；应接回第三章的冲突分组与未知表示。

正常运行的状态变化可以在纸上逐步算出：

| 转换完成后 | 已用工具次数 | 已完成步骤 | 已有证据 | 状态 |
|---|---:|---|---|---|
| 搜索与计划 | 1 | 空 | 空 | 继续 |
| 读取 D1 | 2 | read:D1 | D1 | 继续 |
| 读取 D2 | 3 | read:D1、read:D2 | D1、D2 | 继续 |
| 回答 | 3 | 两个读取及 answer | D1、D2 | completed |

实际结果里的 `audit` 为 `['search', 'read:D1', 'read:D2', 'answer']`，
`tool_calls` 为 `3`，`answer` 为：

```json
{
  "start_date":{"value":"2026-10-12","source_ids":["D1"]},
  "audience":{"value":"内部员工","source_ids":["D2"]},
  "allowed_operations":{"value":"资料检索与阅读","source_ids":["D2"]}
}
```

现在把预算改为 `run_manual(max_tools=2)`。搜索消耗一次，D1 消耗一次，准备读 D2
时停止。结果是 `status='budget_exhausted'`、`tool_calls=2`、`answer=None`，
保留 D1 的日期证据，`audit` 只有 `['search', 'read:D1']`。本例把回答定义为全部
计划读取完成后的步骤，因此不生成部分答案。若产品希望显示已有结果，可以单独增加
“部分结果整理”终止节点，并明确缺失字段；不能把未读 D2 标成完成以绕过依赖。

## 6. 用 LangGraph 承担状态流转

手搓版的 `while` 根据状态决定是否继续。LangGraph 版本用状态类型、节点和条件边
表达相同规则，业务函数保持一致，方便逐项对照。

```python
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

class WorkflowState(TypedDict, total=False):
    calls: int
    max_tools: int
    done: list[str]
    evidence: dict
    audit: list[str]
    document_ids: list[str]
    plan: list[dict]
    status: str
    answer: dict

def run_framework(max_tools: int = MAX_TOOLS) -> dict:
    graph = StateGraph(WorkflowState)
    graph.add_node("search", search_step)
    graph.add_node("plan", plan_step)
    graph.add_node("step", next_step)
    graph.add_edge(START, "search")
    graph.add_edge("search", "plan")
    graph.add_edge("plan", "step")
    graph.add_conditional_edges(
        "step",
        lambda state: "stop" if "status" in state else "continue",
        {"stop": END, "continue": "step"},
    )
    with tracing_context(enabled=False):
        return outcome(graph.compile().invoke(initial(max_tools), {"recursion_limit": 16}))
```

`START → search → plan → step` 是确定工作流；状态里的 `plan` 是本次动态生成的
动作列表。条件边读取 `status`：有终止状态就到 `END`，否则回到 `step`。节点返回
状态更新，图运行器负责按边调度下一节点。这些接口的官方说明见
[LangGraph Graph API](https://docs.langchain.com/oss/python/langgraph/graph-api)。

`recursion_limit=16` 约束框架调度步数，它不等于三次工具预算。一次计划检查、一次
回答节点执行可能都占框架步骤，却没有新增工具调用。工具预算仍由 `calls` 与
`max_tools` 控制；不能把框架抛出步数异常当作正常的业务停止协议。

`tracing_context(enabled=False)` 在调用期间关闭 LangSmith tracing，让这个离线
例子不因本机全局追踪配置意外发送轨迹。示例没有模型端点或网络工具，手搓版与框架版
比较的是调度行为。运行模块会分别比较正常和预算不足两种结果，二者应完全相同。

## 7. 依赖允许并行，不代表已经实现并行

计划中的两个读取互不依赖，因此未来可以并行。但当前实现的 `ready[0]` 每次只执行
一个步骤；给代码加上“并行计划”注释不会带来并行速度。

真正并行执行前，要解决三个额外问题。第一，在派发前为本批动作统一预留预算，否则
两个工作线程都看到“还剩一次”后可能一起开跑。第二，各读取结果要按资料 ID 合并，
不能让最后返回的状态把另一份证据覆盖。第三，回答必须等全部必需读取结束，并区分
成功、永久失败和取消，不能把“已经派发”当成“已经完成”。

如果在 LangGraph 中拆成两个并行写同一状态字段的节点，还需明确结果合并规则；
本章共享顺序 `step` 节点，尚未演示并行 reducer 或网络异步执行。先弄清依赖，
再增加并发，是为了知道性能提升后哪些正确性约束仍须保留。

## 8. 动手改一次，然后解释结果

先写预测，再在独立示例副本中修改。每题至少记录“预期结果、实际轨迹、导致变化的
状态字段”，不要只抄最终 JSON。

1. 将预算设为 `1`。会有多少份资料进入证据？回答是否执行？
2. 在正常计划中把回答的 `after` 改成只有 `read:D1`，直接调用 `validate_plan()`。
   它应该在哪条规则停止？这和工具执行失败有什么区别？
3. 保持回答依赖两个读取，让 `read:D1` 等待 `read:D2`，同时让 `read:D2` 等待
   `read:D1`。预测拓扑检查的第一轮 `ready`。
4. 在 `DOCUMENTS` 增加一份新的虚构 D3，内容设为 `{"contact":"项目值班组"}`。
   仍用预算 `3`，是否只需改计划生成器才能完成？

自查答案：第一题搜索后额度已经耗尽，没有证据、没有完成的读取，结果仍是明确预算
停止，`audit=['search']`。第二题会被“回答必须等待全部读取步骤”拒绝；这发生在
执行前的计划检查，尚未产生那次读取工具错误，不能消耗重试次数掩盖它。

第三题第一轮没有可就绪步骤；两份资料都在等待对方，检查报告循环依赖。第四题生成器
会自动添加 D3 的读取和回答依赖，原有三次预算却只能覆盖搜索与两次读取，程序在 D3
前停止。增加资料数量会改变成本，动态计划不提供免费的执行额度。若明确授权四次，
需要同步调整预算上限与调用参数，并重新验证，不应在循环里悄悄放宽上限。

再做一个迁移练习：设计一个“先读目录，再选择两份资料”的计划。写出目录读取与
资料读取的依赖关系，以及在哪个阶段才能生成具体资料步骤。答案要点是：目录结果
返回前，具体资料身份尚未知；可以用两阶段工作流生成后续计划。不能先假装读过目录，
也不能为避免拆阶段而让模型编出资料 ID。新计划仍需经过权限、依赖与预算检查。

现在研究助手可以说明每一步为何执行、用了多少实际动作，以及为什么停止。下一章
把“这次看起来跑对了”转化为可重复的评测：设计任务集合、失败样例和指标，并比较
一次修改究竟改善了什么。
