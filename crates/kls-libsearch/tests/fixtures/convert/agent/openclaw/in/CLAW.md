---
schemaVersion: 1
agent:
  id: Release Helper
  name: Release helper
  model:
    primary: anthropic/claude-sonnet-4-5
    fallbacks: [openai/gpt-5]
  tools:
    allow: [read, grep, exec, github__list_issues, write]
    deny: [write]
    fs:
      workspaceOnly: true
  subagents:
    allowAgents: [reviewer]
    delegationMode: suggest
  memory:
    search:
      enabled: true
      sources: [memory]
packages:
  - kind: skill
    source: clawhub
    ref: "@acme/deploy"
    version: 1.2.3
mcpServers:
  github:
    command: npx
    args: ["-y", "some-server"]
    env: {TOKEN: "${GITHUB_TOKEN}"}
cronJobs:
  - id: nightly
    schedule: {cron: "0 3 * * *"}
    message: run the checks
extra: 1
---
You cut releases. Check the changelog first.
