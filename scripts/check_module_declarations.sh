#!/usr/bin/env bash
# Issue #672 — CI guard: every module path used in a crate root must be declared.
#
# A call such as `composition_rules::get_rule(...)` in lib.rs only compiles when
# lib.rs also contains `mod composition_rules;` (or `pub mod`, `extern crate`,
# or a `use` that brings the name into scope). Commit 321c32e removed the
# declaration while the call sites stayed, and the crate stopped building on
# main. This script fails fast on that class of mistake before cargo runs.
#
# Usage: scripts/check_module_declarations.sh [path/to/lib.rs]

set -euo pipefail

LIB_RS="${1:-contracts/ttl_vault/src/lib.rs}"

if [ ! -f "$LIB_RS" ]; then
  echo "❌ crate root not found: $LIB_RS"
  exit 1
fi

# Path prefixes that are not local modules: external crates, language
# keywords, primitive types, and turbofish `get::<T>` / attribute `clippy::`.
EXCLUDED='^(crate|self|super|core|alloc|std|soroban_sdk|clippy|get|i8|i16|i32|i64|i128|isize|u8|u16|u32|u64|u128|usize|f32|f64|bool|char|str)$'

# Every lowercase identifier that is used as a path prefix (`name::`).
used=$(grep -oE '(^|[^A-Za-z0-9_:])[a-z_][a-z0-9_]*::' "$LIB_RS" \
  | sed -E 's/^[^a-z_]//; s/::$//' \
  | sort -u \
  | grep -Ev "$EXCLUDED" || true)

# Full `use ...;` statements, including ones spanning several lines, joined
# onto single lines so a name imported inside a `use { ... }` block counts.
use_stmts=$(awk '
  /^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?use[[:space:]]/ { in_use = 1 }
  in_use { buf = buf " " $0 }
  in_use && /;[[:space:]]*$/ { print buf; buf = ""; in_use = 0 }
' "$LIB_RS")

missing=0
for name in $used; do
  if grep -qE "^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+${name}[[:space:]]*;" "$LIB_RS" \
    || grep -qE "\buse\b.*\b${name}\b" <<<"$use_stmts" \
    || grep -qE "^[[:space:]]*extern[[:space:]]+crate[[:space:]]+${name}\b" "$LIB_RS"; then
    continue
  fi
  echo "❌ '${name}::' is used in ${LIB_RS} but no 'mod ${name};' (or use/extern crate) declares it"
  missing=1
done

if [ "$missing" -ne 0 ]; then
  echo ""
  echo "   Restore the module declaration in the crate root, or correct the call path."
  exit 1
fi

echo "✅ All module paths used in ${LIB_RS} are declared."
