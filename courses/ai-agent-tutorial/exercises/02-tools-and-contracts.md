# 第二章桌面练习：实现安全的工具入口

这次先不写完整模型循环，只处理一批运行时请求：判断能否读取，拒绝非法请求，
对允许的请求实际调用宿主工具。目标是区分“工具存在”“参数正确”“本轮有权限”三层规则。
故障请求必须在你的入口被识别，不能先交给宿主执行，再把拦截异常当作已经完成判断。

## 编辑哪个函数

点击第二章正文顶部“本章代码实验”，在“代码实验”选择“Python 手搓”，编辑已有的
`run(task, model, tools, limits, emit)`。这与正文无参 `run_manual()` 是不同入口。
起始代码使用以下公共协议，`result` 的算法由你补全：

```python
from ngy_lab.chapter_support import load_case, call_tool, finish

def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    # 逐项校验 data["requests"]，只对合法请求调用工具。
    result = {"decisions": []}  # 在这里填入你的判定结果。
    return finish(state, result)
```

这个空列表起点不会通过练习。`load_case` 取得本轮动态输入，`call_tool` 维护工具消息，
`finish` 保留消息链并包装本轮 `case_id`；这些助手不提供分类或标准答案。

## 看清本轮输入

`input.allowed_ids` 是宿主授权的资料 ID 列表；`input.requests` 是待判定请求列表。
每项请求为 `{"id": 请求ID, "name": 工具名, "arguments": 参数对象}`。
资料 ID、请求 ID 与事实在运行时生成，不能写死 D1 或正文中的日期。

按请求原顺序处理，依次应用规则：

1. `name` 不是 `read_document`：拒绝，错误码 `unknown_tool`。
2. 参数不是字典，或者键集合不是恰好 `{"document_id"}`：拒绝，`invalid_arguments`。
3. `document_id` 不是非空字符串：拒绝，`invalid_arguments`。
4. 资料 ID 不在 `allowed_ids` 中：拒绝，`outside_scope`。
5. 全部通过，才执行 `read_document`。

先判结构，再判权限；多带一个 `force` 也属于参数错误，不能视为额外授权。
宿主会再次校验权限，这是独立防线。你主动派发未授权动作，练习仍失败。

唯一业务动作是：

```python
payload = call_tool(state, tools, "read_document", {"document_id": document_id})
```

成功返回 `{"ok": True, "data": {"document_id": ..., "facts": {...}}}`。
先确认 `ok`，再使用 `data`；必须真的取到正文事实，不能只填一个“读取成功”字符串。

## 交回什么

返回 `finish(state, {"decisions": decisions})`。每项判定必须有下面四个字段，按原请求顺序排列：

```json
{
  "request_id": "当前请求的id",
  "accepted": false,
  "error_code": "outside_scope",
  "data": null
}
```

接受项用 `accepted=true`、`error_code=null`、`data=工具成功返回的data`。
拒绝项用 `accepted=false`、对应错误码、`data=null`。Python 中空值写 `None`。
请求 ID 是分类结果的关联号；工具派发 ID 由 `call_tool` 按本轮和实际派发序号生成，二者不要混用。

## 两种实现与三个场景

手搓版把上述分类写清楚。桌面库实现用 Pydantic 承担参数结构校验，用 LangGraph 分开
“判定请求”和“执行合法读取”两个阶段；图状态保存判定列表，执行节点只补成功数据。
先在库版文件的 `prepare` 之前声明一个参数模型，或按相同约束自行命名：

```python
from pydantic import BaseModel, ConfigDict, Field, ValidationError

class ReadArguments(BaseModel):
    model_config = ConfigDict(strict=True, extra="forbid")
    document_id: str = Field(min_length=1)
```

这是需要放入现有文件的声明片段，单独运行不会完成作业。`strict=True` 禁止为了通过检查
而自动转换类型，`extra="forbid"` 拒绝多余键，`Field(min_length=1)` 要求非空字符串。
在 `prepare` 中对参数调用 `ReadArguments.model_validate(arguments)`，捕获
`ValidationError` 并转换为本章的 `invalid_arguments`。名称检查在它之前；权限检查在
参数合法之后，仍由你单独实现。这样可以比较哪些手写条件由参数库承担，哪些业务规则
仍留在程序里。
两版都按列表顺序保留结果，不把 `allowed_ids` 放进请求者可修改的工具参数。

| 场景 | 预期变化 |
| --- | --- |
| 正常任务 `normal` | 3 个请求：1 个合法读取、1 个未知工具、1 个越权读取；只发生 1 次实际读取 |
| 故障练习 `fault` | 增加多余键与缺少必需键的请求；仍只有 1 次合法读取 |
| 迁移挑战 `transfer` | 请求顺序打乱并增加另一份合法资料；按新顺序分类，实际读取 2 份资料 |

不要按“第几个请求”决定错误码。宿主同时检查候选判定与实际读取顺序，返回正确表格却
没有读取，或先把全部请求派发再筛选，都不符合任务。

## 先预测，再自查

在“预测与作答”先写：一个请求同时使用未知工具和错误参数时，哪项检查先决定结果？
有 `document_id` 却又带 `force=True`，能否直接忽略多余键？保存首次尝试后运行基础场景。

H1：把一个请求拆成名称、键集合、值类型、授权四格。H2：每次拒绝后立即形成判定，
不要流入读取分支。H3：只看参考的一项校验，再独立补齐其余分支。
完整查看或复制参考应记录 `S`；独立完成且未用提示才记 H0。

自查：先拒绝未知名称；额外键应明确拒绝，不能悄悄改变请求语义。
随后在迁移场景不看参考再运行，解释为什么请求变了、权限规则却没有变。
