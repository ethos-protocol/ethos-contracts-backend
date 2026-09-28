# ZK Proof Verification Guide (Groth16)

This guide explains how Groth16 zero-knowledge proofs are structured and
verified, the byte format Ethos-Protocol expects when a proof is handed to the
`zk_verifier` contract, and how the math maps onto code.

It complements [zk-verifier.md](zk-verifier.md), which documents the
contract's full public API, the current oracle-attestation model and the
migration roadmap. Read this guide first if you are new to Groth16; read
`zk-verifier.md` for contract-specific behaviour.

> **Current status.** The deployed `zk_verifier` contract does **not** yet run
> an on-chain pairing check. It verifies proofs through *oracle attestation*:
> a registered oracle runs the Groth16 verifier described below off-chain and
> then calls `attest(proof, claim)`. `verify_claim` returns `true` only for
> attested `(proof, claim)` pairs whose oracle is still registered. The format
> and math below are exactly what an oracle (or a future native on-chain
> verifier) checks, so proofs produced today stay valid when the contract
> gains native verification.

All Python code blocks in this document are executed, in order, by
[`scripts/test_zk_guide_examples.py`](../scripts/test_zk_guide_examples.py).
The Rust contract examples are mirrored in
[`contracts/zk_verifier/tests/zk_guide_examples.rs`](../contracts/zk_verifier/tests/zk_guide_examples.rs).
If you change an example, update the matching test.

---

## Table of Contents

