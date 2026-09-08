# 第一步：从松果项目的问题开始

讲义修订：`2026-09-07.tutorial-1`；实验与评分契约：`1.0.0`。

从课程导读来到这里，我们先不安装新框架，而是把一个具体问题变成程序。你会先看到
普通 Python 怎样保存、读取资料，再理解为什么还需要模型参与判断。所有必要解释、
示范和自查答案都在正文；可以连续读完，不必先找参考书或打开参考解答。

## 1. 要解决的不是聊天，而是根据资料回答问题

有人问：“松果项目什么时候开始试运行，谁可以使用，允许做什么？”如果只把问题
发给模型，模型并没有收到本次项目通知。即使答得像真的，也不能证明它查过资料。
本章要求每项结论都有正文依据；不知道的字段就明确保留未知。

我们准备了两份虚构资料：

| 资料 | 标题 | 正文说明 |
| --- | --- | --- |
| D1 | 松果项目：试运行安排 | 试运行从 2026-10-12 开始，只说明时间 |
| D2 | 松果项目：参与和操作范围 | 面向内部员工，仅允许资料检索与阅读 |

人会搜索项目名、打开通知、整理答案。我们让模型提出下一步请求，让程序真正执行
搜索或读取，再把结果交回模型。这种围绕目标反复“判断 → 行动 → 反馈”的程序，就是
本章的 Agent。这里的行动只有两个只读工具，不涉及自主修改电脑或访问真实图书库。

如果所有任务永远只读这两份固定资料，普通固定流程也足够。加入模型的意义是让它
根据问题和已得信息选择下一步；加入宿主程序的意义是保证请求按规则执行。第二步
会展开这个协作过程，现在先把装资料和传结果的 Python 基础看清楚。

## 2. 字典装一份资料，列表装多份记录

下面是一段完整的 Python 小程序，保存为普通 `.py` 文件即可运行：

```python
document = {
    "document_id": "D1",
    "title": "松果项目：试运行安排",
    "body": "试运行从 2026-10-12 开始。",
}
found_documents = [document]

print(document["title"])
print(found_documents[0]["document_id"])
```

输出两行：`松果项目：试运行安排` 和 `D1`。大括号建立字典 `dict`，通过键找到值；
`document["body"]` 就是取正文。方括号建立列表 `list`，通过位置取元素，下标从 0
开始，所以 `found_documents[0]` 先取第一份资料，再用 `["document_id"]` 取其 ID。

`document`、`found_documents` 是变量名。变量指向一个对象，不是每次赋值都复制
对象。例如把上段代码接着写成下面这样：

```python
same_document = document
same_document["title"] = "修订后的试运行安排"
print(found_documents[0]["title"])
```

输出也会变成 `修订后的试运行安排`：两个变量和列表里的元素都指向同一个字典。
如果改成 `same_document = {}`，只是让这个名字指向新的空字典，原来的资料不变。
这一区别会影响后面“保留历史”与“清空历史”的行为。接下来把读取动作装进函数。

## 3. 函数接收参数，返回结果；调用者负责保存

下面是另一段完整小程序。这里的 `documents` 是演示用资料存储，不是 Agent 的答案：

```python
documents = {
    "D1": "试运行从 2026-10-12 开始。",
    "D2": "面向内部员工，仅允许资料检索与阅读。",
}


def read_demo(document_id):
    return documents[document_id]


body = read_demo("D1")
print(body)
```

`def` 定义函数；调用 `read_demo("D1")` 时，参数 `document_id` 接到 `"D1"`。
`return` 把找到的正文交回调用位置，并结束这次函数执行。变量 `body` 保存这个返回值。
程序输出 `试运行从 2026-10-12 开始。`。

如果只写 `read_demo("D1")`，函数依然运行，但程序没有保存其返回值。模型不会因此
自动知道正文；后面你还要把结果加入发给模型的消息列表。函数返回与消息回填是两个
连续动作。现在我们用一个列表记录前一个函数的返回结果。

## 4. 用循环保存多次结果，不在每一轮重新开始

下面代码接在上一段后面：

```python
messages = []
for document_id in ["D1", "D2"]:
    body = read_demo(document_id)
    messages.append({"document_id": document_id, "body": body})

print(len(messages))
print(messages[0]["document_id"], messages[1]["document_id"])
```

`for` 逐个取列表元素，先让 `document_id` 等于 D1，执行缩进内两行，再换成 D2。
`append` 在现有列表末尾加一项；`len` 计算元素数量。输出是 `2` 和 `D1 D2`。
两次读取都留了记录。如果把 `messages = []` 移进 `for` 的缩进里，每一轮都会创建
空列表，最后只剩 D2。这就是“运行过”和“结果还在”之间的差别。

这段是固定的两次读取，没有模型判断，所以还不是完整 Agent。下一步会让模型产生
调用列表，再由你的程序逐个处理。正常路径看清后，还需要看读取失败会怎样。

## 5. 异常、continue、break 和 pass 分别做什么

访问不存在的字典键会抛出 `KeyError`。异常表示正常路径被中断，可以用 `try/except`
接住特定问题并走错误分支。下面是可单独运行的例子：

