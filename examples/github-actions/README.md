# Example: GitHub Actions with act

```bash
cd examples/github-actions
gridstator actions list
gridstator actions run ci.yml                       # the whole workflow (test, then lint)
gridstator actions run ci.yml --job test            # one job
gridstator actions run ci.yml -e workflow_dispatch -i greeting=Gridstator
gridstator artifacts <job-id> --download test-report/test-report.zip
```

Edit `src/greet.js` without committing and run again: the workflow runs your working tree, because
`actions/checkout` of the current repository uses the snapshot instead of cloning from GitHub.
