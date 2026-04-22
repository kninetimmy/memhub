# Next Steps

1. Add a real `memhub command record|verify` flow so build/test/run history becomes useful instead of schema-only.
2. Implement Milestone 2 git ingestion plus a narrow `search` command backed by FTS and indexed lookups.
3. Implement Milestone 3 markdown managed-block sync and MCP read/write tools without bypassing the write policy.

## GitHub

If GitHub repository creation fails or is skipped, create and connect it manually:

```bash
git init
gh repo create memhub --source . --remote origin --public
git push -u origin main
```
