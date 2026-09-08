# 第二步：看懂资料研究 Agent 怎样工作

讲义修订：`2026-09-07.tutorial-1`；实验与评分契约：`1.0.0`。

上一节学会了保存资料、调用函数和保留结果。这一节让模型参与选择下一步，再把
完整消息逐条展开。读完后，你应该能指着某条消息说清：
谁提出了请求，谁实际执行，结果放在哪里，下一次模型能看到什么。

在墨页中阅读讲义，在“预测与作答”记录推演。下面的代码块是接口消息的展开示例，
不需要整段贴进“代码实验”。编辑区用于第三步的完整 Agent `run` 函数，不能当作
执行任意短函数的交互式 Python 终端。桌面“查看参考”展示源码，不会一键运行示范；
本节先逐条解释消息，最后给出可直接运行的完整离线演示。无需打开外部参考。

## 1. 如果让你查资料，你会怎样做

任务是：根据资料，查明“松果项目”的试运行日期、面向对象和允许操作，为每项
结论提供资料来源。资料是本章编写的虚构文件，不读取你的真实书库。

你可能先搜索项目名，看到两份通知，再打开正文，最后把答案和出处写下来。现在让
程序做这件事：模型根据当前信息选择下一步，程序执行它提出的工具请求，把执行
结果交回模型，然后重复，直到可以回答或必须停止。这就是本章讨论的 **Agent**。

一次普通问答可能只需要“问题 → 模型 → 回答”。这里的资料不在问题文本里，模型
需要经历“问题 → 请求资料 → 得到资料 → 再决定”。Agent 不是一个额外神秘模型，
而是把模型判断、工具执行和反馈循环组织起来的程序。

本章桌面采用**固定响应模型**：预先写好的响应序列充当模型，并检查结果是否正确
送回。它便于看清循环，不会自由理解任意任务。真实模型会自己生成不同请求，仍需
遵守同样接口；现在先掌握正常路线。

## 2. 先分清四个角色和两种资料

| 名称 | 在本章负责什么 | 具体例子 |
| --- | --- | --- |
| 模型 model | 根据收到的消息产生下一条决策 | 提出“搜索松果项目”或输出最终答案 |
| 宿主 host | 承载运行的程序，校验请求、控制执行、记录实际调用与预算 | 不允许调用未登记工具，预算耗尽时停止 |
| 工具 tool | 完成一个明确操作，返回成功或错误结果 | 按资料 ID 读取一份正文 |
| 学习者编写的循环 | 在模型与工具之间传递消息，决定继续或结束 | 请求模型、处理本轮所有调用、回填结果，再进入下一轮 |

“宿主”侧还有受控模型和工具接口；你不用在第一章编写联网客户端。你要编写的是
最后一行的控制逻辑。工具请求（tool call）只是模型给出的结构化数据，
**模型写出 `read_document` 不会自行执行 Python 函数**。

工具只有两个：`search_documents(query)` 接收搜索词，返回最多三份资料的 ID 和
标题；`read_document(document_id)` 接收一个 ID，返回正文或错误。标题相当于
目录索引，正文才是支持结论的证据。ID 是资料的标识，不是电脑上的文件路径。

**消息 message** 是一个字典；**消息历史 messages** 是按时间保存这些字典的列表。
`role` 表示消息身份：`system` 是运行规则，`user` 是任务，`assistant` 是模型输出，
`tool` 是工具结果。**状态 state** 是程序为后续步骤保留的数据，本章主要包括消息
历史、计数和停止原因。普通 Python 函数不会替你自动保留这些状态。

## 3. 第一次模型决策：提出搜索，还没有资料

运行开始，消息列表只有两条：宿主准备的 `system` 规则，以及原始 `user` 任务。
规则要求从正文取得证据；任务提出问题，没有附带正确答案。

提供给你的接口是：

```python
reply = model.complete(messages, tools.schemas)
```

`complete` 请求一次模型决策。`tools.schemas` 是工具说明：名称、用途、必需参数
及其类型；schema 的意思是“数据结构约定”。本章已提供，不必自己编写。
以下是正常场景第一次返回的完整形状，Python 中的 `None` 表示暂时没有回答正文：

