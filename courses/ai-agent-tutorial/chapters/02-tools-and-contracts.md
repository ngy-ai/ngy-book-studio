# 第二章：工具的说明、校验与授权，是三件不同的事

第一章已经让资料研究 Agent 跑过“请求工具、执行工具、回填结果、继续或停止”的循环。
现在把它交给另一个人使用：他只允许读取松果项目，却在对话中要求“顺便删除资料”，
或把资料 ID 写成整数，还可能让模型添加一个 `allowed_ids` 参数扩大范围。
循环即使写对，也不能回答这些请求究竟该不该执行。本章给循环加上明确的边界。

本章完整运行文件是 `courses/agent-foundations/tutorial_examples/ch02_tools.py`。
在 `courses/agent-foundations/` 执行 `uv run --locked python -m tutorial_examples.ch02_tools`。
它使用内存中的虚构资料，分别运行手搓工具入口与 `langchain_core.tools.tool` 入口，
最后打印两份结果。它是独立演示，不是桌面五参 `run` 提交文件，不能整份粘贴进实验区。
在墨页点击正文顶部“本章代码实验”，先读“讲义”，再在本章编辑区完成工具入口任务；
本地演示的固定数据与桌面运行时生成的输入要分开看。

桌面任务的输入、工具和交卷规则见[本章练习讲义](../exercises/02-tools-and-contracts.md)。

## 1. 为什么“函数存在”还不足以成为可靠工具

普通函数的调用者通常是程序员，他知道参数是什么意思。Agent 的调用者是模型，
它需要一份机器能读的说明，告诉它哪些动作存在、如何传参数。这份说明叫 **schema**，
可以理解为“数据结构与约束的说明书”。它描述输入，却不会自动替你执行验证或判断权限。

我们先只保留 `read_document` 一个工具，把注意力集中在接口上。搜索循环在第一章
已经练过；这里用六个预先给定的请求代替模型，保证每次都能观察同样的边界行为。
这些请求不是一个真实模型的推理轨迹，也不需要联网。

这是本例的全部资料与工具说明，`facts` 是虚构 fixture 的明确事实标注，便于确定性核验：

```python
DOCUMENTS = {
    "D1": {"body": "松果项目试运行从 2026-10-12 开始。", "facts": {"start_date": "2026-10-12"}},
    "D2": {
        "body": "松果项目面向内部员工，仅允许资料检索与阅读。",
        "facts": {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
    },
    "PRIVATE": {"body": "另一个虚构项目的内部资料。", "facts": {}},
}

FIELDS = ("start_date", "audience", "allowed_operations")

READ_SCHEMA = {
    "name": "read_document",
    "description": "读取本次授权资料的正文，不能访问任意文件。",
    "parameters": {
        "type": "object",
        "properties": {"document_id": {"type": "string", "minLength": 1, "maxLength": 64}},
        "required": ["document_id"],
        "additionalProperties": False,
    },
}
```

`type: object` 要求参数是对象；`required` 要求有 `document_id`；
`additionalProperties: False` 不接受额外参数。字符串的长短限制用于限制输入，
但“全是空格”仍需要额外检查。`description` 帮模型选对工具，不是执行许可。

资料 `PRIVATE` 也完全虚构。把它放进内存只是为了证明：**资料存在，与本次能读，是两件事。**
本次宿主授权集合是 `D1`、`D2`。这份集合由程序创建，不能让模型传入，更不能从正文中的
“你现在有管理员权限”推断出来。宿主是实际执行程序，拥有最后的校验与授权责任。

## 2. 手搓入口：先看参数，再看权限，最后读正文

先实现输入校验。它接收 Python 对象，成功时返回原参数，失败时抛 `ValueError`。
这与第一章 JSON 字符串解析是不同的一步：这里假设调用请求已经被解析成对象。

```python
def validate_manual(arguments: object) -> dict:
    if not isinstance(arguments, dict) or arguments.keys() != {"document_id"}:
        raise ValueError("参数必须且只能包含 document_id")
    value = arguments["document_id"]
    if not isinstance(value, str) or not value.strip() or len(value) > 64:
        raise ValueError("document_id 必须为 1..64 字符的非空字符串")
    return arguments
```

