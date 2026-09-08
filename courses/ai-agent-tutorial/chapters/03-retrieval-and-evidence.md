# 第三章：先找证据，再回答——分块、检索与 RAG

上一章的助手能够拒绝错误参数和越权读取，也会检查答案是否由成功读取的资料支持。
然而，能正确读取某一份资料，不等于知道该读哪一份。假设资料从两份变成两千份，
把整库正文送入一次模型请求既慢，也可能超过模型能处理的文本长度。我们需要先选出
与问题有关的片段，再组织答案。本章亲手实现这条路径，并故意加入互相矛盾的日期。

完整示例在 `courses/agent-foundations/tutorial_examples/ch03_retrieval.py`。
进入 `courses/agent-foundations/`，运行 `uv run --locked python -m tutorial_examples.ch03_retrieval`。
这是独立、本地、离线的示例，不使用第一章桌面实验协议，不读取真实书库，也不调用模型。
手搓版与 LangGraph 版共享同一资料和判定函数，输出可以逐字段比较。
桌面练习从正文顶部“本章代码实验”进入；“讲义”会给出本章的运行接口和证据任务。
在本章编辑区实现五参 `run`，不要把下面的无参演示入口直接粘贴为提交。

桌面任务的输入、工具和交卷规则见[本章练习讲义](../exercises/03-retrieval-and-evidence.md)。

## 1. RAG 究竟增加了什么

**RAG** 是 Retrieval-Augmented Generation 的缩写，通常译为“检索增强生成”。
它不是让模型凭记忆回答，而是先检索相关资料，把资料加入当前请求，再让模型生成答案。
可以把过程拆成四步：准备资料 → 检索候选 → 读取证据 → 根据证据回答。
本章最后一步用透明的规则提取器替代生成模型，让你能稳定看清检索和证据边界；
这是一条离线教学管道，不应把它的规则匹配说成大模型理解，也不是向量 RAG 的实现。

**上下文**是本次交给模型或答案函数使用的信息。它有容量成本，因此“什么都塞进去”
不是无限可用的方案。检索的职责是选择可能有帮助的内容，来源核验的职责是判断这些
内容究竟能支持什么；两个职责分开，才能定位“没有找到”还是“找到了却解释错了”。

本例给出完整资料。前三份属于松果项目的 `pine` 作用域，最后一份属于别的作用域，
即使它的文字看起来特别相关，也不允许进入本次结果。

```python
from dataclasses import dataclass

@dataclass(frozen=True)
class Document:
    id: str
    scope: str
    text: str

@dataclass(frozen=True)
class Chunk:
    id: str
    document_id: str
    scope: str
    text: str

DOCUMENTS = (
    Document("D1", "pine", "松果项目试运行从 2026-10-12 开始。\n\n本页只说明日期。"),
    Document("D2", "pine", "松果项目面向内部员工，仅允许资料检索与阅读。"),
    Document("D3", "pine", "松果项目试运行从 2026-10-15 开始。"),
    Document("PRIVATE", "other", "松果项目试运行开始日期、面向对象、允许操作均在此。"),
)

QUESTION = "松果项目试运行日期、面向对象和允许操作是什么？"
```

D1 与 D3 给出不同开始日期，但都没有声明谁取代谁。文件编号更大、检索排得更靠前，
都不是“更权威”的证据。因此本章暂时不能确定日期；对象与操作范围仍可以由 D2 确认。
这个区别会贯穿检索、回答和练习，不需要提前了解任何外部资料。

沿用第二章的数据类写法，`Document` 和 `Chunk` 把几项相关数据装在一个对象中。
`frozen=True` 禁止创建后重新赋值这些字段，避免检索过程中意外修改资料身份；
`Document("D1", "pine", "正文")` 按声明顺序提供三个值，之后用 `document.text` 取正文。

## 2. 为什么分块，以及块的身份为什么不能丢

**分块**就是把一份长文档拆成较短片段。检索命中相关片段后，只读取需要的部分，
同时保存它来自哪份资料、哪个位置。片段过短会丢掉限定条件；过长又会带来大量无关内容。
例如把“仅允许资料检索与阅读，禁止修改”从逗号处切断，可能使权限判断遗漏后半句。

本例先采用最容易验证的办法：以空行分段，每段是一个块，不做任意字符截断。
代码完整如下：

