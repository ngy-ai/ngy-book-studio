# 第四章：记住什么、什么时候忘记——上下文与记忆

第三章的助手能够选择相关片段，发现两份通知对日期的说法冲突。但运行结束后，
我们还会遇到另一种问题：昨天已经查过日期，今天用户追问“现在还是那天开始吗？”
如果每次重新读取所有资料，效率不高；如果直接复制昨天的答案，又可能传播过期信息。
本章不是给模型增加一个无限聊天列表，而是给历史信息建立可解释的使用规则。

示例在 `courses/agent-foundations/tutorial_examples/ch04_memory.py`。
在课程目录运行 `uv run --locked python -m tutorial_examples.ch04_memory`。
所有资料、用户身份和日期均为虚构；示例只操作内存，不读写任何真实用户记录。
两版展示相同的召回、重新核验与更正流程，是本地完整演示。
桌面点击“本章代码实验”，按“讲义”给出的运行时输入实现本章五参 `run`；
练习中的记忆也是虚构数据，不能用这里的固定 M1、M6 或日期代替输入处理。

桌面任务的输入、工具和交卷规则见[本章练习讲义](../exercises/04-memory-and-context.md)。

## 1. 工作上下文与跨次记忆各解决什么问题

**工作上下文**是本次推理或工作流正在使用的信息，例如当前问题、候选证据、工具结果、
剩余预算。第一章的 `messages` 列表就是一种工作上下文。它帮助下一轮知道上一轮发生了什么，
但不意味着里面每句话都值得长期保存。

**持久记忆**是跨请求、跨会话甚至跨进程保留的信息。它可能是“用户希望答案简短”这样的
偏好，也可能是“某次执行读到了某份通知”这样的事件记录，或者带来源与有效期的事实。
这三类东西不能混用：用户偏好不能证明项目日期，历史执行成功也不能证明此刻仍然成功。

下面有一份常见的危险“记忆”：`{"start_date":"2026-10-12"}`。
它没有告诉你是哪个项目、属于谁、哪份资料支持、何时读取、是否被更正。它越容易被
拼进提示词，越容易让模型把缺少限定的信息当成当前事实。
因此我们先设计记录结构，再考虑用关键词还是向量去检索它。

## 2. 给记忆加上身份、来源和有效性

这是本例完整的记忆类型。`frozen=True` 表示一条记录创建后不直接修改字段；
更正时生成新记录，并把旧记录标为被替代，方便保留历史关系。

```python
from dataclasses import asdict, dataclass, replace
from datetime import date

@dataclass(frozen=True)
class Memory:
    id: str
    user: str
    project: str
    field: str
    value: str
    source_id: str
    expires: str
    verified: bool = True
    status: str = "active"
    supersedes: str | None = None
```

`id` 是这条记忆的身份，`user` 和 `project` 限定归属，`field` 说明它描述什么事实。
`source_id` 指向证据，`expires` 是本例规定的失效日期，`verified` 表示宿主是否已核验。
`status` 区分正在使用和已被替代的记录，`supersedes` 指向被本条更正的旧记忆。

这些字段有不同职责。把资料放进某人的命名空间，不能自动证明该人仍有权限；
存储时设置 `verified=True` 也不是魔法签名。在本例中，fixture 与写入函数由宿主控制，
模型没有接口自行提交已验证记录。真实产品还需要访问控制、可验证来源和可信写入边界。

为了让结果稳定，我们把“今天”固定为 2026-10-11，而不调用系统当前日期。
本例的五条初始记录完整如下：

```python
INITIAL = (
    Memory("M1", "learner", "pine", "start_date", "2026-10-12", "D1", "2026-10-20"),
    Memory("M2", "learner", "pine", "allowed_operations", "旧范围", "OLD", "2026-10-01"),
    Memory("M3", "other-user", "pine", "start_date", "其他用户信息", "PRIVATE", "2026-12-31"),
    Memory("M4", "learner", "other-project", "start_date", "其他项目日期", "X", "2026-12-31"),
    Memory("M5", "learner", "pine", "audience", "据说所有人", "RUMOR", "2026-12-31", False),
)

AS_OF = date(2026, 10, 11)
```

