# 第三步：把完整循环写成自己的 run 函数

讲义修订：`2026-09-07.tutorial-1`；实验与评分契约：`1.0.0`。

第二步已经运行过“搜索 → 读两份正文 → 回答”的完整演示。现在不改变消息格式，
只把环境准备交给运行器，把控制逻辑收进你自己的函数。读完本步，你可以直接把
正文中的完整程序放入墨页“代码实验”，运行正常场景，不需要另外查参考实现。

## 1. 分开环境准备和 Agent 的控制逻辑

第二步的演示同时做了两件事：前半段创建模型、工具和预算，后半段组织循环。桌面
运行器负责前半段，并在调用你的代码时传入已创建好的对象：

```text
运行器准备任务、模型、工具、预算
                    ↓
run(task, model, tools, limits, emit)
                    ↓
你的程序保存消息、请求模型、执行工具、回填、结束
                    ↓
运行器检查实际调用与答案，展示结果
```

五个参数都是已有对象，括号中的名称用来在函数内引用它们：

| 参数 | 输入是什么 | 你怎样使用 |
| --- | --- | --- |
| `task` | 本次原始问题，字符串 | 放入用户消息，不改为固定项目名 |
| `model` | 受控模型对象 | `model.complete(messages, tools.schemas)` |
| `tools` | 受控工具对象 | 读取 `schemas`，调用 `execute(call)` |
| `limits` | 调用和期限限制 | 初始化规则、检查与记录停止原因 |
| `emit` | 接收一个字典的函数 | `emit({"phase":"model"})` 标注当前动作 |

`emit` 是函数参数，像第一步的 `read_demo` 一样可以调用；它只是用于讲解的事件，
不是检查通过的依据。真正发生了几次调用，由运行器从模型和工具边界记录。
知道输入从哪来以后，还要明确函数结束时交回什么。

## 2. 返回值要表达“如何结束”，不只是答案文字

`AgentOutcome` 是课程定义的结果对象。下面可在课程 Python 环境独立运行：

```python
from moye_lab.contracts import AgentOutcome

result = AgentOutcome("incomplete", "starter_todo", None, [])
print(result.status)
print(result.stop_reason)
```

输出 `incomplete` 和 `starter_todo`，表示骨架尚未完成。这四个位置依次是状态、
停止原因、答案和消息历史。`None` 表示没有答案，空列表表示尚无历史。

正常回答的状态是 `completed`、原因是 `final_answer`。预算耗尽则是
`budget_exhausted`，具体原因保留为 `model_budget`、`tool_budget` 或 `deadline`。
后两种不是“回答少一点”的别名；它们表示因为额度或期限无法继续完成回答。
第四步会分别演示。现在先准备运行所需的两条初始消息。

## 3. initial_messages 实际创建什么

为避免每个学习者复制一大段重复规则，课程提供 `initial_messages` 辅助函数。
辅助函数只是将一项重复工作封装起来，不会替你运行完整 Agent。其输入是原始任务
与预算对象，输出是一个包含两条字典的列表。下面例子可单独运行：

```python
from moye_lab.contracts import RunLimits
from moye_lab.implementations.common import initial_messages

messages = initial_messages("请根据资料调查松果项目。", RunLimits())
print(len(messages))
print(messages[0]["role"], messages[1]["role"])
print(messages[1]["content"])
```

输出为 `2`、`system user`、`请根据资料调查松果项目。`。第一条 `system` 消息给模型
说明：工具有哪些、先读正文、如何处理未知、调用预算和最终 JSON 结构；第二条
`user` 消息原样携带输入任务。它的工作可以概括为以下数据构造：

```text
[
  {role: system, content: 工具、来源、预算和回答格式规则},
  {role: user,   content: 本次 task 字符串}
]
```

`initial_messages` 不读取 D1/D2，也不把 fixture 的答案放进任务。上面的例子用于
认识返回结构；实际 `run` 必须传运行器收到的 `task`，不能写死示例里的问题。

初始规则是给模型的说明，不是权限检查。模型即使不遵守文字约定，受控工具入口
仍会检查工具、参数与预算。规则准备好以后，就可以调用模型并保存完整响应。

## 4. 一次模型调用之后，只有两个分支

下面几行放在 `run` 内，含义与第二步相同：

```python
assistant = model.complete(messages, tools.schemas)
messages.append(assistant)
calls = assistant.get("tool_calls") or []
```

第一行获得一个消息字典，第二行保留整个字典，第三行取出待执行请求列表。不能
只保存 `assistant["content"]`，因为调用 ID、工具名称和参数都在 `tool_calls` 中。

接着分支：没有请求，就解析最终答案并返回；有请求，就逐个执行并追加工具返回的
完整消息。一次读取返回后，不马上问模型，而是把本轮所有请求处理完：

```python
for call in calls:
    tool_message = tools.execute(call)
    messages.append(tool_message)
```

`call` 每次取一个请求；即使执行 D1 后消息尾部已变成 `tool`，保存的 `calls` 仍然
包含 D2。不要改为每次从 `messages[-1]` 重新找请求，否则会读错消息身份。

本步先不加重试，所以每个请求执行一次。失败结果也应保存，让模型知道发生了什么。
第四步会在 `execute` 与 `append` 之间插入有限重试。先把另一个分支——最终答案——
讲清楚，再把两边拼成完整函数。

## 5. parse_final 怎样把最终消息变成答案

