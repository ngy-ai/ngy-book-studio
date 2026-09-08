# 第五步：用 LangGraph 重新组织同一个 Agent

讲义修订：`2026-09-07.tutorial-1`；实验与评分契约：`1.0.0`。

第四步的手搓版已经会回填错误、有限重试和准确停止。现在我们保留这些行为，只把
“下一步去哪儿”交给图来安排。读完本步，你能完整运行框架版，并指出图里的节点、
边和状态分别对应手搓版哪几行，而不是只知道调用一个框架名称。

本章使用依赖锁中的 LangGraph 1.2.11。按课程导读安装即可，无需另装最新版本。
正文给出必要解释和完整代码；官方资料可用于延伸，不是写出本章代码的前置条件。

## 1. 图是执行路线，节点才真正处理数据

把第四步的循环画出来，它只有两种主要动作：

```text
开始 → 模型节点 ── 有工具请求 ──→ 工具节点
           ↑                       │
           └──── 结果已回填 ────────┘
           │
           └── 没有工具请求 → 校验答案 → 结束
```

LangGraph 将这条路线表达为图 `Graph`。节点 `Node` 是执行某项工作的 Python
函数，边 `Edge` 指定执行顺序，状态 `State` 是节点之间传递的数据。固定边始终去
同一个地方；条件边先读状态，再选择下一节点。`START` 和 `END` 是开始、结束标记，
不需要给它们编写函数。

框架不会因为某个节点叫 `tools` 就自动知道怎样读取资料。节点里面仍要调用受控
工具，处理失败并回填消息。先看一个只有两步的小图，就能弄清状态如何流动。

## 2. 一个完整小图：整理项目名，再计算长度

下面程序可以在课程 Python 环境独立运行。它没有调用模型，也没有读资料：

```python
from typing import TypedDict
from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context


class TextState(TypedDict):
    text: str
    length: int


def trim(state: TextState) -> dict:
    return {"text": state["text"].strip()}


def measure(state: TextState) -> dict:
    return {"length": len(state["text"])}


graph = StateGraph(TextState)
graph.add_node("trim", trim)
graph.add_node("measure", measure)
graph.add_edge(START, "trim")
graph.add_edge("trim", "measure")
graph.add_edge("measure", END)

with tracing_context(enabled=False):
    app = graph.compile()
    result = app.invoke({"text": "  松果  ", "length": 0})
print(result)
```

输出 `{'text': '松果', 'length': 2}`。先解释几个首次出现的 Python 写法：

- `class TextState(TypedDict)` 描述一种字典的字段约定，不要求创建特殊字典对象。
  `text: str` 表示这个键存字符串，`length: int` 表示存整数；类型提示帮助阅读和
  检查代码，不自动证明业务数据正确。
- `state: TextState` 描述函数参数类型，`-> dict` 描述返回类型，运行的仍是普通函数。
- `strip()` 返回去掉两端空白后的新字符串；`len()` 计算字符数量，所以“松果”为 2。
- `with tracing_context(enabled=False)` 在这个代码块内关闭 LangSmith 追踪，避免
  使用继承的追踪配置。`with` 会在进入、退出代码块时管理这项临时设置，不改变图逻辑。

再看框架动作：`add_node` 登记名称与函数；`add_edge` 连固定边；`compile()` 把
图定义变成可执行对象；`invoke(...)` 输入初始状态并取得最后状态。

本例没有配置额外的状态合并函数，所以节点返回的字段覆盖旧值，没返回的字段保持
原值。`trim` 只返回 `text`，`length` 仍为 0；`measure` 再把 `length` 更新成 2。
如果交换节点顺序，最终文字仍是“松果”，但长度是 6，因为计数时四个空格还在。
这说明顺序是程序含义的一部分。接着把同样的状态传递放回资料研究。

## 3. Agent 图中，状态需要保存什么

本章使用三项状态：`messages` 是完整消息历史，`pending_tools` 表示本轮是否有
待处理调用，`answer` 是最终答案或空值。完整代码会定义：

```python
from typing import TypedDict


class AgentState(TypedDict):
    messages: list[dict]
    pending_tools: bool
    answer: dict | None
```

`list[dict]` 表示列表中放字典，`bool` 表示真或假；`dict | None` 中的竖线表示
允许“字典或 None”两种类型。这里只有一个答案对象，不是两个对象相加。

正常轨迹中状态这样变化：