M1 是本人的松果日期；M2 已过期；M3 属于另一个用户；M4 属于另一个项目；
M5 只是未经核验的传言。先不要运行，预测这五条中哪些可以进入当前工作上下文。
正确结果只能包含 M1。记录再相关，也不能越过归属和有效性限制。

## 3. 手搓召回：先做硬过滤，再谈相关性

**召回**是从存储中取出可能有用的信息。本章存储很小，直接扫描记录即可。
如果日后改成向量检索，归属、有效期和可信状态仍应在相关性排名前约束候选，
不能把别人的记忆排进前几名，再请求模型“自行忽略”。

下面给出完整的内存存储类；先读 `__init__` 和 `recall`，写入方法在下一节逐段解释。

```python
class MemoryStore:
    def __init__(self):
        self.records = list(INITIAL)

    def recall(self, user: str, project: str, as_of: date) -> dict:
        accepted, rejected = [], {"scope": 0, "expired": 0, "unverified": 0, "superseded": 0}
        for item in self.records:
            if (item.user, item.project) != (user, project):
                rejected["scope"] += 1
            elif item.status != "active":
                rejected["superseded"] += 1
            elif date.fromisoformat(item.expires) <= as_of:
                rejected["expired"] += 1
            elif not item.verified:
                rejected["unverified"] += 1
            else:
                accepted.append(asdict(item))
        return {"records": accepted, "rejected_counts": rejected}

    def apply_verified_correction(self, user: str, project: str, evidence: dict) -> str:
        # 受控流程调用 read_current_source；相等检查仅匹配 fixture，不认证外部来源。
        if evidence != CORRECTION or project != evidence["project"]:
            raise ValueError("必须使用本次重新读取、可核验的更正证据")
        previous = next(
            (
                item
                for item in self.records
                if (item.user, item.project, item.field, item.source_id)
                == (user, project, evidence["field"], evidence["supersedes_source"])
            ),
            None,
        )
        if previous is None:
            raise ValueError("没有同一用户、项目、字段中的被更正记录")
        new = Memory(
            "M6",
            user,
            project,
            evidence["field"],
            evidence["value"],
            evidence["source_id"],
            "2026-10-20",
            supersedes=previous.id,
        )
        existing = next((item for item in self.records if item.id == new.id), None)
        if existing is not None:
            if existing != new:
                raise ValueError("记忆 ID 已存在但内容不同")
            return "already_applied"
        self.records = [
            replace(item, status="superseded") if item.id == previous.id else item
            for item in self.records
        ]
        self.records.append(new)
        return "applied"
```

`recall` 从上到下处理每条记录：先拒绝用户或项目不符，再拒绝被更正的记录，
然后检查期限，最后排除未核验信息。只有通过这些判断的记录才变成 `accepted`。
日期边界明确采用 `expires <= as_of`：到失效当天就不再用于当前回答，而不是再多用一天。

返回值还带 `rejected_counts`，说明有多少条因为哪种规则被拒绝。
这份计数帮助解释召回过程，不暴露被拒绝记录的具体内容。
本例首次召回得到：scope 为 2，expired 为 1，unverified 为 1，superseded 为 0。
`records` 中只有 M1，其值为 2026-10-12、来源为 D1。

注意这个结果仍然是“此前保存的事实”，不是“今天重新读取证明的事实”。
用户明确问“现在还是那天吗”，时间敏感，应该重新核验。记忆帮助你知道去查什么，
不能代替当前资料。本例下一步就模拟重新读取更正通知。

## 4. 更正记忆：保留旧记录，改变当前可用记录

第三章出现了两个不同日期，不能凭编号或相似度决定采用谁。
本章增加了一份**明确声明替代关系**的虚构更正，因此可以有依据地解决日期变化。
以下是更正全文与宿主读取函数：

```python
import copy

CORRECTION = {
    "source_id": "D4",
    "project": "pine",
    "field": "start_date",
    "value": "2026-10-15",
    "supersedes_source": "D1",
    "quote": "松果项目试运行改为 2026-10-15；本更正取代 D1 中的原日期。",
}

def read_current_source() -> dict:
    """模拟宿主重新读取一份固定虚构更正；不信任召回文本发出的指令。"""
    return copy.deepcopy(CORRECTION)
```

