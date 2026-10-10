# paths-cli

Get a dependency's local source directory for an AI agent to inspect. Supports npm, pnpm, and Rust Cargo projects.

## Why use paths?

Coding agents need context to use dependencies correctly. Documentation and web snippets can leave questions about edge cases, error handling, or how APIs work together. Installed packages may contain compiled output and omit the tests and examples that demonstrate intended usage. Local source lets an agent search across files, trace behavior, and check assumptions against the library's own code.

`paths` makes that source available on demand. For npm and pnpm, it checks out the upstream repository with best-effort matching to your dependency version. For Rust, it returns the source Cargo resolves for your project. For example, an agent investigating an unexpected validation error can run `paths zod` or `paths serde`, then inspect the relevant implementation.

## Install

```sh
npm i -g paths-cli
```

The npm launcher requires Node.js 20+. For npm/pnpm source lookups, install Git 2.25+ and the project's package manager on `PATH`; for Rust, install Cargo on `PATH`. Supports macOS 11+ (Intel/Apple Silicon), Linux with glibc 2.35+ (x64/ARM64), and Windows x64. Yarn, Alpine/musl, and Windows ARM64 are not supported.

## Usage

Run inside your project or one of its subdirectories:

```sh
paths react
paths react zod
paths @tanstack/react-query
paths react@19.0.0
paths serde                             # In a Rust project
paths serde@1.0.228
```

`paths` detects the nearest Node manifest (`package.json` or `pnpm-workspace.yaml`) or `Cargo.toml`, stopping at the Git boundary. If both families occur in that directory, select a manager explicitly.

For npm/pnpm, `paths` uses the installed dependency's version and prints an absolute path to its source repository. You can request an exact `name@version` without installing that package first. For monorepos, the returned path is the repository root.

For Cargo, it prints the crate directory for a registry, Git, or path dependency, or the current crate itself. An exact `name@version` must exist in the project's resolved dependency graph; `paths` does not select arbitrary registry versions. Package names and dependency aliases are accepted. When multiple versions match within a package's graph, the nearest dependency wins, then the lowest semantic version, then path.

```sh
paths --filter web react                 # Select an exact workspace name
paths --filter ./apps/web react          # Or a path relative to the workspace root
paths --package-manager npm react        # Override package-manager detection
paths --package-manager cargo serde --json
paths --cache-dir .agent-repos react     # Choose where npm/pnpm checkouts are stored
paths react --json                      # Include version and revision information
paths --help
```

`--filter` (`-F`) must match exactly one workspace; patterns are not supported. If the dependency is missing there, other workspaces are searched with a warning.

Cargo searches the current package first, or a virtual workspace's default members in path order. `--filter` accepts an exact workspace package name or workspace-root-relative path. Cargo metadata is loaded with `--locked`; missing or stale lockfiles fail with guidance to run `cargo fetch`. Cargo may download dependency sources, but `paths` leaves the lockfile unchanged. When a requested crate or version is absent, it retries with `--all-features` before searching remaining workspace members in path order.

For npm/pnpm, source matching is best effort. `paths` prefers the published commit, then a version tag. If neither is available, it tries a historical commit or the current default branch and warns that the source may differ from the package version.

Npm/pnpm checkouts are reused from your system's cache directory unless you set `--cache-dir`. Treat them as reference material: modified or incomplete checkouts are rejected rather than overwritten. Move an affected checkout aside and retry. The cache is shared across projects for the same OS user and has a fixed 5 GiB soft limit (also applied to each custom `--cache-dir`). After an invocation creates a checkout, `paths` removes least recently used checkouts until under the limit. Cache-only lookups update usage timestamps without scanning sizes. Modified or busy checkouts and all results from the current invocation are preserved, so the cache can exceed the limit. Older checkouts without usage metadata are removed first.

Cache locks protect `paths` operations, but cannot detect editors or agents reading previously returned paths; a later invocation may evict those checkouts. Size is measured from filesystem entry lengths, including `.git`, rather than allocated disk blocks. Cleanup failures produce warnings without failing source lookups.

Checkouts omit common binary files, source maps, Git LFS content, and submodules, so some fixtures or examples may be incomplete.

Cargo source directories are managed by Cargo. They bypass the paths cache, usage tracking, and eviction; `--cache-dir` applies only to npm/pnpm. Published crates may omit upstream tests and examples.

## Using with AI agents

Add this guidance to your project's `AGENTS.md`:

```md
When dependency behavior is unclear, run `paths <package> --json`. Inspect the returned source directory's implementation, tests, and examples when available. Read LLMS.md if present. In projects with both Node and Cargo manifests, pass --package-manager npm, pnpm, or cargo.

Use the source for reference only. Keep changes in this project and continue importing dependencies normally. Check the reported resolution method before assuming the source exactly matches the installed version.
```

For npm/pnpm, if your agent can only read files inside the workspace, use `paths react --json --cache-dir .agent-repos` and add `.agent-repos/` to your project's `.gitignore`. Cargo source directories may be outside your workspace.

## Output

Normal output contains one absolute path per successful request, in input order. Warnings and errors go to stderr. A failed request does not prevent processing the remaining packages.

`--json` outputs one JSON record per request, with either a `result` or an `error`:

```json
{"request":"react","result":{"name":"react","version":"19.0.0","path":"/cache/.../commit","repository":"https://github.com/facebook/react","commit":"...","method":"published"}}
{"request":"serde","result":{"name":"serde","version":"1.0.228","path":"/cargo/registry/src/.../serde-1.0.228","method":"cargo"}}
{"request":"missing","error":"installed dependency `missing` not found"}
```

For npm/pnpm, `method` is `published`, `tag`, `publication-time`, or `default-branch`; the last two are approximations. Cargo results use `method: "cargo"` and omit `repository` and `commit`. Errors before package processing, such as failure to detect the project, only appear on stderr.

Exit status is `0` on success, `1` if any request fails, or `2` for invalid CLI arguments.