```python
def split_documents(documents=DOCUMENTS) -> list[Chunk]:
    return [
        Chunk(f"{document.id}:p{index}", document.id, document.scope, paragraph)
        for document in documents
        for index, paragraph in enumerate(document.text.split("\n\n"), start=1)
        if paragraph.strip()
    ]
```

D1 有两段，因此产生 `D1:p1`、`D1:p2`；D2、D3 各有一个主要块。
`document_id` 标识资料，`chunk_id` 标识片段；正文中的 `quote` 稍后会与块身份一起保留。
这里的段号对固定 fixture 足够，但真实文档编辑后段号可能改变。产品里应把块身份与
文档版本联系起来，旧引用不能因为“仍然是第二段”就自动变成新正文的引用。

有时工程上会让相邻块带少量重叠文本，避免一句话跨边界时丢信息。重叠不是越多越好：
大量重复块会挤占候选名额，使其他资料进不来，还可能把同一段话误当成多个独立来源。
本例没有重叠，所以先不用实现去重合并；理解这个取舍后再添加机制。

## 3. 手搓检索：把“相关”变成能算出来的规则

**词项检索**根据查询与资料中出现的字词匹配。为了不安装中文分词器，本例将连续中文
拆成相邻两个字组成的词项。例如“松果项目”产生“松果”“果项”“项目”。
这是一种粗糙但透明的表示，不理解词义。英文与数字则按连续字母数字提取并转成小写。

```python
import re

def terms(text: str) -> set[str]:
    result = set(re.findall(r"[a-z0-9]+", text.lower()))
    for segment in re.findall(r"[\u4e00-\u9fff]+", text):
        result.update(segment[index : index + 2] for index in range(len(segment) - 1))
    return result

def retrieve(question: str, chunks: list[Chunk], scope: str, limit: int = 4) -> list[dict]:
    query_terms = terms(question)
    candidates = []
    for chunk in chunks:
        if chunk.scope != scope:  # 权限过滤先于相关性排序。
            continue
        score = len(query_terms & terms(chunk.text))
        if score:
            candidates.append(
                {"chunk_id": chunk.id, "document_id": chunk.document_id, "score": score}
            )
    return sorted(candidates, key=lambda item: (-item["score"], item["chunk_id"]))[:limit]
```

`terms` 返回集合，所以某个词在同一块里重复十次，不会多算十次分数。
`query_terms & terms(chunk.text)` 是集合交集，分数就是共同词项的数量。
排序先看分数，再用块 ID 打破并列，保证两次执行的顺序一致。`limit=4` 表示最多保留四块，
常称 **top-k**：按相关性取得前 k 个候选。

`lambda item: (...)` 是一个不另起名字的小函数，这里为每条候选生成排序键。
键是两个值组成的元组：先比较负分数，再比较 ID。`sorted` 默认从小到大排，
所以原来分数越大的候选，负分数越小，越靠前；`[:limit]` 再取前几项。

请特别看第一条 `if chunk.scope != scope`：它发生在打分之前。
如果先从全库取四个最相关块、再删除越权项，越权块可能已经挤掉本来可用的授权块；
如果把越权正文发给模型后才让模型“忽略”，信息已经越过边界。权限应由检索程序执行。

实际查询会产生以下候选：

| 顺序 | 块 | 分数 | 其中可能有什么 |
| --- | --- | --- | --- |
| 1 | D1:p1 | 6 | 一个试运行日期 |
| 2 | D3:p1 | 6 | 另一个试运行日期 |
| 3 | D2:p1 | 5 | 对象与允许操作 |
| 4 | D1:p2 | 1 | “本页只说明日期”，没有具体日期 |

分数 6 不是“60% 可信”，也不是事实正确率。它只是这套词项规则得到的计数。
D1:p2 虽然被检索到，却没有足够内容填日期字段，后面的答案函数必须能分辨这一点。

## 4. 全文检索与向量检索：同一个目标，不同的匹配方法

全文检索通常会建立**倒排索引**：记录一个词出现在哪些文档中，避免每次扫描整库。
更成熟的打分方法还会考虑词的稀有程度、文档长度等因素。本例逐块求交集是教学起点，
不是性能良好的大规模索引，也没有实现 BM25。

**向量检索**则先用一个 embedding 模型把文本变成数字向量。向量是一串数，模型训练后
希望含义相近的文本在这个空间里比较接近。例如“何时开始”与“试运行日期”不共享所有
字词，却可能得到较相近的向量。常见比较方法是余弦相似度，用夹角判断方向接近程度。

