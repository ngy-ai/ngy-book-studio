# 第一章：亲手构建并修好一个资料研究 Agent

讲义修订：`2026-09-07.tutorial-1`（连续教程版）；实验与评分契约仍为 `1.0.0`。

这一章面向有基础编程能力、没有 Agent 经验的学习者。你将根据虚构资料回答
“松果项目”的试运行日期、面向对象和允许操作，并给每项结论附上正文来源。
先用普通 Python 组织工具调用循环，再用 LangGraph 重建，最后处理故障与新资料。

正文从具体问题开始，逐步解释数据结构、模型消息、工具请求、循环和状态；完整
正常版、有限重试版与框架版都在讲义中，可以直接阅读和运行。练习附就地答案，
不需要先读参考书、打开参考解答或理解一组未解释的辅助函数才能继续。

## 本章在完整教程中的位置

[完整 AI Agent 教程](../ai-agent-tutorial/README.md) 从本章继续展开模型接口、检索、
记忆、规划与工程实践。桌面“学习 AI Agent”默认打开“AI Agent 教程总览”，左侧
“完整教程 · 十章”可以进入各章；“第1章 · 工具调用循环”打开本章导读。

第一章仍有独立的六步练习、手搓与 LangGraph 编辑区、固定场景检查和学习记录。
后续章节提供连续正文、本地示例和独立桌面实验；在章节正文点击“本章代码实验”
即可练习，代码、预测、历史和备份按章保存。能阅读后续章节不表示第一章评测已通过，
也不自动登记掌握。按自己的进度阅读，不需要先取得专家认证。

## 先知道自己会做出什么

搜索只能得到资料 ID 与标题，读取才得到正文。你的程序需要在模型和工具之间
正确传递消息：模型提出搜索请求，程序执行并回填；模型提出读取请求，再执行和
回填；模型依据正文给出答案，程序准确结束。不存在或读不到的资料不能当作来源。

六步围绕同一任务连续展开，前一节的代码和解释会在后一节继续使用：

1. [从松果项目的问题开始](lessons/01-prerequisites.md)：为什么需要取资料；用 Python
   字典、列表、函数和异常保存资料与结果；基础自查及答案。
2. [看懂完整工具循环](lessons/02-observe-loop.md)：模型、宿主、工具分别做什么；
   逐条展开真实消息；运行一次完整离线演示。
3. [把循环写成自己的 run 函数](lessons/03-build-manual.md)：解释五个输入和返回值，
   讲清初始化与答案解析，给出可运行的完整正常版。
4. [资料没有按预期返回时怎样继续](lessons/04-recover-failures.md)：非法请求、暂时失败、
   永久缺失与预算停止；给出完整有限重试版。
5. [用 LangGraph 重新组织 Agent](lessons/05-build-langgraph.md)：先运行最小图，再将
   同一循环改为状态、节点与条件边；完整实现与配对检查。
6. [换资料检查方法](lessons/06-transfer-and-review.md)：公开迁移、错误诊断、就地自查、
   延迟复测与进入后续教程。

完整示范是教材的一部分。可以先跟着运行，再关闭示范重写关键段落；看过示范
完成与独立完成分别记录，不用为了保留“独立”标签而卡在缺少解释的地方。

## 在墨页中阅读与练习

从“AI Agent 教程总览”或本章导读点击“开始第一章”；已有学习位置时点击
“继续第一章”。左侧“第一章 · 六步练习”保留六步入口。学习时可以连续阅读正文，
遇到例子先顺着数据与控制流演算，再到作答区记录自己的判断。

| 要做的事 | 使用位置 |
| --- | --- |
| 阅读本章解释与完整示范 | 中间“讲义”；也可通过章节和步骤入口连续阅读 |
| 写预测、答案、修改理由 | “预测与作答”，保留首次判断，修改时追加 |
| 运行第三步正常版、第四步修复版 | “代码实验 → Python 手搓”，使用完整 `run(...)` 文件 |
| 运行第五步框架版 | “代码实验 → LangGraph” |
| 查看实际调用与保存证据 | 右栏“结果”“逐步轨迹”“本次作答”“历史” |

