#!/usr/bin/env python3
"""Validate docs/deployment-runbook.md against the repository.

A runbook that has drifted from the scripts it describes is worse than no
runbook. This script checks that every concrete fact the runbook states is
still true:

  * every `scripts/...` path it references exists;
  * the flags it documents (--dry-run, --force) are parsed by deploy_utils.sh;
  * the typed confirmation phrases match deploy_utils.sh / deploy_mainnet.sh;
  * deployer identities and required env vars match the deploy scripts;
  * the pinned Rust version matches rust-toolchain.toml and scripts/build.sh;
  * the WASM paths match the ones the deploy scripts use;
  * the network table matches environments.toml (RPC URL + passphrase);
  * the deploy-state marker path and step names match deploy_utils.sh;
  * every `-- <fn> --arg ...` contract invocation names a real `ttl_vault`
    function and real parameter names;
  * every backend URL it curls is a registered route in backend/src/main.rs;
  * every bash block is syntactically valid (`bash -n`);
  * every linked doc exists and the checklist is present.

Stdlib only. Exit code 0 on success, 1 on failure.
"""
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
RUNBOOK = REPO_ROOT / "docs" / "deployment-runbook.md"
DEPLOY_UTILS = REPO_ROOT / "scripts" / "deploy_utils.sh"
DEPLOY_TESTNET = REPO_ROOT / "scripts" / "deploy_testnet.sh"
DEPLOY_MAINNET = REPO_ROOT / "scripts" / "deploy_mainnet.sh"
BUILD_SH = REPO_ROOT / "scripts" / "build.sh"
TOOLCHAIN = REPO_ROOT / "rust-toolchain.toml"
ENVIRONMENTS = REPO_ROOT / "environments.toml"
TTL_VAULT_LIB = REPO_ROOT / "contracts" / "ttl_vault" / "src" / "lib.rs"
BACKEND_MAIN = REPO_ROOT / "backend" / "src" / "main.rs"

FAILURES: list[str] = []


def check(condition: bool, message: str) -> None:
    print(f"  {'ok  ' if condition else 'FAIL'} {message}")
    if not condition:
        FAILURES.append(message)


def check_script_references(doc: str) -> None:
    refs = sorted(set(re.findall(r"scripts/[\w./-]+\.(?:sh|py)", doc)))
    check(len(refs) >= 4, f"runbook references {len(refs)} scripts")
    for ref in refs:
        check((REPO_ROOT / ref).is_file(), f"{ref} exists")


def check_flags_and_confirmations(doc: str) -> None:
    utils = DEPLOY_UTILS.read_text()
    mainnet = DEPLOY_MAINNET.read_text()
    for flag in ("--dry-run", "--force"):
        check(flag in doc, f"runbook documents {flag}")
        check(re.search(rf"^\s*{re.escape(flag)}\)", utils, re.MULTILINE) is not None,
              f"deploy_utils.sh parses {flag}")

    check('FORCE-REDEPLOY-${network^^}' in utils, "deploy_utils.sh uses FORCE-REDEPLOY-<NETWORK> phrase")
    for network in ("TESTNET", "MAINNET"):
        check(f"FORCE-REDEPLOY-{network}" in doc, f"runbook states FORCE-REDEPLOY-{network}")

    check('"$CONFIRM" != "mainnet"' in mainnet, "deploy_mainnet.sh requires typing 'mainnet'")
    check("type `mainnet` to confirm" in doc, "runbook states the 'mainnet' confirmation")

    check("[dry-run]" in utils and "`[dry-run]`" in doc, "dry-run output prefix matches")


def check_identities_and_env(doc: str) -> None:
    testnet = DEPLOY_TESTNET.read_text()
    mainnet = DEPLOY_MAINNET.read_text()
    check('DEPLOYER="deployer"' in testnet and "| testnet | `deployer` |" in doc,
          "testnet deployer identity matches deploy_testnet.sh")
    check('DEPLOYER_IDENTITY:-deployer-mainnet' in mainnet and "`deployer-mainnet`" in doc,
          "mainnet default identity matches deploy_mainnet.sh")
    check("DEPLOYER_IDENTITY" in doc, "runbook documents DEPLOYER_IDENTITY override")
    for var in re.findall(r'\$\{(\w+):\?', mainnet):
        check(var in doc, f"runbook documents required mainnet env var {var}")


