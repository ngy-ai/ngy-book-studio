# 第七章：在人与程序之间设置可靠的暂停点

第六章能够检查答案质量。现在用户想让松果助手把研究结论保存成一份草稿。内容正确，并不等于这次保存已经获得授权；程序重试，也不等于可以保存第二份。本章围绕一个很小的动作，讲清审批、中断、恢复和幂等性怎样配合。

本章的“保存”只是在 Python 内存字典里新增虚构草稿，不写文件、不连接真实系统。配套文件为 `courses/agent-foundations/tutorial_examples/ch07_approval.py`。在 `courses/agent-foundations` 运行：

```powershell
uv run --locked python -m tutorial_examples.ch07_approval
```

手搓版用内存中的 JSON 字符串模拟检查点；框架版使用锁定 LangGraph 1.2.11 的 `InMemorySaver`。它们都不能跨进程崩溃恢复。桌面点击“本章代码实验”，按“讲义”补全审批任务的五参 `run`；本地无参演示与桌面提交分开。人的回复均由测试场景明确提供，不会实际弹出业务审批对话框，也不要把学习窗口的“恢复备份”与程序内的审批恢复混为一谈。

桌面任务的输入、工具和交卷规则见[本章练习讲义](../exercises/07-human-approval-and-recovery.md)。

## 1. 审批究竟批准了什么

假设屏幕上只显示“允许助手继续吗”，用户点击允许后，草稿内容又被模型改了。此时继续保存，等于把一个模糊按钮解释成了对未来任意内容的授权。可靠的审批对象应是一个具体动作快照：

```text
operation: save_memory_draft
action_id: songguo-draft-1
revision: 1
text: 松果项目于 2026-10-12 开始。
source_ids: [D1]
```

操作说明、目标、内容、来源和版本共同构成审批对象。**摘要**是对这份对象按稳定规则序列化后计算的指纹；内容或版本改变，摘要也改变。回复要绑定这个摘要，而不是只传一个脱离上下文的 `True`。它可以帮助检测“展示的内容与执行内容不一致”，但摘要本身不是签名，也不能证明是谁批准的。真实产品仍需身份验证和授权记录。

一个动作的状态可以依次变化：`prepared → awaiting_approval → approved → completed`，也可以从等待进入 `rejected`、`expired` 或 `cancelled`。这些都是不同的结果：拒绝说明用户不愿执行；过期说明这次授权窗口已失效；取消说明执行流程应停止。不要把它们统一写成“稍后重试”。

## 2. 先手搓“暂停后恢复”的完整小程序

下面的小例子不使用框架，展示核心顺序。`checkpoint` 是内存字符串，`reply` 是明确写出的虚构用户回复。程序在收到回复前没有写入草稿：

```python
import hashlib
import json

def digest(value):
    text = json.dumps(value, sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()

proposal = {"action_id": "draft-1", "revision": 1,
            "text": "松果项目于 2026-10-12 开始。", "source_ids": ["D1"]}
request = {"proposal": proposal, "digest": digest(proposal), "expires_at": 30}
checkpoint = json.dumps({"status": "awaiting_approval", "request": request},
                        ensure_ascii=False)
drafts = {}
print("等待时草稿数：", len(drafts))

# 恢复：这里的 now 是教学逻辑时钟，不是等待了十秒。
state = json.loads(checkpoint)
shown = state["request"]
reply = {"decision": "approve", "digest": shown["digest"]}
now, cancelled = 10, False

def save_once(value):
    key = value["action_id"]
    if key in drafts:
        if drafts[key]["digest"] != digest(value):
            raise ValueError("同一个动作标识对应了不同内容")
        return drafts[key]["receipt"]
    receipt = "memory:" + key
    drafts[key] = {"digest": digest(value), "receipt": receipt,
                   "proposal": json.loads(json.dumps(value, ensure_ascii=False))}
    return receipt

if cancelled:
    status = "cancelled"
elif now >= shown["expires_at"]:
    status = "expired"
elif digest(proposal) != shown["digest"]:
    status = "proposal_changed"
elif reply["digest"] != shown["digest"]:
    status = "invalid_approval"
elif reply["decision"] != "approve":
    status = "rejected"
else:
    first = save_once(proposal)
    second = save_once(proposal)
    assert first == second
    status = "completed"
print(status, len(drafts))
```

输出为等待时 0 份，完成后 1 份。第二次调用 `save_once` 返回第一次的回执，没有新增草稿。这种“同一个逻辑动作重复请求，效果不重复”的性质叫**幂等性**。这里的幂等键是 `action_id`；摘要负责检查同一个键是否被错误地拿来保存不同内容。两者职责不同。

为何还要考虑“保存成功，回复却丢失”？调用方看到的是超时，不知道保存是否已发生；盲目换一个动作标识重试，会绕过去重。保留同一个动作标识，向执行边界查询或重试，才能区分未执行与已执行。内存字典只能演示这个逻辑；生产系统需要把去重记录与业务效果放在可靠事务或等价机制里，不能宣称本例实现了跨系统“恰好一次”。

## 3. 用六条状态演算理解恢复边界

完整程序为每个场景建立新的草稿存储、审批请求和执行上下文，避免上一场景的数据影响下一场景。观察以下结果：

| 场景 | 暂停后发生什么 | 最终状态 | 新增草稿数 |
| --- | --- | --- | --- |
| approved | 对相同摘要批准，随后模拟重复投递 | completed | 1 |
| rejected | 用户明确拒绝 | rejected | 0 |
| expired | 逻辑时钟到 31，截止值为 30 | expired | 0 |
| cancelled | 恢复前已取消 | cancelled | 0 |
| tampered | 回复携带了另一个摘要 | invalid_approval | 0 |
| changed | 草稿已改为 revision 2 | proposal_changed | 0 |