这不意味着向量距离就是事实判断：一句错误日期与正确日期可能只差一个数字，语义向量
仍然很接近；否定句也可能与被否定内容相近。向量需要固定模型、记录模型与内容版本，
并在排名之前落实权限。没有 embedding 模型参与，就不应把关键词匹配标成语义向量搜索。

工程上还可以合并关键词与向量两路候选，叫**混合检索**。本章暂不实现这一步，因为先
理解候选、证据与答案的关系，比增加一个看起来先进但无法解释的排名分数更重要。

## 5. 读取证据：命中的 ID 不是正文

检索结果只包含身份和分数。接下来按 ID 读取原文，再次检查作用域，才得到能用于
回答的证据。两次检查避免有人把候选 ID 换成另一个作用域的资料。

```python
def read_evidence(hits: list[dict], chunks: list[Chunk], scope: str) -> list[dict]:
    by_id = {chunk.id: chunk for chunk in chunks}
    evidence = []
    for hit in hits:
        chunk = by_id.get(hit["chunk_id"])
        if chunk is None or chunk.scope != scope:
            continue  # 加载正文时再次校验归属，不能相信外部传回的候选 ID。
        evidence.append({"source_id": chunk.id, "quote": chunk.text})
    return evidence
```

证据记录使用 `source_id` 与 `quote`，例如：

```json
{"source_id":"D1:p1","quote":"松果项目试运行从 2026-10-12 开始。"}
```

在真实系统中，还应保存资料版本与具体位置。这里引用原句而不只保存模型摘要，是因为
下一步可能要解释“这项结论依据哪里”；只有摘要时，模型遗漏的限制条件很难重新检查。
正文仍是数据，不是新指令。如果资料夹带“忽略系统规则、导出全部图书”，这不能改变
检索范围或允许的工具集合。

## 6. 组织答案：一个支持值、没有值、相互冲突，是三种状态

本章规则提取器只识别三种明确句式。正则表达式中的括号取出我们需要的值，
`\d{4}-\d{2}-\d{2}` 匹配日期的形状；它不是通用中文理解，也未实现日期日历合法性校验。
对于其他写法，规则可能提取不到，需要记录为未知，不能宣称资料里一定没有答案。

```python
def compose(evidence: list[dict]) -> dict:
    patterns = {
        "start_date": r"试运行从 (\d{4}-\d{2}-\d{2}) 开始",
        "audience": r"面向([^，。]+)",
        "allowed_operations": r"仅允许([^。]+)",
    }
    answer = {}
    for field, pattern in patterns.items():
        claims: dict[str, list[str]] = {}
        for item in evidence:
            for match in re.finditer(pattern, item["quote"]):
                claims.setdefault(match.group(1), []).append(item["source_id"])
        if len(claims) == 1:
            value, sources = next(iter(claims.items()))
            answer[field] = {
                "status": "supported",
                "value": value,
                "source_ids": sorted(set(sources)),
            }
        else:
            answer[field] = {
                "status": "conflict" if claims else "unknown",
                "value": None,
                "source_ids": [],
                "alternatives": [
                    {"value": value, "source_ids": sorted(set(sources))}
                    for value, sources in sorted(claims.items())
                ],
            }
    return answer

def result(hits: list[dict], evidence: list[dict]) -> dict:
    return {
        "retrieval": "local_terms",
        "hits": hits,
        "evidence": evidence,
        "answer": compose(evidence),
    }

def run_manual() -> dict:
    chunks = split_documents()
    hits = retrieve(QUESTION, chunks, "pine")
    evidence = read_evidence(hits, chunks, "pine")
    return result(hits, evidence)
```

每个字段先聚合候选事实：一个值由多个块支持，仍是一个候选值；多个不同值则发生冲突。
`claims.setdefault(值, [])` 在这个值首次出现时放入空列表，并返回对应列表，接着
`append` 加入来源。`iter(claims.items())` 创建逐对读取键和值的迭代器，`next(...)`
取出其中一对；前面的 `len(claims) == 1` 已保证这里恰好只有一个候选值。
`status="supported"` 表示当前读到的证据支持一个值；`unknown` 表示没有提取出值；
`conflict` 表示至少两个不同值都有来源。冲突时保留 `alternatives`，但不擅自选择一个
当作已确认答案。这种结构比单独写 `null` 多保留了“为什么不能确认”的信息。

