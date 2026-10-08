# Complexity reports

[llm-cc](https://github.com/pawelchcki/llm-cc) compares the code complexity of
each pull request with its merge base and ranks the repository on pushes to
`main`. Reports are advisory: increased complexity does not fail the analysis
or replace the existing build and test checks.

`buildbuddy.yaml` runs the shared Bazzite coordinator in the
`linux-amd64-kvm` pool, reserving the `bazzite-host` resource. It uses the
host's pinned scorer, model and settings, and a shared cache; GPU workers
score only uncached contents. No application build or source script runs as
part of the analysis.

`.ci-toolkit.yml` consumes the six report artifacts from the
`Complexity comparison` status published by BuildBuddy. It publishes the
full reports and upserts one bounded comment on each open pull request.
Reports are retained for 14 days, with five default-branch sets kept.
`report.md` and `report.json` contain the comparison; `baseline.md` and
`baseline.json` rank the run's head revision. The baseline from a `main`
push is the repository baseline.

## File categories

`rules.json` keeps the three published libraries (`src/`, `gzip-zero/src/`
and `zstd-zero/src/`) in the runtime category. Integration tests, test
helpers, fixtures and fuzzers are tests. The browser, extraction example,
firmware examples, benchmark harness and artifact tools are tooling.
Tests take precedence over tooling. Scores apply to entire files, so inline
Rust unit tests remain in their containing source file's category.

JavaScript modules (`.mjs`) are scored as JavaScript. Generated browser
output, dependencies and build trees are excluded. Unsupported languages
remain visible as unmeasured paths in the report.

Validate or inspect classification locally:

```console
llm-cc rules check .llm-cc/rules.json
llm-cc rules explain src/layer.rs gzip-zero/tests/invariants.rs web/app.js web/tests/browser.spec.mjs
```

## Enable reporting

The BuildBuddy and ci-toolkit GitHub apps must have access to
`pawelchcki/oci-zero`. Register the repository with the BuildBuddy group
that can use the `linux-amd64-kvm` pool and its `bazzite-host` resource.
The shared host must have `/var/lib/llm-cc/bin/llm-cc-coordinate` installed;
configure a read-only `GITHUB_TOKEN` BuildBuddy secret for GitHub discovery.

Merge the publication policy and rules onto `main` before expecting
pull-request comments: the publisher reads policy from the target commit,
and the scorer also reads rules from the target commit. The integration PR
therefore cannot publish its own comment using its new policy. Its merge
starts the first repository baseline run.

The upstream [consumer guide](https://github.com/pawelchcki/llm-cc/blob/main/tools/comparison/consumer/README.md)
documents host setup, credentials and the report artifacts.

## Local branch comparison

With llm-cc and its model/backend prepared:

```console
llm-cc compare run --repo . --target origin/main --output-dir /tmp/oci-zero-comparison
```

This compares committed `HEAD`; commit local edits before running it.
Use the same scorer, model and settings as the shared host when comparing
local scores with CI reports.