讲义里的短 Python 示例分为独立程序与明确标注的续接片段。独立程序可在普通课程
Python 环境运行；桌面编辑区要求 `run(task, model, tools, limits, emit)`，不是
任意短代码控制台。只使用桌面时，可阅读短例子的完整输出，第三步开始直接运行
正文给出的 Agent 程序。桌面尚不自动评阅文字解释，按就地答案自查即可继续。

桌面使用固定响应模型与一次性 Windows 隔离进程，支持取消；不需要真实 API 密钥，
不读取个人图书库。源码、作答和报告保存在独立学习档案，保存后可导出备份、恢复
或开始新一轮。CLI 工作区与桌面档案分开，不会自动同步。

## 首次准备课程环境

运行实验需要 Windows、Python 3.12 和 uv。uv 是管理 Python 环境与依赖的工具，
安装说明可查[官方安装页](https://docs.astral.sh/uv/getting-started/installation/)。
只阅读教程不要求先完成安装；准备动手时，在本目录执行：

```powershell
Set-Location E:\projects\epub-reader\courses\agent-foundations
uv sync --locked
```

`--locked` 使用课程已固定的依赖版本，包括 LangGraph 1.2.11；首次安装依赖需要
联网，之后固定响应实验本身离线运行。桌面学习不必创建 CLI 工作区，准备好后可
在界面重新检测环境。不会自动启动模型服务或下载模型。

## 可选：在命令行运行

命令行用户先在课程目录创建自己的工作区：

```powershell
uv run --locked python -m moye_lab new-workspace first-agent
```

`manual.py`、`langgraph_agent.py` 是自己的实现文件，`learning-record.md` 保存
作答。把第三、四步完整手搓代码放入前者，第五步完整图代码放入后者，再运行：

```powershell
uv run --locked python -m moye_lab run --implementation workspaces/first-agent/manual.py --scenario normal
uv run --locked python -m moye_lab run --implementation workspaces/first-agent/langgraph_agent.py --scenario normal
```

对照自己的两版实现：

```powershell
uv run --locked python -m moye_lab compare --scenario normal --manual workspaces/first-agent/manual.py --langgraph workspaces/first-agent/langgraph_agent.py
```

`manual`、`langgraph` 这两个不带文件路径的名称指内置参考实现。用于检查环境时可以
运行它们，但不能把参考成功记为自己独立实现通过。场景依次为 `normal`、
`invalid_call`、`invalid_arguments`、`transient`、`missing`、`model_budget`、
`tool_budget`，再到第六步的 `transfer`。

第二步的完整观察程序可保存为 `workspaces/observe.py`；第五步最小图可保存为
`workspaces/graph_demo.py`，用 `uv run --locked python 文件路径` 运行。其余标注
为“接在上一段后”的片段需要保留前面的定义，不能把每个代码块都当成完整文件。

CLI 在当前进程执行指定 Python 文件，只用于本机可信代码；桌面隔离边界不适用于
CLI。运行报告放在 `runs/`，工作区在 `workspaces/`，均是本地忽略目录，不提交
个人作答、运行报告或凭据。真实模型是第六步的可选观察，不是本章入门条件。

## 完成本章之后

你应能解释一次请求怎样提出、执行和回填，写出有界循环，识别重试与永久错误，
并说明未知字段为什么不能编造来源。两份实现、实际报告、首次预测和修复理由
共同构成学习记录；自动检查通过只说明作品符合相应场景，不能自动认证专家。

[参考解答](worksheets/reference-answers.md)、[评分量规](worksheets/rubric.md)和
[延伸资料](references.md)可以用于复盘，正文已经包含读懂与完成本章所需的解释。
[课程规格](spec.md)面向维护者，包含完整场景设置；首次迁移前无需查它。
[试学记录表](worksheets/pilot-session.md)用于记录真实学习卡点，目前不能把自动
验证当成未参与设计的学员已经学会。

准备好后从[第一步](lessons/01-prerequisites.md)开始；完成六步后回到
[完整教程](../ai-agent-tutorial/README.md)继续下一章。
