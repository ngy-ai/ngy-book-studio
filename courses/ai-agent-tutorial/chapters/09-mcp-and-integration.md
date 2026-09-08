# 第九章：MCP——把工具连接变成有契约的消息交换

前面各章直接调用 Python 函数。如果资料检索器换成另一个团队维护的程序，Agent 应该怎样发现它有哪些工具、传参数、接结果？为每个工具重复设计一套格式，会把大量精力花在连接细节上。MCP（Model Context Protocol，模型上下文协议）为这类连接规定了通用的消息和能力协商方式。

本章固定解释 **MCP 2025-11-25** 这一版规范，不把网页当前版本等同于所有客户端支持的版本。配套程序 `courses/agent-foundations/tutorial_examples/ch09_protocol.py` 只模拟其中很小的消息子集，既不是完整 MCP server，也没有通过任何 MCP 互操作验证。它不开子进程、不监听端口、不读取凭据。运行：

```powershell
uv run --locked python -m tutorial_examples.ch09_protocol
```

命令在 `courses/agent-foundations` 执行。桌面点击“本章代码实验”，按“讲义”完成本章的本地协议消息任务，编辑五参 `run`，不要粘贴无参演示入口。两种练习都不会连接真实 MCP 服务或你的图书库；通过消息用例不等于完成传输层或互操作验证。

桌面任务的输入、工具和交卷规则见[本章练习讲义](../exercises/09-mcp-and-integration.md)。

## 1. 先分清三个参与者和两层问题

**宿主 Host** 是面向用户的应用，决定本次会话的权限和使用方式。**客户端 Client** 是宿主中管理某条 MCP 连接的部分，负责协商、发送请求和接收响应。**服务端 Server** 暴露工具、资源等能力。客户端不一定是大模型，服务端也不一定运行大模型；模型可以只负责建议调用什么，宿主负责决定是否真正调用。

把通信分成两层会容易很多。消息层回答“这段数据表示调用哪个工具、请求编号是多少”；传输层回答“字节经由管道还是 HTTP 到达对方”。协议不能代替工具内部业务，更不会自动保证资料可信、运行代码安全或用户已批准某个动作。

本章主要关注 tools，也就是可调用的能力。MCP 还区分可读取的 resources 和可提供给客户端使用的 prompts 等能力；它们不必被每个服务同时实现。服务声明 `tools`，只表示支持工具相关协议，不表示当前用户可以读取每一份资料。

## 2. JSON-RPC：给请求和响应配对

MCP 使用 JSON-RPC 2.0 消息。JSON 是数据表示方式，RPC 表示远程过程调用；即使本章都在同一进程，也能先学习相同消息形状：

```json
{"jsonrpc":"2.0","id":7,"method":"tools/call",
 "params":{"name":"read_document","arguments":{"document_id":"D1"}}}
```

`jsonrpc` 指消息格式版本；`id` 是本次请求的关联编号；`method` 是协议方法；`params` 是方法参数。工具名放在 `params.name`，不要误写成 `method="read_document"`。多个请求可能交错返回，客户端靠 `id` 找到对应的等待者，不能靠“刚才发的最后一个请求”猜响应归属。

