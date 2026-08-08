---
name: notion-database-watcher
short: Customize a Notion database watcher trigger.
description: Poll recipe for Notion database or data source changes.
when-to-use: Use when Notion changes should be polled with durable state.
---
# Notion database watcher

Keep the poll state key stable once deployed. Customize `on_database_change`
to send notifications or dispatch downstream review work.
