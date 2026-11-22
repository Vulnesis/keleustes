---
name: Code-Reviewer
description: Reviews code for bugs
tools: Read, Grep, Bash, mcp__x__y, Task(security-auditor)
model: sonnet
color: blue
permissionMode: acceptEdits
hooks:
  PreToolUse: []
mcpServers:
  github: {}
a2a:
  peers: [security-auditor]
allow_agents: [planner]
subagents: [helper]
schedule: "0 * * * *"
---
You review code. Be precise.