def check_toolchain(doc: str) -> None:
    pinned = re.search(r'channel\s*=\s*"([\d.]+)"', TOOLCHAIN.read_text())
    build = re.search(r'EXPECTED_RUST_VERSION="([\d.]+)"', BUILD_SH.read_text())
    check(pinned is not None and build is not None, "toolchain pins found")
    if pinned and build:
        check(pinned.group(1) == build.group(1), "rust-toolchain.toml and build.sh agree")
        check(f"`{pinned.group(1)}`" in doc, f"runbook states Rust {pinned.group(1)}")
        stale = set(re.findall(r"Rust\s*\|\s*`([\d.]+)`", doc)) - {pinned.group(1)}
        check(not stale, f"no stale Rust versions in runbook ({sorted(stale) or 'none'})")


def check_wasm_paths(doc: str) -> None:
    script_paths = set(re.findall(r"target/wasm32-unknown-unknown/release/\w+\.wasm",
                                  DEPLOY_TESTNET.read_text() + DEPLOY_MAINNET.read_text()))
    doc_paths = set(re.findall(r"target/wasm32-unknown-unknown/release/\w+\.wasm", doc))
    check(bool(doc_paths), "runbook references a WASM path")
    check(doc_paths <= script_paths, f"runbook WASM paths used by deploy scripts ({sorted(doc_paths)})")
    build = BUILD_SH.read_text()
    check("target/wasm-hashes.txt" in build and "target/wasm-hashes.txt" in doc, "hash file path matches build.sh")
    for crate in ("ttl_vault", "zk_verifier", "sbt"):
        check(f"contracts/{crate}/Cargo.toml" in build, f"build.sh builds {crate}")
        check(f"`{crate}.wasm`" in doc, f"runbook lists {crate}.wasm")


def parse_environments() -> dict[str, dict[str, str]]:
    sections: dict[str, dict[str, str]] = {}
    current = None
    for line in ENVIRONMENTS.read_text().splitlines():
        header = re.match(r"^\[(\w+)\]", line)
        if header:
            current = header.group(1)
            sections[current] = {}
            continue
        kv = re.match(r'^(\w+)\s*=\s*"(.*)"', line)
        if kv and current:
            sections[current][kv.group(1)] = kv.group(2)
    return sections


def check_networks(doc: str) -> None:
    envs = parse_environments()
    check(len(envs) >= 2, f"parsed {len(envs)} networks from environments.toml")
    for network, values in envs.items():
        row = re.search(rf"^\|\s*{network}\s*\|\s*`([^`]+)`\s*\|\s*`([^`]+)`", doc, re.MULTILINE)
        check(row is not None, f"runbook network table has {network}")
        if row:
            check(row.group(1) == values.get("rpc_url"), f"{network} RPC URL matches environments.toml")
            check(row.group(2) == values.get("network_passphrase"), f"{network} passphrase matches environments.toml")
        first_key = next(iter(values), None)
        check(first_key == "contract_ttl_vault", f"contract_ttl_vault is first key under [{network}]")


def check_deploy_state(doc: str) -> None:
    utils = DEPLOY_UTILS.read_text()
    check('.deploy-state"' in utils and "${network}.state" in utils, "deploy_utils.sh uses .deploy-state/<network>.state")
    check(".deploy-state/<network>.state" in doc, "runbook documents the marker path")
    scripts = DEPLOY_TESTNET.read_text() + DEPLOY_MAINNET.read_text()
    for step in set(re.findall(r'mark_step_done "\$NETWORK" "(\w+)"', scripts)):
        check(f"`{step}`" in doc, f"runbook documents marker step `{step}`")


def contract_functions() -> dict[str, list[str]]:
    source = TTL_VAULT_LIB.read_text()
    fns: dict[str, list[str]] = {}
    for match in re.finditer(r"pub fn (\w+)\s*\(\s*env: Env\s*,?(.*?)\)", source, re.DOTALL):
        params = re.findall(r"(\w+)\s*:", match.group(2))
        fns.setdefault(match.group(1), params)
    return fns