成功响应包含相同 `id` 和 `result`，协议错误包含相同 `id` 和 `error`，二者不能同时出现。**通知 Notification** 不带 `id`，不等待响应；后面的 `notifications/initialized` 就属于这一类。若编号本身非法，错误响应不能继续照抄那个非法对象作为编号。配套程序将这种错误响应的 `id` 设为 JSON `null`。消息结构可核对 [JSON-RPC 官方规范](https://www.jsonrpc.org/specification)。

不要把 JSON-RPC `id` 当作幂等键：它用于匹配请求和响应，不自动保证一次业务动作只执行一次。保存草稿之类的工具仍需第七章的业务动作标识、审批绑定和去重机制。

## 3. 初始化后才能发现和调用能力

一次正常连接先走下面的顺序：

```text
客户端 → initialize（我支持哪个版本、有哪些能力、客户端信息）
服务端 → 初始化结果（使用哪个版本、服务端能力与信息）
客户端 → notifications/initialized（通知：初始化完成）
客户端 → tools/list
服务端 → 工具名称、描述、inputSchema 等
客户端 → tools/call
服务端 → 工具结果或错误
```

**版本协商**不是简单比较双方软件版本号。客户端提出自己支持的协议版本；服务端支持它时返回同一版本，否则返回自己支持的另一版本。客户端若不支持服务端返回的版本，应停止连接，不能假装字段差不多就继续。客户端程序版本 `1.0.0` 与协议版本 `2025-11-25` 是两件事。[MCP 生命周期规范](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)给出了这一顺序和协商规则。

下面是完整的本地状态演算。它只处理演示中用到的消息形状，便于看见初始化状态；输入检查更完整的教学子集在配套模块里。

```python
import json

VERSION = "2025-11-25"
phase = "new"
tool = {"name": "read_document", "description": "读取虚构松果资料",
        "inputSchema": {"type": "object",
                        "properties": {"document_id": {"type": "string"}},
                        "required": ["document_id"], "additionalProperties": False}}

def receive(message):
    global phase
    method = message["method"]
    if method == "notifications/initialized":
        if phase == "initializing":
            phase = "ready"
        return None
    base = {"jsonrpc": "2.0", "id": message["id"]}
    if method == "initialize" and phase == "new":
        phase = "initializing"
        return {**base, "result": {"protocolVersion": VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "local-demo", "version": "1.0.0"}}}
    if phase != "ready":
        return {**base, "error": {"code": -32600, "message": "尚未初始化"}}
    if method == "tools/list":
        return {**base, "result": {"tools": [tool]}}
    if method != "tools/call":
        return {**base, "error": {"code": -32601, "message": "未知协议方法"}}
    params = message["params"]
    if params["name"] != "read_document":
        return {**base, "error": {"code": -32602, "message": "未知工具"}}
    sid = params["arguments"]["document_id"]
    if sid != "D1":
        result = {"content": [{"type": "text", "text": "scope_denied"}], "isError": True}
    else:
        data = {"source_id": "D1", "facts": {"start_date": "2026-10-12"}}
        result = {"content": [{"type": "text", "text": json.dumps(data)}],
                  "structuredContent": data, "isError": False}
    return {**base, "result": result}

messages = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": VERSION, "capabilities": {},
        "clientInfo": {"name": "lesson", "version": "1.0.0"}}},
    {"jsonrpc": "2.0", "method": "notifications/initialized"},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "read_document", "arguments": {"document_id": "D1"}}},
]
for message in messages:
    response = receive(json.loads(json.dumps(message)))
    if response is not None:
        print(json.dumps(response, ensure_ascii=False))
```

你会看到三份响应：初始化结果、工具列表、D1 正文；通知没有响应。这里经过 JSON 序列化和解析，说明双方交换的是数据，不是共享 Python 对象。它没有传输层、完整校验和连接关闭流程，不能用来声称自己实现了一个可供其它应用连接的 MCP 服务。

`global phase` 让函数更新外面的连接阶段变量；配套模块把阶段收进每个 `LocalPeer` 实例，避免不同连接共享一个全局阶段。`{**base, "result": ...}` 把 `base` 的字段复制到新字典，再加入结果；`**` 在这里是字典展开，不是乘方计算。

## 4. 工具参数合法，不等于工具执行成功

`inputSchema` 用 JSON Schema 描述输入形状。例子要求一个名为 `document_id` 的字符串，且不接受多余字段。Schema 是契约说明；服务端仍要实际校验，不能只把它显示给模型。配套程序手写校验这一个小模式，没有声称实现任意 JSON Schema 验证器。

有两类错误需要区分。未知协议方法、未知工具或错误的请求外壳可以用 JSON-RPC `error` 返回。已进入工具执行语义，但参数业务含义不合法、来源缺失或权限不足，则可以用工具结果中的 `isError: true` 表达。只看到响应里有 `result`，就判定“工具成功”，会把业务失败当成证据。

工具可以返回 `content` 文本块，也可以返回 `structuredContent` 对象。配套程序同时返回 JSON 文本及结构化内容，便于展示和程序处理。若公布 `outputSchema`，还需要保证并验证结构化结果符合它。[官方 Tools 规范](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)说明了这些字段及错误边界。

运行完整模块时，正常读取 D1 的 `isError` 是 `False`；请求 `OTHER_PROJECT` 的结果是 `scope_denied`；未初始化先列工具得到错误码 `-32600`；只支持虚构未来版本的客户端会拒绝继续。这里的错误文字是教学实现选择，不是所有 MCP 服务都必须使用的错误文本。

## 5. 接到 LangGraph，以及真正传输时新增的责任

框架版把步骤连成 `initialize → discover → read`。每个节点调用相同本地客户端，将结果写入状态；它没有安装 MCP SDK，也没有把 LangGraph 当作 MCP 实现。下面可以接在第三节代码后执行，同样走本地消息函数：

```python
from typing import TypedDict
from langgraph.graph import START, END, StateGraph
from langsmith import tracing_context

class State(TypedDict):
    response: dict

def read_node(state):
    response = receive(messages[-1])  # 前面的演算已完成初始化
    if "error" in response or response["result"].get("isError", False):
        raise ValueError("工具未成功，不能拿失败消息当研究证据")
    return {"response": response["result"]["structuredContent"]}

graph = StateGraph(State)
graph.add_node("read", read_node)
graph.add_edge(START, "read")
graph.add_edge("read", END)
with tracing_context(enabled=False):
    print(graph.compile().invoke({"response": {}}))
```

**stdio** 传输通常由客户端启动子进程，经标准输入输出交换消息，每条消息占一行；日志应走标准错误，不能把“服务已启动”随手打印进协议输出。**Streamable HTTP** 通过 HTTP 请求交换消息，并可使用 SSE 流式传递；SSE 指服务器持续发送事件的一种 HTTP 机制。它不等同于旧版本单独的 HTTP+SSE 传输。实现 HTTP 时还需处理协商后的协议版本请求头、会话、Origin 校验和连接生命周期，不能只把 `receive` 套上一条 POST 路由就宣布完成。[官方传输规范](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)

网络断开不代表业务动作被取消，超时也不代表对方没有执行。需要关联编号、超时策略、取消通知，以及有副作用工具的幂等性。把失败暴露给 Agent 之前，还要限制返回体大小、校验内容形状，并避免把服务端返回的指令误当成宿主权限。

## 6. 认证、授权和本轮范围要分别检查

认证回答“你是谁”，授权回答“你能做什么”。OAuth 访问令牌可以表达授予客户端的访问范围，但连上某个 MCP 服务，并不意味着本轮允许读取任意图书 ID。服务端仍要在工具执行边界落实具体对象权限，宿主也要限制本次任务范围；模型传来的参数只能在授权范围内选择。

MCP 的 HTTP 授权规范定义了一套令牌和授权流程，stdio 不直接照搬它。本章不实现两者，也不读取环境变量或任何真实凭据。会话编号、工具发现结果和工具标注都不能代替权限核验。正式接入时应使用适合目标协议版本的成熟 SDK，并单独验证传输、授权和工具业务，而不是将本章的消息模拟直接发布。[官方授权规范](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)

**练习：** 删除初始化通知后调用工具，预计在哪一步失败？把工具名改成不存在的名字，与把资料 ID 改成无权访问的值，响应有何不同？服务端返回相同 `id` 但同时含 `result` 和 `error`，客户端能接受吗？先写预测，再运行本地变式。

**提示与自查答案：** H1 看连接阶段，H2 看错误在协议外壳还是工具结果里，H3 对照 `phase` 和 `isError`。删除通知会停在未就绪阶段；未知工具走协议错误，无权来源走工具失败；同时包含结果和错误的响应不合规，不能取一个顺眼的字段继续。完成后再解释：接入协议不会消除第六章的评测、第七章的审批和第八章的宿主证据记录。

下一章：[综合工程与研究训练](10-capstone-and-research.md)。API 核对日期：2026-09-07。LangGraph 版本依现有 `uv.lock`，本章没有新增 MCP 依赖。