模型最终返回的 `content` 是 JSON 字符串；程序需要先解析，才能逐个检查答案字段。
`parse_final(assistant)` 完成这件事：接收完整 assistant 消息，返回答案字典。
以下是一个完整的独立小例子：

```python
import json
from moye_lab.implementations.common import parse_final

answer_data = {
    "start_date": {"value": None, "source_ids": []},
    "audience": {"value": None, "source_ids": []},
    "allowed_operations": {"value": None, "source_ids": []},
}
assistant = {"role": "assistant", "content": json.dumps(answer_data)}
answer = parse_final(assistant)
print(answer["start_date"]["value"] is None)
```

输出 `True`。这只是“全部未知”的格式演示，不是正常松果项目的期望答案。
辅助函数的检查顺序是：

1. 确认 `content` 是可解析 JSON 文本，不混入解释或 Markdown 围栏。
2. 确认顶层恰有 `start_date`、`audience`、`allowed_operations` 三个字段。
3. 确认每个字段恰有 `value` 与 `source_ids`；值是字符串或空值，来源是字符串列表。
4. 拒绝重复来源；未知值的来源必须为空。完整实现还拒绝重复 JSON 键等歧义输入。
5. 合格时返回解析后的字典，不合格时抛出 `ProtocolError`，表示消息协议不符合约定。

格式正确仍不能证明事实正确。例如来源写成根本没读过的 D9，可能形状合法，但
会被宿主的证据检查拒绝。因此循环不用猜答案，也不要把所有错误吞掉后返回成功。
现在初始化、工具分支、回答分支都已讲清，可以拼成可运行文件。

## 6. 完整正常版：可以直接放入代码实验

下面是一份完整实现，不含待填空位。先读注释，再在桌面选择“Python 手搓”，用它
替换骨架，选择正常资料研究运行。先跟着示范完成，再关掉示范独立重建，是两次
不同的学习证据；看完整代码不用假装是独立完成。

```python
from moye_lab.contracts import AgentOutcome, BudgetExceeded
from moye_lab.implementations.common import initial_messages, parse_final


def run(task, model, tools, limits, emit):
    messages = initial_messages(task, limits)
    try:
        while True:
            emit({"phase": "model"})
            assistant = model.complete(messages, tools.schemas)
            messages.append(assistant)
            calls = assistant.get("tool_calls") or []

            if not calls:
                answer = parse_final(assistant)
                return AgentOutcome("completed", "final_answer", answer, list(messages))

            for call in calls:
                emit({"phase": "tool"})
                message = tools.execute(call)
                messages.append(message)
    except BudgetExceeded as error:
        return AgentOutcome("budget_exhausted", error.reason, None, list(messages))
```

`return` 结束整个函数，因而没有请求时不需要再写 `break`。有请求时，内层 `for`
处理完就自然回到外层 `while` 顶部，再请求模型。`list(messages)` 创建一份列表副本，
将当时的历史交进结果对象；循环中的字典消息一直按原样保留。

`except BudgetExceeded as error` 只接住额度或期限耗尽异常，`as error` 给异常对象
一个名字，`error.reason` 读取具体停止原因。语法错误、解析错误等其它异常会继续
交给运行器显示，不会被伪装成完成。受控接口检查真正的模型与工具计数。

正常结果是三个正确字段，来源为日期 D1、对象 D2、操作范围 D2，停止原因
`final_answer`，3 次模型决策、3 次工具执行、8 条最终消息。这份代码尚无有限重试，
所以正常成功不表示已经完成第四步的故障处理。

## 7. 从输出倒查控制流，再做一次自己的改动

运行后先看“结果”，再沿“逐步轨迹”找搜索、D1、D2、最终回答。如果没有得到预期，
先定位最早发生差异的地方：

| 现象 | 对应代码原因 | 修正后应看到什么 |
| --- | --- | --- |
| 仍是 `starter_todo` | 当前选择的实现仍为原骨架 | 运行上面的完整 `run` 后出现第一次模型决策 |
| 模型抱怨结果未回填 | 漏掉 assistant 或某条工具消息 | 下一次模型输入包含完整请求及匹配结果 |
| 只读取 D1 | 在工具循环内部提前 `return` | 一轮中 D1、D2 都执行并回填 |
| 消息越来越少 | 在循环内重新建立 `messages` | 每轮保留原任务、旧结果和新增结果 |
| 最终 JSON 不合格 | 输出结构错误或加入额外说明 | 使用真实最终消息解析，错误保留供定位 |

小变式：如果把 `messages.append(message)` 移到 `for` 外，只执行一次，会发生什么？
先写预测再看答案：当模型请求 D1、D2 时，两次工具可能都执行了，但只保留最后的
D2 结果；下一次模型缺少 D1 对应反馈。这个错误不能靠“工具计数还是 3”发现，必须
检查消息历史。修复就是把追加放回每次请求处理之内。

保存代码、预测和运行结果后，可以在保留快照的前提下独立重写一次核心循环。
不查示范时能解释每个分支，比记住二十多行代码更有用；同题重写还不代表未见迁移。

命令行用户将完整实现保存到课程工作区，例如 `workspaces/first-agent/manual.py`，
在课程目录运行：

```powershell
uv run --locked python -m moye_lab run --implementation workspaces/first-agent/manual.py --scenario normal
```

桌面与 CLI 工作区独立，不会自动互相同步。到这里，第二步的演示已经成为你的完整
Agent 入口。[第四步](04-recover-failures.md)只改变每个调用的处理：让非法请求能反馈，
让暂时失败最多重试一次，让永久缺失与预算耗尽都准确结束。