| 完成动作 | 消息数 | `pending_tools` | `answer` | 下一步 |
| --- | --- | --- | --- | --- |
| 准备规则与任务 | 2 | False | None | model |
| 模型提出搜索 | 3 | True | None | tools |
| 搜索结果回填 | 4 | False | None | model |
| 模型提出读 D1/D2 | 5 | True | None | tools |
| 两份正文回填 | 7 | False | None | model |
| 模型给最终答案 | 8 | False | 答案字典 | END |

节点返回更新后的完整历史，使用默认覆盖规则。不要同时给它配置列表追加的
reducer；reducer 就是自定义合并旧值与新值的函数。旧历史已有 5 条，节点又返回
完整 7 条，若做列表相加就变成 12 条，前 5 条被重复加入。

图状态已经明确，接下来选择哪些已实现的单步操作可以复用。

## 4. Session 封装单步，不替你决定整条路线

第四步的重复操作可以收进一个对象：初始化消息、问一次模型、处理本轮所有工具、
产生结束结果。课程已经提供这样的 `Session`，中文可以理解为“一次运行的工作状态”。

`session = Session(task, model, tools, limits, emit)` 接收与 `run` 相同的五个参数，
创建规则和任务消息、模型计数及截止时间。创建它不会自行搜索或开始循环。
它有下面这些明确的输入输出：

| 方法或属性 | 输入与输出 | 对应前面学过的逻辑 |
| --- | --- | --- |
| `session.messages` | 可读写的完整消息列表 | 手搓版 `messages` |
| `session.model_step()` | 无额外参数，返回一条 assistant 消息，同时追加到历史 | 检查额度 → `model.complete` → 保留完整响应 |
| `session.tool_step()` | 读取历史末尾 assistant 中的请求，更新历史，返回 None | 每个调用执行 → 解析结果 → 最多额外重试一次 → 回填最终消息 |
| `session.completed(answer)` | 答案字典 → 正常 `AgentOutcome` | 校验期限后保留完整历史与 `final_answer` |
| `session.exhausted(error)` | 预算异常 → 超限 `AgentOutcome` | 保留 `error.reason`、无最终答案、已有历史 |

`model_step` 不执行工具，`tool_step` 不再次请求模型。这就是为什么仍然需要图的
节点和边。`tool_step` 的内部策略等同于第四步的有限重试，并额外检查结果的调用 ID
及 JSON 形状；宿主仍负责真实计数、权限与来源核验。

第三步的 `parse_final` 继续负责将最终 assistant 的 JSON 正文解析为答案字典并
检查三个字段的结构。它不取资料；看到空 `content` 的工具请求时不能调用它。
这几个辅助功能的职责现在都已明确，可以写出整张图。

## 5. 完整框架版：与手搓版共用相同边界

在墨页“代码实验”选择“LangGraph”，将下列完整代码放进编辑区。手搓版保留作为
对照。正文示范可以直接使用，如实记录为看示范后完成；之后再重写关键节点自查。

```python
from typing import TypedDict
from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from moye_lab.contracts import BudgetExceeded
from moye_lab.implementations.common import Session, parse_final


class AgentState(TypedDict):
    messages: list[dict]
    pending_tools: bool
    answer: dict | None


def run(task, model, tools, limits, emit):
    session = Session(task, model, tools, limits, emit)

    def model_node(state: AgentState) -> AgentState:
        emit({"phase": "graph_node", "node": "model"})
        session.messages = list(state["messages"])
        assistant = session.model_step()
        pending = bool(assistant.get("tool_calls"))
        return {
            "messages": list(session.messages),
            "pending_tools": pending,
            "answer": None if pending else parse_final(assistant),
        }

    def tool_node(state: AgentState) -> AgentState:
        emit({"phase": "graph_node", "node": "tools"})
        session.messages = list(state["messages"])
        session.tool_step()
        return {"messages": list(session.messages), "pending_tools": False, "answer": None}

    def next_node(state: AgentState) -> str:
        return "tools" if state["pending_tools"] else "finished"

    graph = StateGraph(AgentState)
    graph.add_node("model", model_node)
    graph.add_node("tools", tool_node)
    graph.add_edge(START, "model")
    graph.add_conditional_edges("model", next_node, {"tools": "tools", "finished": END})
    graph.add_edge("tools", "model")

    try:
        with tracing_context(enabled=False):
            app = graph.compile()
            result = app.invoke(
                {"messages": list(session.messages), "pending_tools": False, "answer": None},
                config={"recursion_limit": 2 * limits.max_model_decisions + 3},
            )
        return session.completed(result["answer"])
    except BudgetExceeded as error:
        return session.exhausted(error)
```

