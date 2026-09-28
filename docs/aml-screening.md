# AML Screening (#547)

Beneficiary addresses are screened against sanctions data, and transfers to
flagged addresses are blocked.

## On-chain (`contracts/ttl_vault/src/aml.rs`)

- `check_aml_compliance(env, address) -> bool` — `false` if the address is on
  the local flag list or the configured sanctions oracle reports it.
- `set_aml_oracle(admin, Option<Address>)` — oracle contract exposing
  `is_sanctioned(Address) -> bool` (Chainalysis on-chain sanctions list
  interface). Oracle call failures **fail closed**.
- `set_aml_reporter(admin, Option<Address>)` — service account (the backend)
  allowed to flag/unflag besides the admin.
- `flag_aml_address(caller, address, reason)` / `unflag_aml_address(caller, address)`
  / `get_aml_flag(address)`.

Enforcement: `create_vault`, `set_beneficiaries`, `add_beneficiary` and
`update_beneficiary` reject flagged beneficiaries, and every token transfer
from a vault to a non-owner recipient is preceded by a compliance check.
Failures return `ContractError::AmlFlaggedAddress` (131).

## Backend (`backend/src/aml.rs`)

Screens addresses with the Chainalysis sanctions API plus a manual flag list.

| Variable              | Default                                         |
|-----------------------|-------------------------------------------------|
| `CHAINALYSIS_API_KEY` | unset — provider screening disabled             |
| `CHAINALYSIS_API_URL` | `https://public.chainalysis.com/api/v1/address` |
| `AML_CACHE_TTL_SECS`  | `3600`                                          |
| `AML_FAIL_OPEN`       | `false` (provider errors block)                 |

```
GET    /aml/check/:address   # screening verdict
POST   /aml/flags            # admin: {"address", "reason", "flagged_by"}
GET    /aml/flags            # admin
DELETE /aml/flags/:address   # admin
```