1. [What Groth16 proves](#1-what-groth16-proves)
2. [Notation and curve parameters](#2-notation-and-curve-parameters)
3. [From a statement to a QAP](#3-from-a-statement-to-a-qap)
4. [Trusted setup: proving and verifying keys](#4-trusted-setup-proving-and-verifying-keys)
5. [Proof structure](#5-proof-structure)
6. [The verification equation](#6-the-verification-equation)
7. [Why the equation holds (completeness)](#7-why-the-equation-holds-completeness)
8. [Proof and verifying-key byte format](#8-proof-and-verifying-key-byte-format)
9. [Worked example in Python](#9-worked-example-in-python)
10. [Using proofs with the `zk_verifier` contract](#10-using-proofs-with-the-zk_verifier-contract)
11. [Security checklist](#11-security-checklist)
12. [Testing the examples](#12-testing-the-examples)

---

## 1. What Groth16 proves

Groth16 is a **zk-SNARK**: a *succinct*, *non-interactive* *argument of
knowledge* that is *zero-knowledge*.

Given a public statement `x` (for example "this vault owner is over 18" or
"I know a preimage of this hash") and a secret witness `w`, the prover
convinces a verifier that it knows `w` such that `C(x, w) = true` for an
agreed circuit `C` — without revealing anything about `w`.

| Property | Meaning for Ethos-Protocol |
|---|---|
| **Completeness** | An honest prover with a valid witness always produces a proof that verifies. |
| **Soundness** | A prover without a valid witness cannot produce a verifying proof (except with negligible probability). |
| **Zero-knowledge** | The proof leaks nothing about the witness beyond the truth of the statement. |
| **Succinctness** | Proofs are constant size (3 group elements, 256 bytes on BN254) and verification costs a constant number of pairings, regardless of circuit size. |

The trade-off is a **per-circuit trusted setup**: each circuit needs its own
proving key and verifying key, produced by a setup ceremony whose secret
randomness ("toxic waste") must be destroyed.

---

## 2. Notation and curve parameters

Groth16 runs over a *pairing-friendly* elliptic curve. Ethos-Protocol uses
**BN254** (also called alt_bn128), the same curve used by Ethereum's pairing
precompiles and by circom/snarkjs tooling.

| Symbol | Meaning |
|---|---|
| `𝔽_r` | Scalar field. All witness values, public inputs and polynomial arithmetic live here. |
| `𝔾₁`, `𝔾₂` | Source groups of order `r`, with generators `G₁`, `G₂`. `𝔾₁` points are over `𝔽_p`, `𝔾₂` points over `𝔽_p²`. |
| `𝔾_T` | Target group (a subgroup of `𝔽_p¹²*`). |
| `e : 𝔾₁ × 𝔾₂ → 𝔾_T` | Bilinear pairing. |
| `[a]₁`, `[a]₂` | The scalar `a` "in the exponent": `a·G₁` and `a·G₂`. |

The one property of the pairing that Groth16 relies on is **bilinearity**:

```
e([a]₁, [b]₂) = e(G₁, G₂)^(a·b)
```

so the verifier can check *multiplicative* relations between hidden scalars
by multiplying pairings, and *additive* relations by adding group elements.

BN254 constants (used by the format helpers in section 8):

```python
# BN254 base-field modulus p (coordinates of G1/G2 points live in F_p / F_p^2)
BN254_P = 21888242871839275222246405745257275088696311157297823662689037894645226208583
# BN254 scalar-field modulus r (order of G1, G2, GT; public inputs are < r)
BN254_R = 21888242871839275222246405745257275088548364400416034343698204186575808495617

FIELD_ELEMENT_BYTES = 32           # one F_p (or F_r) element, big-endian
G1_BYTES = 2 * FIELD_ELEMENT_BYTES  # x || y
G2_BYTES = 4 * FIELD_ELEMENT_BYTES  # x.c1 || x.c0 || y.c1 || y.c0
PROOF_BYTES = G1_BYTES + G2_BYTES + G1_BYTES  # A || B || C

assert PROOF_BYTES == 256
assert BN254_R < BN254_P
```

---

## 3. From a statement to a QAP

Groth16 does not prove arbitrary programs directly. The statement is first
compiled to a **rank-1 constraint system (R1CS)** and then to a **quadratic
arithmetic program (QAP)**.

### 3.1 R1CS

Collect every value the computation touches into one vector

```
a = (1, x₁, …, x_ℓ, w_{ℓ+1}, …, w_m)
     ↑   └─ public ─┘ └─ private ──┘
   constant
```

Each constraint `j` is a triple of vectors `(Uⱼ, Vⱼ, Wⱼ)` requiring

```
⟨Uⱼ, a⟩ · ⟨Vⱼ, a⟩ = ⟨Wⱼ, a⟩        for j = 1 … n
```

i.e. *one multiplication per constraint*. Additions and multiplication by
constants are free (they fold into the linear combinations).

### 3.2 QAP

Pick `n` distinct evaluation points `ω₁ … ωₙ ∈ 𝔽_r`. For each variable `i`
interpolate polynomials `uᵢ(X)`, `vᵢ(X)`, `wᵢ(X)` of degree `< n` with

```
uᵢ(ωⱼ) = Uⱼ[i],   vᵢ(ωⱼ) = Vⱼ[i],   wᵢ(ωⱼ) = Wⱼ[i]
```

and define the **target polynomial** `t(X) = ∏ⱼ (X − ωⱼ)`.

All `n` constraints hold simultaneously **iff** `t(X)` divides

```
p(X) = (Σᵢ aᵢ uᵢ(X)) · (Σᵢ aᵢ vᵢ(X)) − Σᵢ aᵢ wᵢ(X)
```

that is, iff there is a quotient polynomial `h(X)` with

```
p(X) = h(X) · t(X)                                         (QAP)
```

The prover's job is to convince the verifier that such an `h` exists, by
evaluating everything at a secret point `τ` that nobody knows.

---

## 4. Trusted setup: proving and verifying keys

The setup samples secret scalars `τ, α, β, γ, δ ∈ 𝔽_r` (the toxic waste) and
publishes:

**Proving key** (used only by provers — large, circuit-sized):

```
[α]₁, [β]₁, [β]₂, [δ]₁, [δ]₂
{ [τⁱ]₁, [τⁱ]₂ }                                         powers of τ
{ [ (β·uᵢ(τ) + α·vᵢ(τ) + wᵢ(τ)) / δ ]₁ }   for private i
{ [ τⁱ · t(τ) / δ ]₁ }                                    for h(X)
```

**Verifying key** (used by verifiers — small, grows only with `ℓ`):

```
vk = ( [α]₁, [β]₂, [γ]₂, [δ]₂, IC₀ … IC_ℓ )

ICᵢ = [ (β·uᵢ(τ) + α·vᵢ(τ) + wᵢ(τ)) / γ ]₁    for i = 0 … ℓ (constant + public)
```

The division by `γ` (public part) versus `δ` (private part) is what stops a
prover from moving terms between the public and private halves.

`e([α]₁, [β]₂)` is a constant for a given `vk` and is usually **precomputed**
once, leaving three pairings per verification.

---

## 5. Proof structure

A Groth16 proof is three group elements:

```
π = (A, B, C)      A ∈ 𝔾₁,  B ∈ 𝔾₂,  C ∈ 𝔾₁
```

The prover picks fresh random `r, s ∈ 𝔽_r` (these give zero-knowledge) and
computes, in the exponent:

```
A = α + Σᵢ aᵢ uᵢ(τ) + r·δ

B = β + Σᵢ aᵢ vᵢ(τ) + s·δ

C = ( Σ_{i>ℓ} aᵢ (β·uᵢ(τ) + α·vᵢ(τ) + wᵢ(τ)) + h(τ)·t(τ) ) / δ
    + s·A + r·B − r·s·δ
```

`A` and `C` are published in `𝔾₁`, `B` in `𝔾₂`. The prover never learns
`τ, α, β, δ` — it computes these combinations from the proving-key points,
because each term is a known linear combination of published elements.

---

## 6. The verification equation

Given `vk`, public inputs `x₁ … x_ℓ` and a proof `(A, B, C)`:

**Step 1 — input checks.** Every `xᵢ` must satisfy `0 ≤ xᵢ < r`; `A`, `C` must
be valid `𝔾₁` points and `B` a valid `𝔾₂` point in the order-`r` subgroup.

**Step 2 — fold the public inputs** into one `𝔾₁` point (a multi-scalar
multiplication):

```
vk_x = IC₀ + Σᵢ₌₁^ℓ xᵢ · ICᵢ
```

**Step 3 — check one pairing equation:**

```
e(A, B) = e([α]₁, [β]₂) · e(vk_x, [γ]₂) · e(C, [δ]₂)
```

Most implementations (Ethereum's `ecPairing` precompile, Soroban's pairing
host functions, arkworks' `verify_proof`) check the equivalent
**pairing-product form**, which needs a single final exponentiation:

```
e(−A, B) · e([α]₁, [β]₂) · e(vk_x, [γ]₂) · e(C, [δ]₂) = 1
```

Pseudocode:

```text
fn verify(vk, public_inputs, proof) -> bool:
    if len(public_inputs) != len(vk.ic) - 1: return false
    for x in public_inputs:
        if x >= r: return false
    if !on_curve_g1(proof.a) or !on_curve_g2(proof.b) or !on_curve_g1(proof.c):
        return false
    vk_x = vk.ic[0]
    for i, x in enumerate(public_inputs):
        vk_x = vk_x + x * vk.ic[i + 1]
    return pairing_check([
        (-proof.a,   proof.b),
        (vk.alpha_g1, vk.beta_g2),
        (vk_x,       vk.gamma_g2),
        (proof.c,    vk.delta_g2),
    ])
```

---

## 7. Why the equation holds (completeness)

Taking discrete logs (write every element as its scalar in the exponent), the
pairing equation becomes the scalar identity

```
A·B = α·β + vk_x·γ + C·δ          (mod r)
```

Expand the left side using the definitions of `A` and `B`. Let
`U = Σ aᵢuᵢ(τ)`, `V = Σ aᵢvᵢ(τ)`, `W = Σ aᵢwᵢ(τ)`:

```
A·B = (α + U + rδ)(β + V + sδ)
    = αβ + αV + βU + UV + sδ(α + U) + rδ(β + V) + rsδ²
```

On the right, `vk_x·γ = Σ_{i≤ℓ} aᵢ(βuᵢ + αvᵢ + wᵢ)`, and

```
C·δ = Σ_{i>ℓ} aᵢ(βuᵢ + αvᵢ + wᵢ) + h(τ)t(τ) + sAδ + rBδ − rsδ²
```

so `vk_x·γ + C·δ = βU + αV + W + h(τ)t(τ) + sAδ + rBδ − rsδ²`.

By the QAP identity, `UV − W = h(τ)t(τ)`, i.e. `W + h(τ)t(τ) = UV`.
Substituting `A = α + U + rδ` and `B = β + V + sδ` into `sAδ + rBδ − rsδ²`
gives `sδ(α + U) + rδ(β + V) + rsδ²`, so both sides are equal. ∎

**Soundness intuition.** Because `τ, α, β, γ, δ` are unknown, a cheating
prover can only form `A, B, C` as linear combinations of published points.
The `α·β` cross term and the separate `γ`/`δ` denominators force any
verifying `(A, B, C)` to encode a real QAP solution (formally: in the generic
group model, extracting `a` and `h` from any accepting proof).

**Zero-knowledge intuition.** `r` and `s` are uniformly random, so `A` and `B`
are uniformly distributed and `C` is uniquely determined by the equation —
a simulator knowing the toxic waste can produce identically distributed
proofs without a witness.

---

## 8. Proof and verifying-key byte format

This is the canonical encoding Ethos-Protocol uses for Groth16 artefacts. It
matches the Ethereum/`snarkjs` convention so that proofs from circom, gnark
or arkworks can be converted mechanically.

### 8.1 Field elements and points

* **Field element** — 32 bytes, **big-endian**, value `< p` (coordinates) or
  `< r` (scalars / public inputs).
* **𝔾₁ point** — 64 bytes: `x ‖ y`.
* **𝔾₂ point** — 128 bytes: `x.c1 ‖ x.c0 ‖ y.c1 ‖ y.c0`, where an `𝔽_p²`
  element is `c0 + c1·u`. **Note the imaginary part comes first**; this is
  the EIP-197 order and the most common source of "valid proof fails to
  verify" bugs (see [troubleshooting.md](troubleshooting.md)).
* **Point at infinity** — all-zero bytes. It is never a valid `A`, `B` or
  `C` and must be rejected.

### 8.2 Proof (`proof: Bytes`)

| Offset | Length | Field |
|---:|---:|---|
| 0 | 64 | `A` (𝔾₁) |
| 64 | 128 | `B` (𝔾₂) |
| 192 | 64 | `C` (𝔾₁) |
| **total** | **256** | |

256 bytes is well within the contract's `MAX_PROOF_SIZE` of 4096 bytes.

### 8.3 Public inputs (`claim: Bytes`)

The claim is the concatenation of the public inputs, 32 bytes each,
big-endian, in circuit declaration order:

```
claim = x₁ ‖ x₂ ‖ … ‖ x_ℓ          (32·ℓ bytes, each xᵢ < r)
```

With the contract's `MAX_CLAIM_SIZE` of 1024 bytes, a claim can carry at most
**32 public inputs**. Circuits with more public data should hash it into a
single public input (e.g. Poseidon or SHA-256 truncated below `r`).

### 8.4 Verifying key

| Field | Length |
|---|---:|
| `alpha_g1` | 64 |
| `beta_g2` | 128 |
| `gamma_g2` | 128 |
| `delta_g2` | 128 |
| `ic_len` (u32, big-endian) | 4 |
| `ic[0..ic_len]` | 64 each |

`ic_len` must equal `ℓ + 1`. A verifying key with `ic_len = 2` (one public
input) is `448 + 4 + 128 = 580` bytes.

### 8.5 Encoding helpers

```python
def encode_fe(value: int, modulus: int = BN254_P) -> bytes:
    """Encode one field element as 32 big-endian bytes, rejecting out-of-range values."""
    if not 0 <= value < modulus:
        raise ValueError("field element out of range")
    return value.to_bytes(FIELD_ELEMENT_BYTES, "big")


def decode_fe(data: bytes, modulus: int = BN254_P) -> int:
    if len(data) != FIELD_ELEMENT_BYTES:
        raise ValueError("field element must be 32 bytes")
    value = int.from_bytes(data, "big")
    if value >= modulus:
        raise ValueError("field element not canonical (>= modulus)")
    return value


def encode_g1(point) -> bytes:
    x, y = point
    return encode_fe(x) + encode_fe(y)


def encode_g2(point) -> bytes:
    # point = ((x_c0, x_c1), (y_c0, y_c1)); serialized imaginary part first.
    (x_c0, x_c1), (y_c0, y_c1) = point
    return encode_fe(x_c1) + encode_fe(x_c0) + encode_fe(y_c1) + encode_fe(y_c0)


def encode_proof(a, b, c) -> bytes:
    out = encode_g1(a) + encode_g2(b) + encode_g1(c)
    assert len(out) == PROOF_BYTES
    return out


def decode_proof(data: bytes):
    if len(data) != PROOF_BYTES:
        raise ValueError(f"proof must be {PROOF_BYTES} bytes, got {len(data)}")
    fe = [decode_fe(data[i:i + 32]) for i in range(0, PROOF_BYTES, 32)]
    a = (fe[0], fe[1])
    b = ((fe[3], fe[2]), (fe[5], fe[4]))  # undo imaginary-first ordering
    c = (fe[6], fe[7])
    if a == (0, 0) or c == (0, 0) or b == ((0, 0), (0, 0)):
        raise ValueError("point at infinity is not a valid proof element")
    return a, b, c


def encode_public_inputs(inputs) -> bytes:
    return b"".join(encode_fe(x, BN254_R) for x in inputs)


def decode_public_inputs(data: bytes):
    if len(data) == 0 or len(data) % FIELD_ELEMENT_BYTES != 0:
        raise ValueError("claim length must be a non-zero multiple of 32")
    return [decode_fe(data[i:i + 32], BN254_R) for i in range(0, len(data), 32)]


# Round-trip check with arbitrary in-range coordinates.
_a = (1, 2)
_b = ((3, 4), (5, 6))
_c = (7, 8)
_encoded = encode_proof(_a, _b, _c)
assert decode_proof(_encoded) == (_a, _b, _c)
assert _encoded[64:96] == encode_fe(4)  # B.x.c1 is serialized first
assert decode_public_inputs(encode_public_inputs([35, 0, BN254_R - 1])) == [35, 0, BN254_R - 1]
```

---

## 9. Worked example in Python

This section runs the *entire* Groth16 pipeline — R1CS, QAP, setup, proving
and verification — for a tiny circuit. To keep it dependency-free it uses a
**toy pairing**: every group element is represented by its discrete log in
`𝔽_r`, so `e([a]₁, [b]₂) = a·b mod r`. This is completely insecure (discrete
logs are public), but the *algebra* is exactly the algebra of section 7, so it
is a faithful executable check of the formulas.

### 9.1 The statement

> "I know a secret `w` such that `w³ + w + 5 = out`", with `out` public.

For `w = 3`, `out = 35`. Flattened into multiplication gates:

```
sym1 = w · w
y    = sym1 · w
out  = (y + w + 5) · 1
```

Variable vector `a = (1, out, w, sym1, y)`; `out` is the single public input
(`ℓ = 1`).

```python
R = BN254_R

# a = [one, out, w, sym1, y]; index 1 (out) is public.
NUM_PUBLIC = 1
U = [  # left operands
    [0, 0, 1, 0, 0],   # w
    [0, 0, 0, 1, 0],   # sym1
    [5, 0, 1, 0, 1],   # y + w + 5
]
V = [  # right operands
    [0, 0, 1, 0, 0],   # w
    [0, 0, 1, 0, 0],   # w
    [1, 0, 0, 0, 0],   # 1
]
W = [  # outputs
    [0, 0, 0, 1, 0],   # sym1
    [0, 0, 0, 0, 1],   # y
    [0, 1, 0, 0, 0],   # out
]


def witness(w_secret: int):
    sym1 = w_secret * w_secret % R
    y = sym1 * w_secret % R
    out = (y + w_secret + 5) % R
    return [1, out, w_secret, sym1, y]


def dot(row, a):
    return sum(x * y for x, y in zip(row, a)) % R


a = witness(3)
assert a[1] == 35
for j in range(len(U)):
    assert dot(U[j], a) * dot(V[j], a) % R == dot(W[j], a)
```

### 9.2 Polynomials over 𝔽_r

```python
def poly_add(p, q):
    n = max(len(p), len(q))
    return [((p[i] if i < len(p) else 0) + (q[i] if i < len(q) else 0)) % R for i in range(n)]


def poly_sub(p, q):
    return poly_add(p, [(-c) % R for c in q])


def poly_mul(p, q):
    out = [0] * (len(p) + len(q) - 1)
    for i, x in enumerate(p):
        for j, y in enumerate(q):
            out[i + j] = (out[i + j] + x * y) % R
    return out


def poly_scale(p, k):
    return [c * k % R for c in p]


def poly_eval(p, x):
    acc = 0
    for c in reversed(p):
        acc = (acc * x + c) % R
    return acc


def poly_divmod(num, den):
    num = list(num)
    inv_lead = pow(den[-1], R - 2, R)
    quot = [0] * max(len(num) - len(den) + 1, 1)
    while len(num) >= len(den) and any(num):
        shift = len(num) - len(den)
        coef = num[-1] * inv_lead % R
        quot[shift] = coef
        for i, d in enumerate(den):
            num[shift + i] = (num[shift + i] - coef * d) % R
        while num and num[-1] == 0:
            num.pop()
    return quot, num  # remainder == [] means exact division


def lagrange_interpolate(xs, ys):
    result = [0]
    for i, (xi, yi) in enumerate(zip(xs, ys)):
        basis, denom = [1], 1
        for j, xj in enumerate(xs):
            if i != j:
                basis = poly_mul(basis, [(-xj) % R, 1])
                denom = denom * (xi - xj) % R
        result = poly_add(result, poly_scale(basis, yi * pow(denom, R - 2, R)))
    return result
```

### 9.3 R1CS → QAP

```python
POINTS = [1, 2, 3]  # one evaluation point per constraint
NUM_VARS = len(U[0])

u_polys = [lagrange_interpolate(POINTS, [U[j][i] for j in range(3)]) for i in range(NUM_VARS)]
v_polys = [lagrange_interpolate(POINTS, [V[j][i] for j in range(3)]) for i in range(NUM_VARS)]
w_polys = [lagrange_interpolate(POINTS, [W[j][i] for j in range(3)]) for i in range(NUM_VARS)]

t_poly = [1]
for pt in POINTS:
    t_poly = poly_mul(t_poly, [(-pt) % R, 1])


def combine(polys, a):
    acc = [0]
    for poly, ai in zip(polys, a):
        acc = poly_add(acc, poly_scale(poly, ai))
    return acc


def quotient_h(a):
    p = poly_sub(poly_mul(combine(u_polys, a), combine(v_polys, a)), combine(w_polys, a))
    h, remainder = poly_divmod(p, t_poly)
    if any(remainder):
        raise ValueError("witness does not satisfy the QAP: t(X) does not divide p(X)")
    return h


h_poly = quotient_h(a)  # exact division succeeds for a valid witness
```

### 9.4 Setup, prove, verify (toy pairing)

```python
import random

def toy_pairing(g1_scalar, g2_scalar):
    """e([a]1, [b]2) = a*b in the exponent of GT (toy model, NOT secure)."""
    return g1_scalar * g2_scalar % R


def setup(rng):
    tau, alpha, beta, gamma, delta = (rng.randrange(1, R) for _ in range(5))
    inv_gamma, inv_delta = pow(gamma, R - 2, R), pow(delta, R - 2, R)

    def lc(i):  # beta*u_i(tau) + alpha*v_i(tau) + w_i(tau)
        return (beta * poly_eval(u_polys[i], tau)
                + alpha * poly_eval(v_polys[i], tau)
                + poly_eval(w_polys[i], tau)) % R

    vk = {
        "alpha_g1": alpha, "beta_g2": beta, "gamma_g2": gamma, "delta_g2": delta,
        "ic": [lc(i) * inv_gamma % R for i in range(NUM_PUBLIC + 1)],
    }
    # The toy proving key simply keeps the trapdoor; a real one publishes
    # only the group elements listed in section 4.
    pk = {"tau": tau, "alpha": alpha, "beta": beta, "delta": delta,
          "inv_delta": inv_delta, "lc": lc}
    return pk, vk


def prove(pk, a, rng):
    tau, alpha, beta, delta = pk["tau"], pk["alpha"], pk["beta"], pk["delta"]
    r_blind, s_blind = rng.randrange(R), rng.randrange(R)
    u_at = poly_eval(combine(u_polys, a), tau)
    v_at = poly_eval(combine(v_polys, a), tau)
    h_t = poly_eval(quotient_h(a), tau) * poly_eval(t_poly, tau) % R

    A = (alpha + u_at + r_blind * delta) % R
    B = (beta + v_at + s_blind * delta) % R
    private = sum(a[i] * pk["lc"](i) for i in range(NUM_PUBLIC + 1, NUM_VARS)) % R
    C = ((private + h_t) * pk["inv_delta"]
         + s_blind * A + r_blind * B - r_blind * s_blind * delta) % R
    return (A, B, C)


def verify(vk, public_inputs, proof):
    if len(public_inputs) != len(vk["ic"]) - 1:
        return False
    if any(not 0 <= x < R for x in public_inputs):
        return False
    A, B, C = proof
    vk_x = vk["ic"][0]
    for x, ic in zip(public_inputs, vk["ic"][1:]):
        vk_x = (vk_x + x * ic) % R
    lhs = toy_pairing(A, B)
    rhs = (toy_pairing(vk["alpha_g1"], vk["beta_g2"])
           + toy_pairing(vk_x, vk["gamma_g2"])
           + toy_pairing(C, vk["delta_g2"])) % R   # GT multiplication = exponent addition
    return lhs == rhs


def verify_product_form(vk, public_inputs, proof):
    """e(-A,B) * e(alpha,beta) * e(vk_x,gamma) * e(C,delta) == 1  (exponent sum == 0)."""
    A, B, C = proof
    vk_x = vk["ic"][0]
    for x, ic in zip(public_inputs, vk["ic"][1:]):
        vk_x = (vk_x + x * ic) % R
    total = (toy_pairing(-A % R, B)
             + toy_pairing(vk["alpha_g1"], vk["beta_g2"])
             + toy_pairing(vk_x, vk["gamma_g2"])
             + toy_pairing(C, vk["delta_g2"])) % R
    return total == 0


rng = random.Random(580)
pk, vk = setup(rng)
proof = prove(pk, witness(3), rng)

assert verify(vk, [35], proof)                  # completeness
assert verify_product_form(vk, [35], proof)     # both forms agree
assert not verify(vk, [36], proof)              # wrong public input
assert not verify(vk, [35, 1], proof)           # wrong number of inputs
assert not verify(vk, [35 + R], proof)          # non-canonical input rejected
A, B, C = proof
assert not verify(vk, [35], (A, B, (C + 1) % R))  # tampered C
assert prove(pk, witness(3), rng) != proof        # fresh blinding each time
```

---

## 10. Using proofs with the `zk_verifier` contract

The contract is agnostic to the proof system: `proof` and `claim` are opaque
`Bytes`, bounded by `MAX_PROOF_SIZE = 4096` and `MAX_CLAIM_SIZE = 1024`.
For Groth16, pass the 256-byte encoding from §8.2 as `proof` and the
public-input encoding from §8.3 as `claim`.

### 10.1 End-to-end flow

```
 prover (wallet)        oracle (off-chain)                  zk_verifier (on-chain)
 ───────────────        ──────────────────                  ──────────────────────
 build witness
 prove  ──(proof, claim)──▶ decode_proof / decode_public_inputs
                            verify(vk, inputs, proof)  ── true ──▶ attest(oracle, proof, claim)
                                                                     └─ stores sha256(proof),
                                                                        sha256(claim)
 any contract / client ─────────────────────────────────────────────▶ verify_claim(proof, claim)
                                                                     └─ true while oracle registered
                                                                        and credential not invalidated
```

### 10.2 Oracle attestation (Rust, `soroban-sdk` test client)

```rust
use soroban_sdk::{testutils::Address as _, Address, Bytes, Env};
use zk_verifier::{ZkVerifierContract, ZkVerifierContractClient};

let env = Env::default();
env.mock_all_auths();
let admin = Address::generate(&env);
let oracle = Address::generate(&env);

let contract_id = env.register_contract(None, ZkVerifierContract);
let client = ZkVerifierContractClient::new(&env, &contract_id);
client.initialize(&admin);
client.register_oracle(&oracle);

// 256-byte Groth16 proof (A || B || C) and one 32-byte public input (out = 35).
let proof = Bytes::from_array(&env, &[0x11; 256]);
let mut claim_bytes = [0u8; 32];
claim_bytes[31] = 35;
let claim = Bytes::from_array(&env, &claim_bytes);

// The oracle has already run the Groth16 verifier off-chain.
let credential_id = client.attest(&oracle, &proof, &claim);

assert!(client.verify_claim(&proof, &claim));
```

### 10.3 Behaviour to rely on

| Situation | `verify_claim` result |
|---|---|
| Attested pair, oracle registered | `true` (emits `vfy_claim` with `(true, sha256(claim))`) |
| Pair never attested (e.g. different public input) | `false` |
| Attesting oracle later revoked via `revoke_oracle` | `false` |
| Credential invalidated by an upheld dispute | `false` |
| Empty proof | panics `Error(Contract, #1)` `EmptyProof` |
| Empty claim | panics `Error(Contract, #2)` `EmptyClaim` |
| Proof > 4096 bytes | panics `Error(Contract, #3)` `ProofTooLarge` |
| Claim > 1024 bytes | panics `Error(Contract, #4)` `ClaimTooLarge` |

```rust
// Changing a single public input produces an unattested pair.
let mut other = [0u8; 32];
other[31] = 36;
assert!(!client.verify_claim(&proof, &Bytes::from_array(&env, &other)));

// Revoking the oracle withdraws trust in everything it attested.
client.revoke_oracle(&oracle);
assert!(!client.verify_claim(&proof, &claim));
```

### 10.4 Native on-chain verification (roadmap)

When the contract moves to native verification it will store the verifying
key (§8.4) per circuit and run §6 with Soroban's BN254 host functions
(multi-scalar multiplication for `vk_x`, then a four-pair pairing check). The
`(proof, claim)` encoding does not change. See the roadmap in
[zk-verifier.md](zk-verifier.md).

---

## 11. Security checklist

Verifier implementers (oracles today, the contract tomorrow) must:

- [ ] **Reject non-canonical public inputs** (`xᵢ ≥ r`). Otherwise `x` and
      `x + r` both verify, which breaks nullifier/uniqueness schemes.
- [ ] **Check points are on the curve and in the prime-order subgroup**,
      especially `B ∈ 𝔾₂` (BN254's `𝔾₂` has a large cofactor).
- [ ] **Reject the point at infinity** for `A`, `B`, `C`.
- [ ] **Check the public-input count** equals `len(ic) − 1`.
- [ ] **Bind the verifying key to the circuit**: store/compare a hash of the
      `vk`; never accept a caller-supplied `vk`.
- [ ] **Treat proofs as malleable.** Anyone can re-randomise a valid Groth16
      proof into a different valid proof for the same statement. Never use
      `sha256(proof)` alone as a nullifier — bind uniqueness to a public
      input instead. (The contract keys attestations on
      `(sha256(proof), sha256(claim))`, so a re-randomised proof simply needs
      its own attestation.)
- [ ] **Use a ceremony-generated setup** (multi-party, at least one honest
      participant) for production circuits; a single-party setup lets its
      operator forge proofs.
- [ ] **Keep the `𝔾₂` coordinate order** (`c1` before `c0`) consistent across
      prover, oracle and contract.

---

## 12. Testing the examples

```bash
# Execute every Python example in this guide and assert the results.
python3 scripts/test_zk_guide_examples.py

# Contract-level examples from section 10.
cargo test --package zk-verifier --test zk_guide_examples
```

`scripts/test_zk_guide_examples.py` also cross-checks the size limits quoted
in this guide against the constants in `contracts/zk_verifier/src/lib.rs`, so
the guide fails CI if the contract's limits change without the guide being
updated.
