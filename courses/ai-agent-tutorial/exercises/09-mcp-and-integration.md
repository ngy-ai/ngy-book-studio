# 第九章桌面练习：完成一次有状态的协议会话

这次不实现真实 MCP 服务，而是在宿主的进程内模拟器中发送消息，观察初始化、能力发现、
读取与版本拒绝。你要根据返回消息推进状态，不能因为工具调用没有抛异常就认定协议业务成功。
整个练习不打开端口、不启动 MCP 子进程、不使用网络或凭据，不构成互操作验证。

## 从本章代码入口开始

点击第九章“本章代码实验”，编辑本章五参 `run`。起始代码用 `load_case` 取得本轮输入，
`call_tool` 发送宿主动作，`finish` 包装最终结果。输入在 `state["case"]["input"]`：

| 字段 | 含义 |
| --- | --- |
| `protocol_version` | 这个教学客户端支持的协议版本，当前为 `2025-11-25` |
| `document_id` | 本轮允许读取的动态资料 ID |
| `preflight` | 是否先故意发一次尚未初始化的 tools/list，观察协议错误 |

协议版本来自输入，不要把服务端程序版本 `1.0.0` 当成协议日期。
本轮客户端只支持输入中的一个协议版本；服务端回传别的版本时必须停止后续发现和调用。

## 两层消息不要混淆

本章唯一宿主工具为 `exchange`，参数是一个消息对象：

```python
payload = call_tool(state, tools, "exchange", {"message": message})
response = payload["data"]
```

`payload.ok` 表示宿主完成了消息交换；内部 `response` 才是协议响应，可能含 `result`、
`error`，也可能是通知不返回内容的 `None`。外层成功不等于内部业务成功。

外层工具 ID 由 `call_tool` 使用本轮 case_id 和派发序号生成。
内部 JSON-RPC 的 `id` 是请求关联号，通知不带这个字段。两种编号有不同职责。
本练习固定内部编号 0、1、2、3 以便核对轨迹，不表示 MCP 协议只允许这些编号。

## 按这个顺序推进会话

若 `preflight=True`，先发送 `id=0 / method="tools/list" / params={}`。
它发生在初始化前，应得到内部 `error.message="not_initialized"`；保存该错误文字，
然后继续正常初始化。这是刻意安排的诊断动作，不是遇到任意错误都继续。

初始化消息为：

```python
message = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": data["protocol_version"],
        "capabilities": {},
        "clientInfo": {"name": "moye-practice", "version": "1.0.0"},
    },
}
```

从返回的 `result.protocolVersion` 读取服务端版本。如果与支持版本不同，立即形成
`unsupported_protocol_version` 结果；不要发送 initialized，更不能先读取资料再报告不兼容。
如果版本相同，再依次发送：

| 内部方法 | `id` | `params` |
| --- | --- | --- |
| `notifications/initialized` | 不提供 | `{}` |
| `tools/list` | 2 | `{}` |
| `tools/call` | 3 | `{"name": "read_document", "arguments": {"document_id": 本轮ID}}` |

三条都带 `jsonrpc="2.0"`。通知返回 `None`；工具列表在 `result.tools` 中，每项有 `name`；
本模拟器的读取事实在 `result.facts` 中。这个简化结果不等于完整 MCP 的 content/
structuredContent 契约；正文已解释真实工具结果的结构，不要把本模拟器直接发布为 MCP 服务。

## 输出、手搓与库对照

用 `finish(state, result)` 返回以下五个字段：

```json
{
  "status": "ready",
  "version": "实际协商返回的版本",
  "tools": ["read_document"],
  "facts": {"date": "实际读到的日期"},
  "preflight_error": null
}
```

版本不兼容时 status 为 `unsupported_protocol_version`、tools 为空、facts 为 `None`。
version 仍记录实际服务端回复。没有前置诊断时 preflight_error 为 `None`；诊断时记录
`not_initialized`。事实必须来自本轮调用，不能根据资料 ID 猜值。

手搓版按顺序组消息、检查返回、决定是否继续。库版用 LangGraph 分开准备输入与执行会话，
在执行阶段保留相同的版本门槛。图运行结束不等于会话 ready，更不等于完成了完整 MCP 握手规范。

| 场景 | 业务结果 | exchange 次数 |
| --- | --- | --- |
| 正常任务 `normal` | ready，取得工具名与资料事实 | 4 |
| 故障练习 `fault` | 先观察 not_initialized，再成功初始化和读取 | 5 |
| 迁移挑战 `transfer` | 服务端版本不受支持，停止在初始化响应后 | 1 |

## 先预测再运行

在“预测与作答”写出：两层 ID 各用于什么；哪个消息没有响应；版本不兼容后还有哪些
动作不应发生。保存首次实现后跑基础与故障场景，再用迁移场景检查提前停止。

H1：把阶段画成 new → initializing → ready。H2：每收到一份响应，先识别外层交换结果
与内层协议结果。H3：只看参考的一条消息构造，独立写出版本拒绝与结果汇总。

自查：通知没有内部请求 ID，也不等待协议响应；不兼容后不能发送就绪通知。
正确报告版本拒绝可以通过作业检查。完整示范记 S；真实 stdio/HTTP、认证、授权与互操作
仍需要独立工程与测试，不能从本次通过推断已经完成。
