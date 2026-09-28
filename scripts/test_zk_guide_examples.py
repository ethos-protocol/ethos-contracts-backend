#!/usr/bin/env python3
"""Execute and validate the examples in docs/zk-proof-verification-guide.md.

Every ```python fence in the guide is executed, in document order, in one
shared namespace (later examples build on earlier ones, exactly as a reader
following the guide would). Each block carries its own `assert`s; this script
then adds independent checks on the resulting namespace and cross-checks the
numbers quoted in the guide against contracts/zk_verifier/src/lib.rs.

Stdlib only, so it can run in CI without extra dependencies.

Exit code 0 on success, 1 on any failure.
"""
import random
import re
import sys
import traceback
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
GUIDE_PATH = REPO_ROOT / "docs" / "zk-proof-verification-guide.md"
CONTRACT_PATH = REPO_ROOT / "contracts" / "zk_verifier" / "src" / "lib.rs"
RUST_TEST_PATH = REPO_ROOT / "contracts" / "zk_verifier" / "tests" / "zk_guide_examples.rs"

PYTHON_BLOCK_RE = re.compile(r"```python\n(.*?)```", re.DOTALL)

FAILURES: list[str] = []


def check(condition: bool, message: str) -> None:
    if condition:
        print(f"  ok   {message}")
    else:
        print(f"  FAIL {message}")
        FAILURES.append(message)


def run_guide_blocks(text: str) -> dict:
    blocks = PYTHON_BLOCK_RE.findall(text)
    check(len(blocks) >= 6, f"guide contains python examples (found {len(blocks)})")
    namespace: dict = {"__name__": "zk_guide"}
    for index, block in enumerate(blocks, start=1):
        try:
            exec(compile(block, f"<guide python block {index}>", "exec"), namespace)
            check(True, f"python block {index} executes and its asserts pass")
        except Exception:  # noqa: BLE001 - report every failure, keep going
            traceback.print_exc()
            check(False, f"python block {index} executes and its asserts pass")
    return namespace


def check_bn254_constants(ns: dict) -> None:
    p, r = ns.get("BN254_P"), ns.get("BN254_R")
    check(p is not None and r is not None, "BN254 constants defined")
    if p is None or r is None:
        return
    # Well-known BN254 parameterisation: p and r are both derived from
    # u = 4965661367192848881 via p = 36u^4+36u^3+24u^2+6u+1, r = 36u^4+36u^3+18u^2+6u+1.
    u = 4965661367192848881
    check(p == 36 * u**4 + 36 * u**3 + 24 * u**2 + 6 * u + 1, "BN254_P matches curve parameterisation")
    check(r == 36 * u**4 + 36 * u**3 + 18 * u**2 + 6 * u + 1, "BN254_R matches curve parameterisation")
    check(p.bit_length() == 254 and r.bit_length() == 254, "BN254 moduli are 254-bit")


def check_format_helpers(ns: dict) -> None:
    required = ["encode_proof", "decode_proof", "encode_public_inputs", "decode_public_inputs", "encode_fe"]
    missing = [name for name in required if name not in ns]
    check(not missing, f"format helpers defined (missing: {missing or 'none'})")
    if missing:
        return

    rng = random.Random(8)
    p, r = ns["BN254_P"], ns["BN254_R"]
    for _ in range(25):
        a = (rng.randrange(1, p), rng.randrange(1, p))
        b = ((rng.randrange(1, p), rng.randrange(1, p)), (rng.randrange(1, p), rng.randrange(1, p)))
        c = (rng.randrange(1, p), rng.randrange(1, p))
        encoded = ns["encode_proof"](a, b, c)
        if len(encoded) != 256 or ns["decode_proof"](encoded) != (a, b, c):
            check(False, "random proofs round-trip through encode/decode")
            break
    else:
        check(True, "random proofs round-trip through encode/decode")

    def raises(fn, *args) -> bool:
        try:
            fn(*args)
        except ValueError:
            return True
        return False

    check(raises(ns["decode_proof"], b"\x00" * 255), "decode_proof rejects wrong length")
    check(raises(ns["decode_proof"], b"\x00" * 256), "decode_proof rejects point at infinity")
    check(raises(ns["decode_proof"], b"\xff" * 256), "decode_proof rejects non-canonical coordinates")
    check(raises(ns["encode_fe"], r, r), "encode_fe rejects scalar == r")
    check(raises(ns["decode_public_inputs"], b""), "decode_public_inputs rejects empty claim")
    check(raises(ns["decode_public_inputs"], b"\x00" * 33), "decode_public_inputs rejects ragged claim")
    check(raises(ns["decode_public_inputs"], r.to_bytes(32, "big")), "decode_public_inputs rejects x >= r")

    # 32 public inputs is the documented maximum under MAX_CLAIM_SIZE = 1024.
    check(len(ns["encode_public_inputs"](list(range(32)))) == 1024, "32 public inputs encode to exactly 1024 bytes")


