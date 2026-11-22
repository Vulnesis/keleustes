---
description: Create a git commit
argument-hint: [message]
allowed-tools: Bash(git add:*), Bash(git commit:*)
model: haiku
---

## Context

- Status: !`git status`
- Read @CONTRIBUTING.md

Commit with message $ARGUMENTS.