```python
reply = {
    "role": "assistant",
    "content": None,
    "tool_calls": [
        {
            "id": "call_1_0",
            "type": "function",
            "function": {
                "name": "search_documents",
                "arguments": '{"query":"松果项目"}',
            },
        }
    ],
}
```

从外向里读：这是一条模型消息；`tool_calls` 是请求列表；列表里的请求有唯一 ID；
`function.name` 指明工具，`function.arguments` 是参数的 **JSON 字符串**。
比如 `json.loads(reply["tool_calls"][0]["function"]["arguments"])["query"]`
才取出搜索词。第一步诊断 C 中的字典，是这种参数已经解析后的简化情况。

此时模型决策用了 1 次，实际工具执行仍是 0 次。循环先保留完整的 `reply`，
再将其中的请求交给 `tools.execute(call)`。受控工具接口检查名称、参数和预算，
允许后才真的搜索。不要直接从模型给出的名字任意调用 Python 函数。

## 4. 第一次工具执行：搜索结果怎样回到模型

下面用 Python 的 `json.dumps` 构造实际接口形状，避免在长 JSON 字符串中堆满
反斜杠。`payload` 是便于阅读的字典，最终的 `content` 仍然是字符串：

```python
import json

payload = {
    "ok": True,
    "data": {
        "documents": [
            {"document_id": "D1", "title": "松果项目：试运行安排"},
            {"document_id": "D2", "title": "松果项目：参与和操作范围"},
        ]
    },
}
tool_message = {
    "role": "tool",
    "tool_call_id": "call_1_0",
    "content": json.dumps(payload, ensure_ascii=False),
}
```

`ok` 表示操作是否成功，`data` 装成功结果。`tool_call_id` 对应前一节请求的 `id`，
就像回执上的订单号，告诉模型“这是哪次请求的结果”。这条消息由工具接口返回，
循环应原样保存，不要自己编造成功结果。

所谓**回填**，就是把工具结果追加进消息历史，让下一次模型调用真正收到它：

```python
messages.append(reply)  # 本轮模型提出了什么
messages.append(tool_message)  # 工具实际返回了什么
```

这里演示两次追加的顺序，实际实现中 `reply` 只应追加一次。从最初两条消息到现在，
历史变为：`system → user → assistant(搜索请求) → tool(搜索结果)`，共 4 条。
此刻模型决策 1 次、工具执行 1 次。

停一下：我们知道 D1、D2 的存在与标题，**三个待回答字段仍都没有正文证据**。
搜索命中“参与和操作范围”，不代表已经知道它面向谁，更不能凭标题猜开放权限。

## 5. 第二次模型决策：一次提出两个读取请求

循环将上述 4 条历史重新交给 `model.complete`。正常固定响应如下：

```python
reply = {
    "role": "assistant",
    "content": None,
    "tool_calls": [
        {
            "id": "call_2_0",
            "type": "function",
            "function": {
                "name": "read_document",
                "arguments": '{"document_id":"D1"}',
            },
        },
        {
            "id": "call_2_1",
            "type": "function",
            "function": {
                "name": "read_document",
                "arguments": '{"document_id":"D2"}',
            },
        },
    ],
}
```

这是 **1 次模型决策、2 个工具请求**。本章参考实现依次处理 D1、D2；先保存整条
`assistant` 消息，调用工具读 D1，得到以下消息。`json` 沿用上一节的导入：

```python
d1_message = {
    "role": "tool",
    "tool_call_id": "call_2_0",
    "content": json.dumps(
        {
            "ok": True,
            "data": {
                "document_id": "D1",
                "title": "松果项目：试运行安排",
                "body": "【虚构教学资料】松果项目的试运行从 2026-10-12 开始。本通知只说明时间。",
            },
        },
        ensure_ascii=False,
    ),
}
```

`body` 才是正文。读完 D1 可以确认日期，但对象与操作仍未知。循环继续执行同一轮
已经提出的 D2 请求，此处不再调用模型。成功结果是：