def check_groth16_pipeline(ns: dict) -> None:
    required = ["setup", "prove", "verify", "verify_product_form", "witness", "quotient_h"]
    missing = [name for name in required if name not in ns]
    check(not missing, f"groth16 pipeline defined (missing: {missing or 'none'})")
    if missing:
        return

    setup, prove, verify = ns["setup"], ns["prove"], ns["verify"]
    verify_product_form, witness = ns["verify_product_form"], ns["witness"]

    # Completeness over many independent setups, witnesses and blindings.
    rng = random.Random(2024)
    complete, product_agrees, sound_input, sound_tamper = True, True, True, True
    for _ in range(20):
        pk, vk = setup(rng)
        w = rng.randrange(1, 10_000)
        a = witness(w)
        proof = prove(pk, a, rng)
        out = a[1]
        complete &= verify(vk, [out], proof)
        product_agrees &= verify_product_form(vk, [out], proof)
        sound_input &= not verify(vk, [(out + 1) % ns["R"]], proof)
        A, B, C = proof
        sound_tamper &= not verify(vk, [out], ((A + 1) % ns["R"], B, C))
        sound_tamper &= not verify(vk, [out], (A, (B + 1) % ns["R"], C))
    check(complete, "honest proofs verify across 20 random setups")
    check(product_agrees, "pairing-product form agrees with the two-sided form")
    check(sound_input, "proofs do not verify against a different public input")
    check(sound_tamper, "tampered A or B is rejected")

    # A proof built from a vk for one setup must not verify under another.
    pk1, vk1 = setup(rng)
    _, vk2 = setup(rng)
    check(not verify(vk2, [35], prove(pk1, witness(3), rng)), "proof does not verify under a different verifying key")

    # An invalid witness cannot even produce a QAP quotient.
    bad = witness(3)
    bad[3] = (bad[3] + 1) % ns["R"]  # corrupt sym1
    try:
        ns["quotient_h"](bad)
        check(False, "invalid witness is rejected by the QAP divisibility check")
    except ValueError:
        check(True, "invalid witness is rejected by the QAP divisibility check")


def rust_const(name: str, source: str) -> int | None:
    match = re.search(rf"pub const {name}: u32 = (\d+);", source)
    return int(match.group(1)) if match else None


def check_contract_consistency(text: str) -> None:
    source = CONTRACT_PATH.read_text()
    max_proof = rust_const("MAX_PROOF_SIZE", source)
    max_claim = rust_const("MAX_CLAIM_SIZE", source)
    check(max_proof is not None and f"MAX_PROOF_SIZE = {max_proof}" in text,
          f"guide quotes MAX_PROOF_SIZE = {max_proof} from lib.rs")
    check(max_claim is not None and f"MAX_CLAIM_SIZE = {max_claim}" in text,
          f"guide quotes MAX_CLAIM_SIZE = {max_claim} from lib.rs")
    if max_proof is not None:
        check(256 <= max_proof, "a 256-byte Groth16 proof fits within MAX_PROOF_SIZE")
    if max_claim is not None:
        check(f"at most\n**{max_claim // 32} public inputs**" in text or f"**{max_claim // 32} public inputs**" in text,
              f"guide's max public-input count matches MAX_CLAIM_SIZE / 32 = {max_claim // 32}")

    # Every VerifierError the guide cites must match the enum discriminant.
    for code, name in re.findall(r"Error\(Contract, #(\d+)\)` `(\w+)`", text):
        match = re.search(rf"\b{name} = (\d+),", source)
        check(match is not None and match.group(1) == code,
              f"guide error #{code} {name} matches VerifierError in lib.rs")

    # Every contract function the guide calls must exist.
    for fn in sorted(set(re.findall(r"client\.(\w+)\(", text))):
        check(re.search(rf"pub fn {fn}\(", source) is not None, f"contract exposes `{fn}` used in guide")

    check(RUST_TEST_PATH.exists(), "Rust mirror of section 10 examples exists")


def main() -> int:
    if not GUIDE_PATH.exists():
        print(f"missing {GUIDE_PATH}", file=sys.stderr)
        return 1
    text = GUIDE_PATH.read_text()

    print("Executing guide examples...")
    ns = run_guide_blocks(text)
    print("Checking BN254 constants...")
    check_bn254_constants(ns)
    print("Checking proof format helpers...")
    check_format_helpers(ns)
    print("Checking Groth16 pipeline...")
    check_groth16_pipeline(ns)
    print("Checking consistency with the zk_verifier contract...")
    check_contract_consistency(text)

    if FAILURES:
        print(f"\n{len(FAILURES)} check(s) failed.")
        return 1
    print("\nAll ZK guide examples validated.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
