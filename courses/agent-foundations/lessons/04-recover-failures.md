# 第四步：资料没有按预期返回时，怎样继续

讲义修订：`2026-09-07.tutorial-1`；实验与评分契约：`1.0.0`。

第三步已把正常任务连成完整循环。这一步保持同一任务和接口，只在每次工具调用
之间增加一个判断：结果成功就保留；失败则区分能否再试。我们会从具体错误走到
完整可运行的修复版，最后用记录解释为什么它既不会丢资料，也不会无限循环。

## 1. 先读懂失败结果，而不是把所有失败当成异常

读取 D2 暂时超时后，工具仍返回一条 `role="tool"` 消息。它的 `content` 是 JSON
字符串，解析后得到下面的字典结构：

```json
{
  "ok": false,
  "error": {
    "code": "TIMEOUT",
    "message": "资料读取暂时超时",
    "retryable": true
  }
}
```

`ok=false` 表示这次操作没有成功；`code` 是稳定的错误类型；`message` 是给人的
说明；`retryable=true` 表示允许额外尝试，但不保证第二次一定成功。必须明确为
布尔真值才重试，不能看到错误就无限重复。

把失败作为数据返回，循环便能将其交给模型。它不同于第一步中没有接住的 Python
异常，也不同于预算耗尽：后者用 `BudgetExceeded` 中断控制流，在 `run` 外层捕获。
先处理作为数据返回的错误，我们从根本没有执行成功请求的情况开始。

## 2. 非法请求：拒绝执行之后，仍然要交回结果

在 `invalid_call` 场景中，模型第一次请求未登记工具。宿主只允许
`search_documents` 与 `read_document`，因此在执行前返回：

```json
{"ok": false, "error": {"code": "UNKNOWN_TOOL", "message": "工具不存在，只能使用已登记的只读工具", "retryable": false}}
```

第三步的循环已经会追加这个结果，再询问模型。固定响应模型收到错误后，改为
合法搜索，随后正常读取与回答。于是模型多作一次决策，而实际工具执行仍只有三次。
完整路线是 4 次模型决策、3 次实际工具执行。

这里有两个容易混淆的动作：重试，是同一请求因暂时失败再次执行；纠正，是模型看过
错误后提出一个新请求。未知工具不能重试，但模型可以纠正。循环不能用 `continue`
静默跳过它，也不能自行把未知名字改成别的工具；必须交回匹配调用 ID 的错误消息。

`invalid_arguments` 是另一个场景：工具名合法，但请求 `{}` 没有搜索必需的 `query`：

```json
{"ok": false, "error": {"code": "INVALID_ARGUMENTS", "message": "必须且只能提供非空字符串参数 query", "retryable": false}}
```

它同样在执行前拒绝。运行这两个场景时，在轨迹查看 `actual_execution=false`，
表示该次派发没有真正调用资料后端；随后新的合法搜索应为 `true`。
`tools.execute` 被调用不一定意味着工具真的执行了，不能混用两个计数。

自查：非法请求应该被直接当成最终答案吗？答案是不应该。错误描述的是一次请求的
问题，模型仍可在剩余预算内改正；缺的是反馈，而不是强行终止整个研究。下一节则
面对真正进入读取、但中途超时的情况。

## 3. 暂时失败：同一请求最多再试一次

`transient` 场景里，搜索成功，D1 成功，D2 第一次读取超时、第二次成功。因此你
只需要在第三步 `execute` 与 `append` 中间加一个判断。先看可单独定义的函数：

```python
import json


def execute_with_retry(tools, call):
    message = tools.execute(call)
    payload = json.loads(message["content"])
    if payload["ok"] is False and payload["error"].get("retryable") is True:
        message = tools.execute(call)
    return message
```

输入是受控工具对象与模型实际提出的一个请求；返回值是该请求最终的工具消息。
第一次成功时直接返回；第一次失败且明确允许重试时，原样再次执行同一 `call`，
用第二次结果覆盖局部变量 `message`，然后返回。没有 `while`，所以不会第三次尝试。

`and` 从左往右判断，左边为假时不计算右边，因此成功结果没有 `error` 字段也不会
去读取它。`get("retryable") is True` 要求明确授权；字符串 `"true"` 不算。
本章工具边界保证工具消息结构，宿主继续核验调用 ID、实际结果和下一次模型输入。

注意结果追加的位置：

```python
message = execute_with_retry(tools, call)
messages.append(message)
```

每个请求只回填一条最终消息；宿主的实际执行轨迹保留两次尝试。如果第一次超时
就先追加一次，第二次成功又追加一次，会变成一个调用的两条结束回执，不符合本章
协议。先前 D1 的成功消息无需删除，它没有因为另一份资料超时而失效。

本场景正常应为 3 次模型决策、4 次工具执行。第二次尝试发生在处理同一个请求内，
不需要再次询问模型；模型下一次收到的就是最终成功结果。如果重试后仍失败，
同样返回最后那条失败消息，不假装成功。接下来看看明确不允许重试时怎么办。

## 4. 永久缺失：不能从已见标题补出正文

在 `missing` 场景里，搜索还能发现 D2，但正文读取返回：

```json
{"ok": false, "error": {"code": "NOT_FOUND", "message": "资料不存在", "retryable": false}}
```

