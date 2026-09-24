# Changelog

## 0.2.0

- Add a 5 GiB soft cache limit with least-recently-used cleanup after new checkouts.
- Preserve modified or busy checkouts and all results from the current invocation during cleanup. Cleanup failures warn without failing source lookups.
- Update usage timestamps on cache hits without scanning cache sizes.
- Add contributor setup, validation, and GitHub release deployment instructions.