```python
d2_message = {
    "role": "tool",
    "tool_call_id": "call_2_1",
    "content": json.dumps(
        {
            "ok": True,
            "data": {
                "document_id": "D2",
                "title": "松果项目：参与和操作范围",
                "body": "【虚构教学资料】松果项目试运行面向内部员工，仅允许资料检索与阅读。",
            },
        },
        ensure_ascii=False,
    ),
}
```

两条结果都要回填，并保留此前历史。此时一共 7 条消息：最初 2 条，加搜索请求和
结果 2 条，再加本轮读取请求 1 条、读取结果 2 条。下一次模型才能同时看到 D1 与 D2。
如果每轮把列表清空，或者仅留下 D2，先前取得的日期就没有传给下一次决策。

## 6. 第三次模型决策：用正文作答，然后停止

模型收到 7 条消息，正常场景返回最终答案，没有 `tool_calls`。下面先展开答案对象，
再展示包住它的模型消息。三个字段分别表示日期、对象、操作范围：

```python
answer = {
    "start_date": {"value": "2026-10-12", "source_ids": ["D1"]},
    "audience": {"value": "内部员工", "source_ids": ["D2"]},
    "allowed_operations": {"value": "资料检索与阅读", "source_ids": ["D2"]},
}
reply = {"role": "assistant", "content": json.dumps(answer, ensure_ascii=False)}
```

`value` 是结论，`source_ids` 是支持这项结论的资料 ID 列表。无法确认时，Python
对象使用 `{"value": None, "source_ids": []}`；发送为 JSON 后是 `null` 与空数组。
循环保存最终消息、校验答案结构并结束，宿主检查来源是否对应实际读取与支持事实。
不是“模型说完成了”就能证明答案有依据。

现在完整历史有 8 条消息，但只有 **3 次模型决策和 3 次实际工具执行**：

| 顺序 | 谁在做什么 | 这一步新增的知识或动作 |
| --- | --- | --- |
| 1 | 宿主准备规则和用户任务 | 知道要查什么，还不知道答案 |
| 2 | 模型决定搜索 | 提出一次工具请求 |
| 3 | 工具搜索，循环回填结果 | 知道 D1、D2 的 ID 与标题 |
| 4 | 模型同时请求读取 D1、D2 | 一次决策提出两个请求 |
| 5 | 工具读 D1，循环保留结果 | 日期已有正文证据 |
| 6 | 循环继续交付本轮 D2 请求 | 无需新的模型决策，D1 仍保留 |
| 7 | 工具读 D2，循环回填结果 | 对象、操作范围也有正文证据 |
| 8 | 模型作答，循环与宿主检查结束 | 三个结论对应各自来源 |

本章最多 8 次模型决策、6 次实际工具执行；明确可重试的失败才允许额外尝试一次，
重试也占工具预算。第四步再处理失败；现在先知道预算由宿主执行约束，模型不能
通过“我还需要一次”获得超出预算的权限。

## 7. 合上示范，自己讲一遍

在“预测与作答”写四句话，每句对应一个动作：**模型决策 → 工具执行 → 结果回填
→ 继续或停止**。然后不看上面的计数，回答：为什么不是 3 次工具执行就等于
3 次工具型模型决策？最终作答算不算模型决策？

做两个变化，每题先写判断和理由，再看下面的自查线索：

1. 如果模型先请求读 D1，收到结果后才另作一次决策请求读 D2，最后再作答，总共
   几次模型决策、几次实际工具执行？这里是纸面变式，固定响应序列不会自动这样变。
2. 如果 D2 搜得到却读不到，标题仍叫“参与和操作范围”，哪些字段可以保留？
   哪些应写未知？如果预算已用完，谁来阻止继续执行？

**自查线索：** 第一题把原先一次批量读取决策拆成两次，搜索与最终作答也各占
一次，所以是 4 次模型决策、3 次工具执行。第二题 D1 正文仍支持日期，对象与操作
没有正文应为未知；标题不能补足证据。宿主负责拒绝预算外调用。