这次实际查询正文发生了，所以 `actual_execution=true`，但没有得到成功正文。
`execute_with_retry` 看到不可重试后直接返回错误，循环回填，模型给出部分答案：

```json
{
  "start_date": {"value": "2026-10-12", "source_ids": ["D1"]},
  "audience": {"value": null, "source_ids": []},
  "allowed_operations": {"value": null, "source_ids": []}
}
```

虽然教材早已告诉你正常情况下 D2 写了什么，这次运行并没有取得它。不能把自己的
记忆塞进答案，更不能因标题叫“参与和操作范围”就猜权限。只有本次成功读取且
支持该字段的正文，才能作为来源。

两个 `null` 表示本次未确认，不表示没有参与者或禁止所有操作。任务允许保留未知，
所以这种部分回答可以 `completed/final_answer` 正常结束。计数是 3 次模型决策、
3 次实际执行，D2 只读一次。

到这里，一次工具操作可能成功、可重试失败、不可重试失败，三条路都已闭合。
但“允许重试”还不等于“有剩余额度”，需要同时尊重整个运行的预算。

## 5. 预算耗尽：结束的是这次运行

本章最多 8 次模型决策、6 次实际工具执行；重试照样占工具额度。默认运行期限为
180 秒，运行器可能传入更短限制。宿主会在动作边界拒绝越界，不能依赖模型自觉。

| 场景 | 固定响应做什么 | 应出现的计数和停止原因 |
| --- | --- | --- |
| `model_budget` | 不断请求未登记工具 | 8 次模型决策、0 次实际工具执行，`model_budget` |
| `tool_budget` | 持续提出不同的合法搜索 | 7 次模型决策、6 次实际工具执行，`tool_budget` |

第二行里，第七次模型决策已经发生，但其第七个搜索在真正执行之前被拒绝。不能
把请求数量直接当成实际执行数量。额度耗尽时，受控接口抛出 `BudgetExceeded`，
它不是含 `retryable` 的工具失败，所以不能送进重试分支。

第三步已有的异常处理返回 `budget_exhausted`、原始 `error.reason`、`answer=None`
和已产生的消息。未知字段的正常答案是一个完整答案对象；预算耗尽的 `None` 表示
没有完成最终回答。保留这一区分，才能在后续改进时看出究竟卡在什么地方。
现在把三种工具处理与预算停止拼成完整版本。

## 6. 完整修复版：替换手搓编辑区即可运行

这里使用上一节已经解释过的 `initial_messages`、`parse_final`、`AgentOutcome`
和 `BudgetExceeded`。新增逻辑只有 `execute_with_retry`，外层循环保持不变：

```python
import json

from ngy_lab.contracts import AgentOutcome, BudgetExceeded
from ngy_lab.implementations.common import initial_messages, parse_final


def execute_with_retry(tools, call):
    message = tools.execute(call)
    payload = json.loads(message["content"])
    if payload["ok"] is False and payload["error"].get("retryable") is True:
        message = tools.execute(call)
    return message


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
                message = execute_with_retry(tools, call)
                messages.append(message)
    except BudgetExceeded as error:
        return AgentOutcome("budget_exhausted", error.reason, None, list(messages))
```

宿主还按工具名称与参数限制重复执行：成功后重复读取、永久失败后再次读取、第三次
尝试都会被拒绝；换调用 ID 也不能绕过。你的函数表达正确策略，宿主负责实际约束，
两个层次共同工作。学生自行打印“通过”或发出事件，不是权威检查结果。

先保留第三步正常版，再运行 `transient` 观察它缺少什么；使用修复版复验后，应该
看到 D2 多一次执行、最终只回填成功消息。随后依次运行非法名称、非法参数、永久
缺失和两个预算场景，按上文预期核对。不要只看绿色结果，还要解释产生它的路径。

## 7. 用两个变化检查自己理解的是策略

先回答，再看本节答案：

1. 同轮两个读取，第一个成功，第二个暂时失败且重试后仍失败。共执行几次读取？
   交给模型几条工具结果，保留多少份成功正文？
2. 某错误错误地标为 `retryable=true`，这段循环会无限执行吗？如果重试前工具预算
   已达到 6 次，又会发生什么？

第一题：实际读取 3 次，回填 2 条消息，保留第一份成功正文；第二个调用只回填最后
那次失败。第二题：单个 `if` 最多额外执行一次，宿主还检查同一工具与参数的重试
次数；总预算已满时抛出预算异常，不执行下一次尝试。可重试标记既不保证成功，也
不增加额度。

一次有效复盘可以这样写：“我原以为超时不会占执行次数；记录显示 D2 已实际进入
工具。因此我把重试放在同一请求中，且只追加最终消息。修改后模型仍为 3 次、工具
从 3 次变为 4 次，D1 仍在后续输入里。”把真实的首次预测、修改和复验追加保存，
看示范完成如实记为辅助完成，不覆盖此前失败。

第四步把单次成功路径扩展成有界的失败处理。[第五步](05-build-langgraph.md)不会
新增工具或改变预算，而是用 LangGraph 的节点与边重新组织同一过程。你已经知道
每一步应做什么，因此能够判断框架到底接管了哪部分工作。