每个场景的 `writes_before_resume` 都必须为 0。这个指标回答“是否在批准前已经产生效果”，比只检查最终状态字符串更有力。`receipt` 则是执行边界给出的回执；用户说“同意了”不能替代“已经保存成功”的证据。

超时与取消都有时间边界。若取消发生在保存提交之前，应阻止新效果；若发生在提交之后，不能假称草稿不存在，应该报告已提交并按业务能力提供后续动作。这个例子把取消检查和内存写入连续执行，没有外部竞争；真实并发环境还需要定义提交点，处理检查通过之后、提交发生之前的竞争窗口。

## 4. LangGraph 的中断与继续

**检查点**保存流程在哪一步以及当时的状态；**线程标识 `thread_id`** 指定要恢复哪条流程。它是业务执行标识，不等于操作系统线程。`interrupt(payload)` 暂停并把待处理内容交给调用方，`Command(resume=reply)` 则把外部回复交回流程。

下面是一份完整框架小例子。先运行到暂停，再恢复；全程仍只有内存效果：

```python
from typing import TypedDict
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.graph import START, END, StateGraph
from langgraph.types import interrupt, Command
from langsmith import tracing_context

class State(TypedDict):
    proposal: str
    approved: bool
    drafts: list[str]

def approve(state):
    decision = interrupt({"draft": state["proposal"]})
    return {"approved": decision == "approve"}

def save(state):
    return {"drafts": [state["proposal"]] if state["approved"] else []}

graph = StateGraph(State)
graph.add_node("approve", approve)
graph.add_node("save", save)
graph.add_edge(START, "approve")
graph.add_edge("approve", "save")
graph.add_edge("save", END)
app = graph.compile(checkpointer=InMemorySaver())
config = {"configurable": {"thread_id": "songguo-1"}}
with tracing_context(enabled=False):
    paused = app.invoke({"proposal": "待批准的松果草稿",
                         "approved": False, "drafts": []}, config)
    assert paused["drafts"] == []
    print(paused["__interrupt__"][0].value)
    resumed = app.invoke(Command(resume="approve"), config)
    print(resumed["drafts"])
```

这段重点展示框架暂停接口，所以只按固定字符串批准；完整配套文件才把前面的摘要、过期、取消和幂等存储一起接入。两段的范围要分清，不能把这个短例子直接当作生产审批器。

恢复时，包含 `interrupt` 的节点会从头执行。中断之前的代码可能再次运行，因此不要把“发消息、扣款、写草稿”放在那里。把准备工作做成可重复的计算，把实际效果放到批准后的幂等边界。也不要用宽泛的异常捕获吞掉中断机制；只处理你了解语义的业务异常。上述行为核对自 [LangGraph 官方中断文档](https://docs.langchain.com/oss/python/langgraph/interrupts)。

## 5. 检查点不是隔离，也不是永久存档

`InMemorySaver` 在当前 Python 进程里保存状态，进程结束就失去这些记录。用它成功暂停再恢复，只证明了进程内流程可恢复。换成持久检查点后，还需要关心记录损坏、权限、并发恢复、保留期限、版本不匹配以及恢复后的副作用去重。[官方持久化说明](https://docs.langchain.com/oss/python/langgraph/persistence)介绍的是检查点机制，不会替你的外部系统定义事务。

审批也不是沙箱。一个 Python 程序如果拥有读文件、启动子进程和联网权限，等待审批的状态变量本身并不能剥夺这些能力。真正运行不可信代码时，需要操作系统进程、文件和网络权限等独立边界；工具层还应执行范围和参数校验。反过来，即使程序处于隔离环境，也不能自动获得用户对某项业务动作的同意。

本章既没有对抗恶意 Python，也没有测试磁盘崩溃恢复。你学到的是如何使“用户看见的提案、批准的提案、实际执行的提案”保持对应，以及在失败和重试时保留真实状态。

## 6. 练习与自查

先保存首次预测和修改内容，再看答案。H1 提示：问“这次回复还能否对应刚才展示的内容”。H2 提示：把状态、内容摘要和执行回执分别列出来。H3 提示：只修改小程序的 `proposal["revision"]`，让它发生在请求创建之后、最终比较之前。

1. 在用户批准后把草稿改成 revision 2，应沿用旧批准还是重新批准？需要比较什么？
2. `save_once` 已写入，但调用方没收到回执。重试时应该换 `action_id` 吗？
3. 把 `save_once(proposal)` 放到 `interrupt` 前面，再继续一次，会出现哪两类问题？
4. 第二天重启 Python 后，本例的 `InMemorySaver` 能否找回今天的待审批任务？

**自查答案：** 第一题应重新请求批准，旧摘要与新提案不一致；如果只比较动作名字，就检测不到内容变化。第二题应保留同一个逻辑动作标识，执行边界返回已有回执；内容不同却复用同一键要明确冲突。第三题既可能未批准就执行，又可能在节点恢复重跑时重复执行；只修复其中一个不足以可靠。第四题不能，内存检查点没有跨进程持久性。

下一章会让两个角色合作研究。你将看到：交接工作与恢复流程一样，都需要明确的状态、权限和责任边界。

下一章：[多 Agent 系统](08-multi-agent-systems.md)。本章 API 核对日期：2026-09-07；实际运行版本以现有 `uv.lock` 为准。