若解释卡住：先只看当前动作由谁执行；再检查消息列表保存了什么；最后逐次给
`model.complete` 和真实工具执行编号。能说清一次请求从提出到回填的路径后，再看
下一节的完整程序，检查这些动作是否都能在代码中找到。看懂示范不计为独立实现通过。

## 8. 将上述消息串起来：一段完整离线演示

现在你已经认识消息的各个字段，再看完整程序就不会只有术语。下面程序使用本课程
已准备好的模拟资料和固定响应模型，执行的正是前面三次决策。它只演示正常路线，
不处理暂时故障；第三步把核心循环装成桌面可调用的 `run` 函数，第四步再补重试。

先解释程序中负责准备环境的几个名字：

- `get_scenario("normal")` 返回正常场景对象，其 `task` 属性就是松果项目的问题。
- `RunLimits()` 创建本章预算对象：最多 8 次模型决策、6 次工具执行、1 次额外重试。
- `ToolBroker(...)` 创建受控工具入口，持有模拟资料、计数与真实执行记录。
- `ScriptedModel(...)` 创建固定响应模型；`ObservedModel(...)` 在外层检查每次输入
  是否保留正确历史，并记录模型决策。没有调用真实服务。
- `initial_messages(task, limits)` 创建前面解释的两条规则和任务消息；
  `parse_final(reply)` 解析最终 `content` 的 JSON，检查三个答案字段，返回字典。

这些首字母大写的名称是类，可以理解为对象模板；`类名(参数)` 创建一个对象，
`对象.属性` 读其数据，`对象.方法(...)` 调用它提供的功能。先会使用这个边界即可。
`time.monotonic()` 提供只用于测量经过时间的时钟，让宿主可以检查运行期限。

```python
import json
import time

from moye_lab.contracts import RunLimits
from moye_lab.implementations.common import initial_messages, parse_final
from moye_lab.runtime import ObservedModel, ScriptedModel, ToolBroker
from moye_lab.scenarios import get_scenario

scenario = get_scenario("normal")
limits = RunLimits()
tools = ToolBroker(scenario, limits, time.monotonic())
model = ObservedModel(ScriptedModel(scenario), tools, limits)
messages = initial_messages(scenario.task, limits)

while True:
    reply = model.complete(messages, tools.schemas)
    messages.append(reply)
    calls = reply.get("tool_calls") or []
    if not calls:
        answer = parse_final(reply)
        break
    for call in calls:
        result = tools.execute(call)
        messages.append(result)

print(json.dumps(answer, ensure_ascii=False, indent=2))
print("模型决策：", len(model.records))
print("工具执行：", tools.executions)
print("消息总数：", len(messages))
```

`while True` 持续重复，遇到 `break` 才退出；本例以“没有工具请求”进入回答分支。
`reply.get("tool_calls")` 在键不存在时返回 `None`；`or []` 将空值统一成空列表，
`if not calls` 就表示没有待执行请求。`for` 处理本轮所有请求，再进入下一次 `while`。
`indent=2` 只让打印的 JSON 每层缩进两个空格，便于阅读。

程序输出第六节的三个字段及来源，随后是 `模型决策：3`、`工具执行：3`、`消息总数：8`。
你可以从任一输出倒查对应代码：第三次模型返回最终答案后，没有再调用工具。
预算和历史约束由受控接口检查；本段没有捕获超限异常，所以仅用来演示正常路线。

使用命令行时，在课程目录创建 `workspaces` 文件夹，把代码保存为
`workspaces/observe.py`，执行：

```powershell
uv run --locked python workspaces/observe.py
```

桌面学习者可以直接按上述输出对照阅读，第三步的完整程序能放入“代码实验”运行。
如果你愿意先运行现成参考，可以使用下面的命令。参考成功只是示范通过，
不是独立实现证据：

```powershell
uv run --locked python -m moye_lab run --implementation manual --scenario normal
```

上一节的列表与函数现在组成了真正的请求反馈循环。[第三步](03-build-manual.md)
保留这段循环，把“准备环境”和“Agent 如何行动”分开：环境交给运行器，你提供
完整 `run(...)`，并准确返回正常结束或预算耗尽的结果。
