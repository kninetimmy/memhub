# Next Steps

1. Finish Milestone 3 markdown sync by tightening managed-block content, backup semantics, and any missing edge-case coverage.
2. Implement Milestone 3 MCP read/write tools as thin adapters over the existing indexed search and explicit write paths.
3. Decide whether Milestone 2 search should index more source types before MCP depends on it.

## GitHub

If GitHub repository creation fails or is skipped, create and connect it manually:

```bash
git init
gh repo create memhub --source . --remote origin --public
git push -u origin main
```