这段程序中的三个函数定义在 `run` 里面，因此可以使用本次 `run` 的 `session`
和 `emit`，不会把不同运行的历史混进全局对象。每个节点先从图状态复制消息，处理
完再返回更新后的完整状态；图负责将它交给下一节点。

`bool(...)` 将非空请求列表转为 `True`；`A if 条件 else B` 是条件表达式，只计算
被选中的一边。所以 `None if pending else parse_final(assistant)` 只在没有请求
时解析答案。`model_step()` 已经追加 assistant，节点不能再追加一次。

`next_node` 只读状态返回标签；`add_conditional_edges` 将 `"tools"` 标签映射到
工具节点，`"finished"` 映射到 `END`，不需要真的创建名叫 finished 的节点。
工具节点后固定回模型，因为“工具执行完”不等于“最终答案已形成”。

`recursion_limit` 限制图调度的步数，本例按模型额度给足空间，让课程预算约束先
准确返回停止原因。它不是模型决策数，也不取代 8/6 的课程预算。`emit` 中的节点
标注帮助看路线，权威调用次数仍来自宿主。

## 6. 比较两版时，比较行为而不是代码长短

先运行正常场景，期望仍是 3 次模型决策、3 次工具执行和三个有来源的字段。图节点
顺序为 `model → tools → model → tools → model`。只有两次工具节点进入，却执行
三次工具，因为第二次工具节点处理 D1、D2 两个请求。

再用两版分别运行第四步的场景，核对以下实际差异：

| 场景 | 两版都应该做到 |
| --- | --- |
| 非法名称与参数 | 拒绝执行，回填错误，允许模型纠正；4 次决策、3 次执行 |
| 暂时失败 | D2 多执行一次，模型仍 3 次、工具 4 次，只回填最终结果 |
| 永久缺失 | 不再读 D2，保留 D1；两个字段未知，正常部分回答 |
| 模型预算 | 8 次决策后停止，保留 `model_budget` |
| 工具预算 | 实际执行不超过 6 次，保留 `tool_budget` |

桌面每次运行一个实现，在右栏“历史”选两份报告对照任务、场景、答案、来源、计数
与停止原因。节点日志与手搓事件不必文字一致，但不能出现框架多读一次资料却被
当成“内部操作不计数”的情况。

如果工具结束后直接停止，查 `tools → model` 固定边；如果历史重复，查是不是
重复追加 assistant 或把全量历史又做追加合并。先找最早差异，再改对应节点或边。

## 7. 自查：框架到底接管了什么

问题一：模型一轮请求两个工具，第二个永久缺失，图应怎样继续？
答案：工具节点处理两个请求，保留第一个成功正文和第二个失败结果，回模型；模型
据此输出有依据的部分答案。失败策略在 `Session.tool_step` 和宿主工具边界，
不是由“用了 LangGraph”自动产生。

问题二：能否删除两个节点，改成一个节点直接调用完整手搓 `run`？技术上可以包一层，
但本章就失去用状态、节点、条件边表达循环的对照目标。你需要能解释模型节点和
工具节点的输入输出，以及条件边为何只在模型后判断。

问题三：什么情况下保留普通 Python，什么情况下用图？两步简单循环用 Python 容易
看清；当分支变多、状态流转和人工确认需要明确表达时，图更便于组织。框架的价值
来自执行结构，工具安全、来源核验和预算仍要由应用定义与执行。

命令行用户可把完整代码保存到 `workspaces/first-agent/langgraph_agent.py`，再运行：

```powershell
uv run --locked python -m moye_lab run --implementation workspaces/first-agent/langgraph_agent.py --scenario normal
```

开头的小图可单独保存为 `workspaces/graph_demo.py`，用
`uv run --locked python workspaces/graph_demo.py` 执行；桌面编辑区仍只接收完整
Agent 的 `run` 文件，不是任意脚本控制台。

第五步完成了从手搓到框架的同任务重建。[第六步](06-transfer-and-review.md)会改变
资料与故障位置，检查你写的是可复用方法，还是只适用于松果项目的一组固定答案。