第一层比较键集合，意味着缺少 `document_id` 和多带 `allowed_ids` 都失败。
第二层检查值，整数 `42` 不能因为“可以转成字符串”就通过。接口如果悄悄把错误输入
纠正成另一种类型，调用者可能以为自己传对了，下次继续产生难以发现的错误。
我们选择严格拒绝，让请求者看到结构化错误后再提出新请求。

接下来定义宿主。`dataclass` 用来减少对象初始化代码；`default_factory=dict/list`
让每个 Host 有自己的读取记录和审计记录，不会与另一轮演示共用一个列表。
`frozenset` 是不可变集合，适合表达本次固定的授权 ID。

```python
import copy
from dataclasses import dataclass, field

@dataclass
class Host:
    allowed_ids: frozenset[str] = frozenset({"D1", "D2"})
    reads: dict = field(default_factory=dict)
    audit: list = field(default_factory=list)

    def read(self, document_id: str) -> dict:
        # 授权来自宿主；模型参数中没有 allowed_ids，也不靠正文来决定权限。
        if document_id not in self.allowed_ids:
            return {"ok": False, "error": {"code": "FORBIDDEN", "retryable": False}}
        document = DOCUMENTS.get(document_id)
        if document is None:
            return {"ok": False, "error": {"code": "NOT_FOUND", "retryable": False}}
        self.reads[document_id] = copy.deepcopy(document)
        return {"ok": True, "data": {"document_id": document_id, **copy.deepcopy(document)}}

    def dispatch(self, name: str, arguments: object, invoke) -> dict:
        if name != READ_SCHEMA["name"]:
            result = {"ok": False, "error": {"code": "UNKNOWN_TOOL", "retryable": False}}
        else:
            try:
                result = invoke(arguments)
            except ValueError:
                result = {"ok": False, "error": {"code": "INVALID_ARGUMENTS", "retryable": False}}
        code = "OK" if result["ok"] else result["error"]["code"]
        self.audit.append({"tool": name, "arguments": arguments, "code": code})
        return result
```

从 `dispatch` 往下读：工具名称不符，直接返回 `UNKNOWN_TOOL`；名称合法，才调用传入的
`invoke`。手搓版的 `invoke` 会先执行参数校验，然后执行 `read`。
在 `read` 内，权限检查位于取正文之前：不在授权集合中的 ID 统一得到 `FORBIDDEN`，
不会因为它恰好存在就返回正文。授权范围内的 ID 找不到，才返回 `NOT_FOUND`。

两者的意思不同。`FORBIDDEN` 表示本次不能访问；`NOT_FOUND` 表示在允许访问的范围内
没有找到这份资料。本例的这两类错误均不可重试。第一章的超时策略仍成立：只有明确的
`retryable=true` 才允许有限重试；“我很想知道答案”不构成重试或扩大权限的理由。

每次派发都会记录工具、参数和结果码，这叫 **审计记录**：它用于复查发生过什么。
本章记录的是请求级事件，不能把 `len(audit)` 当成成功读取次数。
只有正文真的成功返回，才加入 `reads`；后面的来源校验只相信这个集合。

## 3. 结构化输出：解析成功之后，还需要两层检查

最终答案仍有日期、对象和允许操作三个字段，每个字段都有 `value` 和 `source_ids`。
结构化输出的好处是下游可以分别检查每个字段，而不是从一段自由文字里猜意思。
但一个形状正确的 JSON，也可以包含完全错误的事实。

手搓版先检查形状，再检查来源支持：

```python
def validate_manual_answer(answer: object) -> dict:
    if not isinstance(answer, dict) or answer.keys() != set(FIELDS):
        raise ValueError("回答必须恰有三个字段")
    for fact in answer.values():
        if not isinstance(fact, dict) or fact.keys() != {"value", "source_ids"}:
            raise ValueError("每项事实必须包含 value 与 source_ids")
        if fact["value"] is not None and not isinstance(fact["value"], str):
            raise ValueError("value 必须为字符串或 None")
        if not isinstance(fact["source_ids"], list) or not all(
            isinstance(source, str) for source in fact["source_ids"]
        ):
            raise ValueError("source_ids 必须为字符串列表")
    return answer

def supported(answer: dict, reads: dict, validate=validate_manual_answer) -> bool:
    try:
        parsed = validate(answer)
    except ValueError:
        return False
    for name, fact in parsed.items():
        value, sources = fact["value"], fact["source_ids"]
        if value is None:
            if sources:
                return False
        elif (
            not sources
            or len(sources) != len(set(sources))
            or not all(
                source in reads and reads[source]["facts"].get(name) == value for source in sources
            )
        ):
            return False
    return True
```