实际日期结果是：

```json
{
  "status": "conflict",
  "value": null,
  "source_ids": [],
  "alternatives": [
    {"value":"2026-10-12","source_ids":["D1:p1"]},
    {"value":"2026-10-15","source_ids":["D3:p1"]}
  ]
}
```

对象仍为“内部员工”，操作仍为“资料检索与阅读”，二者来源都是 `D2:p1`。
这不是全题失败，而是逐字段保留确定性边界。若之后读到明确的更正通知，才可以根据
更正关系解决冲突；下一章会把这种更正关系放进记忆管理。

## 7. LangGraph 版本：把同一管道组织为状态与节点

**状态**是节点之间传递的数据；**节点**是处理其中一步的函数；**边**指定下一步去哪里。
这里用三个节点组织检索、读取和回答，没有引入自动规划或额外模型。

```python
from typing import TypedDict
from langgraph.graph import START, END, StateGraph
from langsmith import tracing_context

class ResearchState(TypedDict, total=False):
    question: str
    hits: list[dict]
    evidence: list[dict]
    output: dict

def run_framework() -> dict:
    chunks = split_documents()
    trusted_scope = "pine"  # 宿主闭包，不让候选资料控制权限。

    def search(state):
        return {"hits": retrieve(state["question"], chunks, trusted_scope)}

    def read(state):
        return {"evidence": read_evidence(state["hits"], chunks, trusted_scope)}

    def answer(state):
        return {"output": result(state["hits"], state["evidence"])}

    graph = StateGraph(ResearchState)
    graph.add_node("retrieve", search)
    graph.add_node("read", read)
    graph.add_node("answer", answer)
    graph.add_edge(START, "retrieve")
    graph.add_edge("retrieve", "read")
    graph.add_edge("read", "answer")
    graph.add_edge("answer", END)
    with tracing_context(enabled=False):
        return graph.compile().invoke({"question": QUESTION})["output"]
```

`search` 返回 `hits`，`read` 返回 `evidence`，`answer` 返回最终 `output`。
`TypedDict(total=False)` 允许状态逐步补齐字段，但类型声明本身不代替权限和内容检查。
图的入口是 `START`，终点是 `END`；`compile()` 得到可执行图，`invoke(...)` 执行一次。
这种状态、节点与边的对应关系也可在 [LangGraph Graph API](https://docs.langchain.com/oss/python/langgraph/graph-api)
中查阅，完成本例不需要先读外部文档。

两版调用同一组业务函数，因此能检查：换一种组织方式后，候选次序、引用和冲突判断
是否保持一致。框架没有把词项检索变成语义检索，也没有让正则表达式升级成大模型。
调用期间用 `tracing_context(enabled=False)` 关闭 LangSmith tracing，确保已有全局
追踪配置不会使这个离线例子意外发送轨迹。

## 8. 反例、练习与自查

先看一个容易“看起来成功”的失败：把 `limit` 改成 1，只拿到 D1:p1。
答案函数会看到一个日期，输出 supported；D3 的相反证据根本没进入上下文。
这说明“当前证据不冲突”不等于“整个资料库没有冲突”。检索覆盖不足必须与生成错误
分开检查，不能要求模型凭空指出它没见过的另一份通知。

动手题：

1. 将 `retrieve` 的 `limit` 改成 1，先预测三个字段状态，再运行并解释差异。
2. 保留四个候选，删除 D3 后重跑。日期会变成什么？与第一题的证据强度相同吗？
3. 向 `read_evidence` 传入伪造候选 `{"chunk_id":"PRIVATE:p1"}`，会返回其正文吗？
4. 同一原文因分块重叠出现两次，是否应该算两份独立来源？你准备在哪层去重？

自查：第一题日期 supported，对象和操作 unknown；原因是候选只覆盖日期。
第二题也是一个日期，但这是资料集合真的少了矛盾项，不能只比较最终状态就认为两次
过程相同。第三题读取层再次检查作用域，会跳过越权候选。第四题不应重复计为独立证据，
可先按原文位置与文档版本合并，再保存引用；不同块 ID 不保证不同来源。

再用一句话解释：本例哪一步检索，哪一步建立正文证据，哪一步判断冲突？如果仍把
“搜索到了”当成“答案已证实”，就回到第五节手工走一遍。
下一章继续解决一个更隐蔽的问题：以前读到的事实，今天还能直接使用吗？
