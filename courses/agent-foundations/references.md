# 本章参考资料

核对日期：2026-09-07。以下核对了公开入口、目录及列出的代表页面；没有执行这些
仓库代码，也没有对整套教材作质量验收。网页和 GitHub `main` 都会变化，当前未固定
上游 commit；不把下面的访问日期写成发布版本。实际实验依赖以本目录 `uv.lock`
为准，不要求安装六套课程的依赖。

这六份资料分别帮助理解概念、观察与评估、工具循环、工具设计、构建路径和恢复
模式。本章讲义与虚构实验独立编写，按遇到的问题跳转阅读，不要求顺序读完六套。

| 资料 | 已核对形态与版本说明 | 本章精确阅读位置与用途 |
| --- | --- | --- |
| [Hugging Face Agents Course](https://huggingface.co/learn/agents-course/en/unit0/introduction) | 在线课程；按 Unit 组织；未固定网页版本，2026-09-07 核对 | [Unit 1: Dummy Agent Library](https://huggingface.co/learn/agents-course/en/unit1/dummy-agent-library)：对照模型提出动作、真实函数执行与结果回填，配合第二、三步 |
| [LangChain Academy: Building Reliable Agents](https://academy.langchain.com/courses/building-reliable-agents) | 本次可见公开课程目录；未声称看完登录后的课程；未固定网页版本，2026-09-07 核对 | 同页 Module 1 的 Observation，以及 Module 2 的 Creating Datasets、Running Experiments、Code-based Eval、LLM-as-Judge、Pairwise Evaluations：配合第四、五步分离轨迹、运行判定与评阅 |
| [learn-claude-code](https://github.com/shareAI-lab/learn-claude-code) | 代码与讲解型仓库；本次根目录为 s01–s17；`main` 未固定 commit，2026-09-07 核对 | [s01_agent_loop](https://github.com/shareAI-lab/learn-claude-code/tree/main/s01_agent_loop)、[s02_tool_use](https://github.com/shareAI-lab/learn-claude-code/tree/main/s02_tool_use)：观察循环与工具分派；不要与仓库旧 docs/agents 的课号混用 |
| [Microsoft AI Agents for Beginners](https://github.com/microsoft/ai-agents-for-beginners) | 入门课程与示例仓库；当前路线涉及 Microsoft Agent Framework / Foundry V2；`main` 未固定 commit，2026-09-07 核对 | [04-tool-use](https://github.com/microsoft/ai-agents-for-beginners/tree/main/04-tool-use)：补充工具定义和调用职责；本章不安装其云端示例环境 |
| [Hello-Agents](https://github.com/datawhalechina/hello-agents) | 系统性教程、配套代码与练习；`main` 未固定 commit，2026-09-07 核对 | [第 4 章：经典范式构建](https://github.com/datawhalechina/hello-agents/blob/main/docs/chapter4/第四章%20智能体经典范式构建.md) 与 [第 6 章：框架开发实践](https://github.com/datawhalechina/hello-agents/blob/main/docs/chapter6/第六章%20框架开发实践.md)：分别对照手搓、框架和选型思考 |
| [xindoo/agentic-design-patterns](https://github.com/xindoo/agentic-design-patterns) | 《Agentic Design Patterns》中文翻译项目，含章节原文、译文与阅读站点；不是 Agent 框架；`main` 未固定 commit，2026-09-07 核对 | [第 12 章：异常处理与恢复](https://github.com/xindoo/agentic-design-patterns/blob/main/chapters/Chapter%2012_%20Exception%20Handling%20and%20Recovery.md)：对照第四步的重试、回退、明确失败与人工升级思考 |

资料中的工具、模型、框架 API 与本章可能不同。阅读时提取机制与证据，不直接复制
过时调用到锁定环境，也不把案例中的 shell、网络或写入工具增加到本章。引用资料
内容时保留来源；实际代码契约请回到本章接口和运行结果核对。