```python
documents = {"D1": "日期正文", "D2": "范围正文"}
events = []

for document_id in ["D1", "X0", "D2"]:
    try:
        body = documents[document_id]
    except KeyError:
        events.append((document_id, "没有找到"))
        continue
    events.append((document_id, body))

print(events)
```

输出为 `[('D1', '日期正文'), ('X0', '没有找到'), ('D2', '范围正文')]`。圆括号在这里
建立元组 `tuple`，用来放一组不需要原地修改的值。X0 不存在时，取正文的语句失败，
程序进入 `except`，记录错误，再由 `continue` 开始下一轮。`continue` 不结束整个
循环；如果换成 `break`，循环就停止，D2 不会再处理，已保存的记录仍保留。

你也可能见到 `except KeyError: pass`。`pass` 是 Python 的空语句，意思是“此处
什么也不做”，通常用作占位。它不会记录错误，不等于重试，也不等于成功。如果
后面的代码还使用 `body`，可能误用上一轮旧值，或遇到变量尚未赋值的问题。
因此本章的工具失败必须形成明确结果，不能靠忽略异常让程序看起来继续运行。

本章受控工具会把常见超时和缺失转换成错误数据；预算耗尽则使用专门异常。第四步
再解释这一区分。先认识这些数据传输时采用的 JSON 格式。

## 6. JSON 文本先解析，才能按字典取值

JSON 是一种传输数据的文本格式，Python 字典则是运行时对象。两者外观相似，
但不能混用。下面程序把文本解析成字典，再读取其中的数据：

```python
import json

raw = '{"ok": true, "data": {"document_id": "D1", "body": "日期正文"}}'
payload = json.loads(raw)
print(payload["data"]["document_id"])
print(payload["ok"] is True)
```

`import json` 使用 Python 自带的 JSON 模块。`json.loads` 将文本解析成对象，输出
分别是 `D1` 和 `True`；`json.dumps` 则反向转成文本。JSON 中的 `true/false/null`
分别对应 Python 的 `True/False/None`，带引号的 `"null"` 是字符串，不是空值。

`is True` 检查是不是明确的布尔真值，字符串 `"true"` 不符合。业务参数也需要检查：
JSON `{"document_id":42}` 完全能解析，但资料 ID 约定为字符串，所以不应执行读取。
`isinstance(value, str)` 可以检查是不是字符串；“数据能解析”和“参数能使用”是两层判断。

到这里，你已经认识下一步的消息所需要的数据类型。用下面三个短题把它们连起来。

## 7. 三个自查题，答案就在后面

先写预测，再看答案。桌面用户把预测写在“预测与作答”；这些短函数可以在普通
Python 中核对，桌面“代码实验”只运行第三步开始的完整 `run(...)` 接口。

### A. 状态是否被保留

```python
def remember(state, document_id):
    state["seen"].append(document_id)
    return len(state["seen"])


state = {"seen": []}
first = remember(state, "D1")
second = remember(state, "D2")
```

写出 `first`、`second` 和最终 `state`。如果在函数第一行加上
`state = {"seen": []}`，外部 `state` 和两次返回又是什么？

### B. 错误与控制流

```python
def read_local(document_id):
    if document_id == "D2":
        raise TimeoutError("temporary")
    return "日期已确认"


events = []
for document_id in ["D1", "D2", "D3"]:
    try:
        text = read_local(document_id)
    except TimeoutError:
        events.append((document_id, "retry_later"))
        continue
    events.append((document_id, text))
```

`raise` 主动抛出异常，`TimeoutError` 表示超时。写出 `events`；把 `continue`
换成 `break` 后有什么变化？如果捕获后不记录，调用者会缺少什么信息？

### C. 从描述到接口

```json
{"name": "read_document", "arguments": {"document_id": "D1"}}
```

假设上述文本已解析为字典 `call`，写出取 ID 的表达式。如果 ID 变成整数 42，
为什么键存在仍然不足以允许调用？这是简化结构，第二步再给它加上真实消息外壳。

### 自查答案与一次变化

A：两次返回 1、2，最后 `state` 是 `{"seen":["D1","D2"]}`。函数修改同一份列表。
加入局部重新赋值后，两次各返回 1，外部仍为 `{"seen":[]}`，因为函数改用了新字典。

B：结果为 `[('D1','日期已确认'), ('D2','retry_later'), ('D3','日期已确认')]`；
换成 `break` 后只有前两项。不记录错误，就无法区分“没有资料”和“没有取得资料”。

C：`call["arguments"]["document_id"]`。整数 42 不符合字符串参数约定，即使键存在
也要拒绝。真实接口还检查缺字段、多余字段和空字符串，第二步会说明这些检查放在哪里。

再把 A 的调用改为 D2、D2：会得到两个重复 D2，列表不会自动去重。业务需要去重时
可在追加前判断是否已存在；实际工具执行记录仍须保留每次尝试，不能删除重试证据。
如果答错，记录“哪一步把对象或控制流看错了”，回看对应小节，再手算一次。

本步把资料表示、函数返回、历史保留和错误分支连了起来。[第二步](02-observe-loop.md)
将把“固定读取 D1/D2”换成“由模型提出调用”，你会看到一段完整资料研究怎样发生。
