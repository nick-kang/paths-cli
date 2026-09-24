# paths-cli

Get a local checkout of a dependency's source repository for an AI agent to inspect. Supports npm and pnpm projects.

## Why use paths?

Coding agents need context to use dependencies correctly. Documentation and web snippets can leave questions about edge cases, error handling, or how APIs work together. Installed packages may contain compiled output and omit the tests and examples that demonstrate intended usage. Local source lets an agent search across files, trace behavior, and check assumptions against the library's own code.

`paths` makes that source available on demand, with best-effort matching to the dependency version your project uses. For example, an agent investigating an unexpected validation error can run `paths zod`, then inspect the relevant implementation and tests. It gets a reusable local reference without you having to find the repository, choose a revision, and manage the checkout yourself.

## Install

```sh
npm i -g paths-cli
```

Requires Node.js 20+, Git 2.25+, and npm or pnpm on `PATH`. Supports macOS 11+ (Intel/Apple Silicon), Linux with glibc 2.35+ (x64/ARM64), and Windows x64. Yarn, Alpine/musl, and Windows ARM64 are not supported.

## Usage

Run inside your project or one of its subdirectories:

```sh
paths react
paths react zod
paths @tanstack/react-query
paths react@19.0.0
```

By default, `paths` uses the installed dependency's version and prints an absolute path to its source repository. You can request an exact `name@version` without installing that package first. For monorepos, the returned path is the repository root.

```sh
paths --filter web react                 # Select an exact workspace name
paths --filter ./apps/web react          # Or a path relative to the workspace root
paths --package-manager npm react        # Override package-manager detection
paths --cache-dir .agent-repos react     # Choose where checkouts are stored
paths react --json                      # Include version and revision information
paths --help
```

`--filter` (`-F`) must match exactly one workspace; patterns are not supported. If the dependency is missing there, other workspaces are searched with a warning.

Source matching is best effort. `paths` prefers the published commit, then a version tag. If neither is available, it tries a historical commit or the current default branch and warns that the source may differ from the package version.

Checkouts are reused from your system's cache directory unless you set `--cache-dir`. Treat them as reference material: modified or incomplete checkouts are rejected rather than overwritten. Move an affected checkout aside and retry. The cache is shared across projects for the same OS user and has a fixed 5 GiB soft limit (also applied to each custom `--cache-dir`). After an invocation creates a checkout, `paths` removes least recently used checkouts until under the limit. Cache-only lookups update usage timestamps without scanning sizes. Modified or busy checkouts and all results from the current invocation are preserved, so the cache can exceed the limit. Older checkouts without usage metadata are removed first.

Cache locks protect `paths` operations, but cannot detect editors or agents reading previously returned paths; a later invocation may evict those checkouts. Size is measured from filesystem entry lengths, including `.git`, rather than allocated disk blocks. Cleanup failures produce warnings without failing source lookups.

Checkouts omit common binary files, source maps, Git LFS content, and submodules, so some fixtures or examples may be incomplete.

## Using with AI agents

Add this guidance to your project's `AGENTS.md`:

```md
When dependency behavior is unclear, run `paths <package> --json`. Inspect the returned repository's implementation, tests, and examples. Read LLMS.md if present.

Use the checkout for reference only. Keep changes in this project and continue importing installed packages. Check the reported resolution method before assuming the source exactly matches the installed version.
```

If your agent can only read files inside the workspace, use `paths react --json --cache-dir .agent-repos` and add `.agent-repos/` to your project's `.gitignore`.

## Output

Normal output contains one absolute path per successful request, in input order. Warnings and errors go to stderr. A failed request does not prevent processing the remaining packages.

`--json` outputs one JSON record per request, with either a `result` or an `error`:

```json
{"request":"react","result":{"name":"react","version":"19.0.0","path":"/cache/.../commit","repository":"https://github.com/facebook/react","commit":"...","method":"published"}}
{"request":"missing","error":"installed dependency `missing` not found"}
```

The `method` is `published`, `tag`, `publication-time`, or `default-branch`; the last two are approximations. Errors before package processing, such as failure to detect the project, only appear on stderr.

Exit status is `0` on success, `1` if any request fails, or `2` for invalid CLI arguments.