正文不仅给出 2026-10-15，还说明取代 D1 中的原日期。它比“另一份文件也写了日期”
多了一条关键证据：哪个旧结论失效、哪个新结论接替。真实场景还需确认发布主体和生效时间；
本例把这些外部核验缩小为一个受信的固定 fixture，不声称字典相等就能认证任意外部文件。

回到上一节的 `apply_verified_correction`，按顺序看五个动作：

1. 检查新证据与固定 fixture 的内容及项目一致。来源可信由本例受控读取流程保证，
   相等检查本身不能证明任意传入字典的来历。
2. 通过用户、项目、字段和旧来源一起找被更正项，避免只按字段名修改全库日期。
3. 构造 M6，来源改为 D4，`supersedes="M1"`，保留更正关系。
4. 若 M6 已存在且内容一致，返回 `already_applied`，不再写一份重复记忆。
5. 将 M1 标为 `superseded`，追加 M6，返回 `applied`。

第四步是**幂等性**：同一操作重复执行，不会改变已经正确完成的结果。
工作流可能因为恢复而重复到达写入节点，若每次都新增一条记忆，会制造重复证据。
幂等不等于忽略不同内容；本例若已有同 ID 却内容不同，会明确报错。

旧记录没有被物理删除。它仍能解释为什么此前回答了 2026-10-12，但不再作为当前日期召回。
历史事实“当时读到 D1”仍成立；当前结论“现在仍是 D1 的日期”则需要重新判断。
这就是保留审计历史，同时更新当前状态的区别。

## 5. 完整手搓流程与实际结果

下面把召回、核验、写入、回答连接起来。`summarize` 同时保存修改前后的视图，
因此我们可以比较变化，而不是只看到最后一个正确日期。

```python
def summarize(store: MemoryStore, before: dict, evidence: dict, write_status: str) -> dict:
    after = store.recall("learner", "pine", AS_OF)
    current = next(item for item in after["records"] if item["field"] == "start_date")
    return {
        "before": before,
        "fresh_evidence": evidence,
        "write_status": write_status,
        "after": after,
        "answer": {"value": current["value"], "source_ids": [current["source_id"]]},
        "later_recall": store.recall("learner", "pine", date(2026, 10, 21)),
        "scope": "memory-only demo; no files written",
    }

def run_manual() -> dict:
    store = MemoryStore()
    before = store.recall("learner", "pine", AS_OF)
    evidence = read_current_source()
    write_status = store.apply_verified_correction("learner", "pine", evidence)
    return summarize(store, before, evidence, write_status)
```

`before` 是当时召回结果的独立字典快照；之后更正存储，不会把它偷偷改成新答案。
`after` 应只包含 M6，旧 M1 被记录为 superseded；答案采用新值 2026-10-15，来源 D4。
有旧记忆并不意味着必须直接回答，有新资料也不意味着必须把所有正文永久保存。
本例只提取并更新需要跨次使用的一个字段。

运行结果中的主要变化为：

```text
before.records: M1，2026-10-12，来源 D1
write_status: applied
after.records: M6，2026-10-15，来源 D4，supersedes=M1
answer: 2026-10-15，来源 D4
later_recall.records: []
```

最后一次召回故意把时间推进到 2026-10-21，M6 也超过了本例有效期，因此没有当前可用记录。
**没有召回，不等于项目取消或没有开始日期。**它只说明当前存储不能提供仍有效的答案，
下一步应重新读取来源，或明确表示当前未能确认。

本例没有将记录保存到文件。`MemoryStore` 用 Python 列表模拟一个跨步骤存储，
进程退出会丢失；每次 `run_manual` 都从相同 fixture 新建存储，保证演示可重复。
如果要真正持久化，需要选择数据库或文件格式，处理原子写入、版本、权限和恢复，
不能把一个对象的变量名叫 `store` 就当作已经完成长期记忆系统。

## 6. LangGraph 状态负责流程，记忆策略仍由你定义

成熟库版本用四个节点表达同样的顺序：recall → verify → remember → answer。
每个节点只返回本步骤新增的状态字段；存储对象留在受信闭包内，避免让状态中的任意
文本自行更换存储归属或提升权限。