def check_contract_invocations(doc: str) -> None:
    fns = contract_functions()
    check("initialize" in fns, "ttl_vault exposes initialize")
    joined = doc.replace("\\\n", " ")
    invocations = re.findall(r"--\s+(\w+)((?:\s+--\w+\s+\S+)*)", joined)
    names = set()
    for name, args in invocations:
        names.add(name)
        check(name in fns, f"`{name}` is a ttl_vault contract function")
        if name in fns:
            for arg in re.findall(r"--(\w+)", args):
                check(arg in fns[name], f"`{name}` takes parameter `{arg}`")
    for name in ("initialize", "get_admin", "is_paused", "pause", "unpause", "upgrade"):
        check(name in fns, f"ttl_vault exposes `{name}` referenced by the runbook")
    check({"initialize", "pause", "upgrade"} <= names, "runbook shows initialize, pause and upgrade invocations")

    already_init = re.search(r"AlreadyInitialized = (\d+),", TTL_VAULT_LIB.read_text())
    check(already_init is not None and f"Error(Contract, #{already_init.group(1)})` (`AlreadyInitialized`)" in doc,
          "AlreadyInitialized error code matches ttl_vault ContractError")


def check_backend_urls(doc: str) -> None:
    main = "\n".join(l for l in BACKEND_MAIN.read_text().splitlines() if not l.lstrip().startswith("//"))
    routes = set(re.findall(r'\.route\(\s*"([^"]+)"', main))
    paths = set(re.findall(r"http://localhost:3000(/[\w/-]*)", doc))
    check(bool(paths), f"runbook curls {len(paths)} backend paths")
    for path in sorted(paths):
        check(path in routes, f"backend route {path} exists")
    check('"0.0.0.0:3000"' in main and "listening on 0.0.0.0:3000" in doc, "listen address matches main.rs")
    check("Contract version check failed" in main, "startup failure log line exists in main.rs")
    check("MIN_CONTRACT_VERSION" in main, "MIN_CONTRACT_VERSION is read by main.rs")


def check_bash_syntax(doc: str) -> None:
    bash = shutil.which("bash")
    if not bash:
        print("  skip bash not available; syntax check skipped")
        return
    for index, block in enumerate(re.findall(r"```bash\n(.*?)```", doc, re.DOTALL), start=1):
        with tempfile.NamedTemporaryFile("w", suffix=".sh", delete=False) as handle:
            handle.write(block)
            path = handle.name
        result = subprocess.run([bash, "-n", path], capture_output=True, text=True)
        Path(path).unlink(missing_ok=True)
        check(result.returncode == 0, f"bash block {index} is syntactically valid {result.stderr.strip()}")


def check_links_and_checklist(doc: str) -> None:
    for link in sorted(set(re.findall(r"\]\(([\w.-]+\.md)(?:#[\w-]+)?\)", doc))):
        check((REPO_ROOT / "docs" / link).is_file(), f"linked doc docs/{link} exists")
    boxes = re.findall(r"^- \[ \] ", doc, re.MULTILINE)
    check(len(boxes) >= 15, f"pre-deployment checklist has {len(boxes)} items")
    for section in ("Pre-deployment checklist", "Rollback procedures", "Post-deployment verification"):
        check(f"## " in doc and section in doc, f"runbook has a '{section}' section")


def main() -> int:
    if not RUNBOOK.exists():
        print(f"missing {RUNBOOK}", file=sys.stderr)
        return 1
    doc = RUNBOOK.read_text()
    for title, fn in [
        ("script references", check_script_references),
        ("flags and confirmations", check_flags_and_confirmations),
        ("identities and env vars", check_identities_and_env),
        ("toolchain", check_toolchain),
        ("WASM artefacts", check_wasm_paths),
        ("networks", check_networks),
        ("deploy state", check_deploy_state),
        ("contract invocations", check_contract_invocations),
        ("backend URLs", check_backend_urls),
        ("bash syntax", check_bash_syntax),
        ("links and checklist", check_links_and_checklist),
    ]:
        print(f"Checking {title}...")
        fn(doc)

    if FAILURES:
        print(f"\n{len(FAILURES)} check(s) failed.")
        return 1
    print("\nDeployment runbook validated.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
