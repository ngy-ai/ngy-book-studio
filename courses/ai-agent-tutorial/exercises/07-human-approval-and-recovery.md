# 第七章桌面练习：批准必须绑定当前提案

你收到一份草稿提案和一份批准记录。只有批准仍有效、内容仍相同、任务未取消，
才允许产生一次虚构草稿写入。重复通知不能造成重复效果。
这里的写入发生在宿主的内存模拟中，不是磁盘、真实业务系统或真实用户审批。

## 编辑入口与本次范围

从第七章“本章代码实验”进入，编辑本章五参 `run`。起始代码用 `load_case` 取得输入，
通过 `call_tool` 执行动作，最后 `finish(state, result)` 交卷。
正文另有 `InMemorySaver / interrupt / Command` 的暂停恢复示范；本桌面任务集中检查
恢复之后的批准门槛和单次提交，不声称测到了跨进程检查点或真实审批对话框。

输入为：

| 字段 | 含义 |
| --- | --- |
| `proposal` | 当前待保存对象，含 `draft_id / revision / body` |
| `approval` | 批准记录，含 `digest / decision / expires_at` |
| `now` | 当前教学逻辑时间 |
| `cancelled` | 当前任务是否已经取消 |
| `deliveries` | 同一逻辑提案的通知投递；重复通知不是不同草稿 |

它们均由本轮宿主生成。草稿 ID 和正文不能写死；也不能把 approval 的摘要原样拿来
充当“重新计算过当前内容”的证明。

## 先重算摘要，再按明确顺序判定

本练习规定摘要算法，双方必须完全一致：

```python
import hashlib
import json

encoded = json.dumps(proposal, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
current_digest = hashlib.sha256(encoded.encode("utf-8")).hexdigest()
```

`sort_keys=True` 固定键顺序，`separators` 去掉键和值之外的格式空格，UTF-8 固定字节编码。
正文某个短示范使用的默认 JSON 空格不能直接套进这个摘要契约；内容相同但序列化不同，摘要也会不同。

按以下优先顺序决定业务状态：

1. `cancelled` 为真：`cancelled`。
2. `approval.decision` 不是 `approve`：`rejected`。
3. `approval.expires_at <= now`：`expired`。
4. 批准摘要不等于当前提案摘要：`proposal_changed`。
5. 全部通过，才允许保存；取得回执后为 `completed`。

顺序使同时发生多个不利条件时仍有确定结果。到期时刻已失效；revision 或 body 改变后
都要重新批准，不能仅比较 draft_id。摘要绑定内容，但不认证真实用户身份。

## 保存一次，交回真实回执

允许保存时执行一次：

```python
payload = call_tool(state, tools, "save_draft", {
    "proposal": proposal,
    "digest": current_digest,
})
```

成功数据含 `receipt`，它由宿主生成，不能预测或自行拼接。`deliveries` 中同一通知出现
三次，也只能对应这一次逻辑提交；不要循环调用三次 save_draft。宿主会拒绝第二次副作用。
本任务没有“成功后再调用工具查询回执”的接口，原回执保留在本次状态中即可。

结果形状固定为：

```json
{"status": "completed", "writes": 1, "receipt": "工具实际返回的回执"}
```

不允许保存时，status 使用对应拒绝原因，`writes=0`、`receipt=None`，不调用保存工具。
不能只是把 writes 写成零，同时已经让宿主产生效果；宿主会核对实际提交次数与决定。

## 手搓、库实现与场景

手搓版先完成纯判断，再进入唯一保存分支。库版用 LangGraph 的准备节点产生决定，
执行节点依据决定保存或直接形成拒绝结果。不要在准备节点中提前调用保存，
也不要因整个图正常结束就默认业务成功。

| 场景 | 本轮可能出现的条件 | 实际保存次数 |
| --- | --- | --- |
| 正常任务 `normal` | 明确批准或明确拒绝；据此得到 completed 或 rejected | 1 或 0 |
| 故障练习 `fault` | 批准已经到期，或当前提案改变；需区分 expired 与 proposal_changed | 0 |
| 迁移挑战 `transfer` | 完整五个分支均可能出现；取消还可能与批准过期同时发生 | 只有 completed 时为 1 |

同一场景多次运行可能给出不同条件。不能把 normal 直接映射为 completed，或把 transfer
固定写成 proposal_changed；每次都要按输入重算。前后两版若抽到不同变式，比较规则与
实际行为是否相符，不要求回执或最终业务状态完全一致。
这些公开场景不穷尽审批问题；通过后仍需解释判断顺序和摘要绑定。
本例只能证明这次任务的提交门禁，不证明磁盘事务、跨系统恰好一次或取消的所有竞争窗口。

## 独立尝试与自查

先写预测：摘要应计算当前提案还是批准记录？三次通知为何不是三份草稿？到期边界用大于
还是大于等于？保存首次判断，再运行手搓版和库版。

H1：列出“当前提案、展示过的提案摘要、批准、执行回执”。H2：画出判断之后只有一个
分支能到保存。H3：只看参考中的摘要规范化或一个拒绝条件，自己补全其余分支。

自查：重算当前提案；重复通知共享同一个逻辑草稿；到期即不能保存。
在迁移场景不看参考说明“为什么不是沿用旧批准”，并如实记录提示。完整示范记 S。