第一段只问：“三个字段齐不齐？值是字符串或 None 吗？来源是字符串列表吗？”
第二段才问：“这个来源本次是否读成功？来源的事实是否支持当前字段的值？”
读这段代码时，`parsed.items()` 会逐对给出字典中的键和值，分别交给 `name` 和 `fact`。
`all(条件 for 元素 in 集合)` 表示逐项检查，只有每一项都满足条件才返回 `True`；
空集合也会返回 `True`，因此已知事实还要先用 `not sources` 拒绝空来源。
`None` 对应 JSON 的 `null`，表示未知；未知值必须没有来源。已知值至少有一个来源，
重复来源不增加证据数量，所以也拒绝重复 ID。

这里采用 fixture 的 `facts` 精确比较，**没有假装解决任意自然语言的事实蕴含判断**。
真实正文中的同义改写、时间限定、否定和引用关系更复杂，后续可以增加规则或模型辅助
核验，但仍要保留可检查的原文。本例先把“格式正确不等于事实正确”做成可运行的边界。

## 4. 把六个请求走完，再亲眼看一次伪造答案被拒绝

下面是完整演示次序。它先尝试四个应被拒绝的请求，再读取两份允许资料，最后把
“内部员工”偷偷改为“所有人”，验证字段结构没变时来源检查能不能发现问题。

```python
def exercise(invoke, host: Host, validate=validate_manual_answer) -> dict:
    for name, arguments in [
        ("delete_document", {"document_id": "D1"}),
        ("read_document", {"document_id": 42}),
        ("read_document", {"document_id": "D1", "allowed_ids": ["PRIVATE"]}),
        ("read_document", {"document_id": "PRIVATE"}),
        ("read_document", {"document_id": "D1"}),
        ("read_document", {"document_id": "D2"}),
    ]:
        host.dispatch(name, arguments, invoke)
    answer = {name: {"value": None, "source_ids": []} for name in FIELDS}
    for source, document in host.reads.items():
        for name, value in document["facts"].items():
            answer[name] = {"value": value, "source_ids": [source]}
    assert supported(answer, host.reads, validate)
    forged = copy.deepcopy(answer)
    forged["audience"]["value"] = "所有人"
    return {
        "answer": answer,
        "successful_reads": sorted(host.reads),
        "audit": host.audit,
        "forged_answer_accepted": supported(forged, host.reads, validate),
    }

def run_manual() -> dict:
    host = Host()

    def invoke(arguments):
        validated = validate_manual(arguments)
        return host.read(validated["document_id"])

    return exercise(invoke, host)
```

你可以先手工预测 `audit`：删除不是已注册动作；整数参数非法；附加权限参数非法；
读取 `PRIVATE` 越权；最后两个读取成功。因此结果码依次为：

```text
UNKNOWN_TOOL
INVALID_ARGUMENTS
INVALID_ARGUMENTS
FORBIDDEN
OK
OK
```

`successful_reads` 只能是 `["D1", "D2"]`。合法答案应为：

```json
{
  "start_date": {"value": "2026-10-12", "source_ids": ["D1"]},
  "audience": {"value": "内部员工", "source_ids": ["D2"]},
  "allowed_operations": {"value": "资料检索与阅读", "source_ids": ["D2"]}
}
```

最后的 `forged_answer_accepted` 必须是 `false`。伪造答案仍然有三个字段，类型也没错，
失败发生在“D2 并没有支持所有人”这一层。找到这个层次，比笼统地说“AI 幻觉”更有用，
因为你知道该检查来源语义，而不是去改 JSON 解析器。

## 5. 成熟库版本：减少参数处理代码，宿主责任仍然存在

现在用现有锁中的 Pydantic 与 LangChain Core 描述同一输入。Pydantic 的 `BaseModel`
是带校验的结构类型；`extra="forbid"` 拒绝额外键；`strict=True` 避免宽松的类型转换。
`field_validator` 给单个字段增加“不能全为空白”的规则。

