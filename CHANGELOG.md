# Changelog

## 0.3.1

- Search both dependency graphs in mixed Node/Cargo projects and return a sole match. Require a package-manager override only when both match.

## 0.3.0

- Add Rust crate lookups using Cargo's resolved local source directories, including aliases, exact resolved versions, optional dependencies, and workspace filters.
- Preserve Cargo lockfiles and keep Cargo sources outside the paths cache.
- Require an explicit package manager when Node and Cargo manifests share the nearest project directory.

## 0.2.0

- Add a 5 GiB soft cache limit with least-recently-used cleanup after new checkouts.
- Preserve modified or busy checkouts and all results from the current invocation during cleanup. Cleanup failures warn without failing source lookups.
- Update usage timestamps on cache hits without scanning cache sizes.
- Add contributor setup, validation, and GitHub release deployment instructions.
