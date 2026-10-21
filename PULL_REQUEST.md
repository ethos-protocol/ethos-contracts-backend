# Bound vault queries and paginate beneficiary/audit-log reads

## Summary

- Deprecate the unbounded audit-log getter and direct callers to bounded pages.
- Bound the activity-log compatibility alias to its first page.
- Add paginated beneficiary-vault queries and cap the deprecated compatibility wrapper.
- Enforce maximum batch sizes for vault status queries (20 IDs) and expiry warnings (50 IDs), returning a typed error when exceeded.
- Add regression coverage for pagination and batch-size boundaries.

## Issues

Closes ethos-protocol/ethos-contracts-backend#764
Closes ethos-protocol/ethos-contracts-backend#763
Closes ethos-protocol/ethos-contracts-backend#762
Closes ethos-protocol/ethos-contracts-backend#761