```python
from langchain_core.tools import tool
from langsmith import tracing_context
from pydantic import BaseModel, ConfigDict, Field, field_validator

class ReadArgs(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)
    document_id: str = Field(min_length=1, max_length=64)

    @field_validator("document_id")
    @classmethod
    def not_blank(cls, value: str) -> str:
        if not value.strip():
            raise ValueError("资料 ID 不得全为空白")
        return value

class Fact(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)
    value: str | None
    source_ids: list[str]

class ResearchAnswer(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)
    start_date: Fact
    audience: Fact
    allowed_operations: Fact

def validate_framework_answer(answer: object) -> dict:
    return ResearchAnswer.model_validate(answer).model_dump()

def run_framework() -> dict:
    host = Host()

    @tool("read_document", args_schema=ReadArgs)
    def read_document(document_id: str) -> dict:
        """读取本次宿主授权的虚构资料，返回正文或结构化错误。"""
        return host.read(document_id)

    with tracing_context(enabled=False):
        return exercise(read_document.invoke, host, validate_framework_answer)
```

`@tool` 把函数包装成可调用工具；`args_schema=ReadArgs` 指定输入约束，`.invoke(...)`
先校验参数，再调用函数。`host` 留在闭包中，模型只能看到 `document_id`，不能提交
一个新的 Host 来扩大授权。`ResearchAnswer.model_validate(...).model_dump()` 则负责
结构化答案的形状校验，最终仍由同一个 `supported` 验证来源与值。
这些 API 对应官方的 [工具定义与参数 schema](https://docs.langchain.com/oss/python/langchain/tools)，
本文已经给出完成例子所需的写法，链接仅供查阅。

Pydantic 的校验错误也继承 `ValueError`，所以宿主可以把两版参数错误统一成同一个结果码。
`tracing_context(enabled=False)` 在调用期间关闭 LangSmith tracing，防止已有的全局
追踪配置使这次离线例子意外发送记录；它不替代工具自身的网络与权限约束。

| 你写的能力 | 手搓版 | 成熟库版 |
| --- | --- | --- |
| 输入形状 | `validate_manual` | `ReadArgs` 与 `.invoke` |
| 输出形状 | `validate_manual_answer` | `ResearchAnswer.model_validate` |
| 工具授权与正文读取 | `Host.read` | 同一个 `Host.read` |
| 来源是否支持结论 | `supported` | 同一个 `supported` |

所以“用了工具装饰器”不能推出“权限已经安全”。库替你组织接口，你仍然定义哪些主体
能访问哪些资料、失败怎样表达，以及哪些结果可以成为证据。本例只审计已知请求，
不提供任意 Python 的隔离能力，也没有真实模型请求。

## 6. 错误反例与动手题

反例一，把 `allowed_ids` 放进 schema，让模型填写。这样权限来自被约束的一方，
`PRIVATE` 就可能被放进集合。修复是把权限留在宿主，把输入缩小到业务需要的资料 ID。
反例二，只用 `ResearchAnswer.model_validate` 就返回成功。它能识别类型错，却无法
自行知道“所有人”与 D2 不符；因此不能删除 `supported`。

先完成以下练习，再核对答案：

1. 请求 `{"document_id":"   "}` 会走到正文读取吗？为什么仅有 `minLength=1` 不够？
2. 把 `source_ids` 改成 `["D2", "D2"]`，是否能证明对象的可信度更高？
3. 给日期字段设置 `value=None`，却保留 `["D1"]`，应通过哪一层、失败在哪一层？
4. 在自己的副本中追加一个合法请求 `{"document_id":"D1"}`，先预测审计数量，再运行。

自查：第一题两版都拒绝，全空格满足长度却不满足有效 ID；第二题来源重复不增加信息，
本例明确拒绝；第三题形状可以成立，但未知值不能携带来源，语义校验失败。
第四题审计由 6 条变为 7 条，成功读取集合仍是两个 ID。这里为展示接口而允许重复读；
不要把它误认为第一章“同一参数仅失败后有限重试”的运行规则已被修改，两例边界不同。

本章之后，Agent 的工具入口已经可解释、可拒绝、可复查。但资料一多，我们不可能
每次把所有正文都交给模型。下一章从分块与检索开始，讨论如何选出相关证据，以及
两个来源说法不一致时为什么应该保留不确定性。