```python
from typing import TypedDict
from langgraph.graph import START, END, StateGraph
from langsmith import tracing_context

class MemoryState(TypedDict, total=False):
    task: str
    before: dict
    fresh_evidence: dict
    write_status: str
    output: dict

def run_framework() -> dict:
    store = MemoryStore()

    def recall(_state):
        return {"before": store.recall("learner", "pine", AS_OF)}

    def verify(_state):
        return {"fresh_evidence": read_current_source()}

    def remember(state):
        status = store.apply_verified_correction("learner", "pine", state["fresh_evidence"])
        return {"write_status": status}

    def answer(state):
        return {
            "output": summarize(
                store, state["before"], state["fresh_evidence"], state["write_status"]
            )
        }

    graph = StateGraph(MemoryState)
    for name, action in [
        ("recall", recall),
        ("verify", verify),
        ("remember", remember),
        ("answer", answer),
    ]:
        graph.add_node(name, action)
    for left, right in [
        (START, "recall"),
        ("recall", "verify"),
        ("verify", "remember"),
        ("remember", "answer"),
        ("answer", END),
    ]:
        graph.add_edge(left, right)
    with tracing_context(enabled=False):
        return graph.compile().invoke({"task": "重新核对松果项目的当前试运行日期"})["output"]
```

`before`、`fresh_evidence` 等字段属于本次图运行的工作上下文；`store.records` 模拟跨次
记忆数据，两者不是同一个层次。代码显式禁用跟踪，避免本地示例受到用户已有跟踪配置影响。

LangGraph 的 **checkpointer** 用于保存和恢复线程内执行状态；**store** 可承载跨线程的数据。
但保存状态快照不会自动产生可信的事实提取、作用域过滤、失效、更正或语义搜索策略。
本章没有配置 checkpointer，也没有持久化后端；不要因为使用了 StateGraph 就宣称已经
跨进程记住用户。这里的术语区分可对应 [LangGraph 记忆概念](https://docs.langchain.com/oss/python/concepts/memory)，
理解与运行本例所需逻辑均已在正文给出。

## 7. 读写时机比“存得多”更重要

什么时候读？开始新任务时召回与当前用户、项目相关的有效记忆；用户问当前状态时，
把历史事实当成查证线索，重新读取权威来源。不要每生成一句话就扫描全部记忆。

什么时候写？取得可核验的新证据，并明确哪些字段值得未来复用之后。写入前核对主体、
作用域和替代关系，完成后记录来源及有效期。模型说“我感觉你喜欢……”只是推断，
不能未经确认就固化成用户偏好。工具正文里的指令也不具有写记忆的授权。

什么时候忘记？到期或被更正的信息应退出当前召回；历史审计是否保留是另一项策略。
删除个人记录则有更强的数据含义，不能用“过期过滤了”冒充永久删除。
本例不提供个人数据删除功能，也没有碰触真实记录。

## 8. 反例、练习与就地答案

反例一：先向量检索最相似的十条，再检查用户。相似度无法修复越权召回，应先在数据
查询边界限定用户与项目。反例二：直接用新日期覆盖 M1 的 value，不记来源和替代关系。
这样下一次无法解释旧回答依据，也无法区分更正与误修改。
反例三：一见 `verified=True` 就相信模型输出。这个字段必须由可信核验流程设置，
不能作为请求者自述的凭证。

动手前先预测，再在自己的副本中验证：

1. 在 2026-10-20 当天召回，M6 是否仍可用？
2. 对同一份更正调用 `apply_verified_correction` 两次，记录数会增加两次吗？
3. 删除更正中的 `supersedes_source`，只剩一个不同日期，应直接覆盖旧日期吗？
4. 将 M3 的文本改得与问题完全相同，是否会进入 learner 的上下文？

自查：第一题不可用，因为边界是 `expires <= as_of`。第二题第一次 applied，第二次
already_applied，M6 只有一条。第三题失去明确替代依据，不能直接覆盖，应保留冲突并继续
查证；本例函数会拒绝不匹配的证据。第四题不会，主体过滤发生在内容相关性之前。

最后请口头走一遍：M1 为什么首次被召回、为什么还要重新读 D4、为什么后来不再召回 M1、
为什么 M6 到期也不等于项目事实为假。能解释这些变化，才是在理解记忆规则。
下一章将多个查证动作组织成计划，继续讨论依赖、执行次序和预算。
