/// Shared byte-level codec for storage-reduction features — Issues #558, #559.
///
/// Two primitives are provided:
///
/// - **XOR delta** — `xor_delta(prev, next)` produces a buffer the length of
///   `next` where every byte that did not change relative to `prev` is zero.
///   `apply_delta(prev, delta)` inverts it. Unchanged regions therefore become
///   long zero runs, which is what makes the RLE stage effective for
///   differential (snapshot-to-snapshot) compression.
/// - **Zero-run RLE** — `rle_encode` / `rle_decode` compress a buffer as a
///   sequence of tokens:
///     * `0x00 n`          — `n` zero bytes (`1..=255`)
///     * `0x01 n b1 .. bn` — `n` literal bytes (`1..=255`)
///
/// Work is done in guest memory (`alloc::vec::Vec<u8>`) rather than through
/// per-byte host calls on `Bytes`, which keeps instruction cost linear and low.
use alloc::vec::Vec as StdVec;
use soroban_sdk::{Bytes, Env};

const TOKEN_ZEROS: u8 = 0x00;
const TOKEN_LITERAL: u8 = 0x01;
const MAX_RUN: usize = 255;
/// Zero runs shorter than this are cheaper to keep inside a literal token.
const MIN_ZERO_RUN: usize = 3;

/// Copies a `Bytes` value into guest memory.
pub fn to_buf(bytes: &Bytes) -> StdVec<u8> {
    let mut buf = alloc::vec![0u8; bytes.len() as usize];
    bytes.copy_into_slice(&mut buf);
    buf
}

/// XORs `next` against `prev` (missing `prev` bytes are treated as zero).
pub fn xor_delta_buf(prev: &[u8], next: &[u8]) -> StdVec<u8> {
    next.iter()
        .enumerate()
        .map(|(i, b)| b ^ prev.get(i).copied().unwrap_or(0))
        .collect()
}

/// Encodes `input` with zero-run RLE.
pub fn rle_encode_buf(input: &[u8]) -> StdVec<u8> {
    let mut out = StdVec::with_capacity(input.len() / 2 + 4);
    let mut literal: StdVec<u8> = StdVec::new();
    let mut i = 0;

    while i < input.len() {
        if input[i] == 0 {
            let mut run = 0;
            while i + run < input.len() && input[i + run] == 0 && run < MAX_RUN {
                run += 1;
            }
            if run >= MIN_ZERO_RUN {
                flush_literal(&mut out, &mut literal);
                out.push(TOKEN_ZEROS);
                out.push(run as u8);
                i += run;
                continue;
            }
        }
        literal.push(input[i]);
        if literal.len() == MAX_RUN {
            flush_literal(&mut out, &mut literal);
        }
        i += 1;
    }
    flush_literal(&mut out, &mut literal);
    out
}

fn flush_literal(out: &mut StdVec<u8>, literal: &mut StdVec<u8>) {
    if literal.is_empty() {
        return;
    }
    out.push(TOKEN_LITERAL);
    out.push(literal.len() as u8);
    out.extend_from_slice(literal);
    literal.clear();
}

/// Decodes a zero-run RLE stream. Returns `None` if the stream is malformed.
pub fn rle_decode_buf(input: &[u8]) -> Option<StdVec<u8>> {
    let mut out = StdVec::with_capacity(input.len() * 2);
    let mut i = 0;
    while i < input.len() {
        let token = input[i];
        let n = *input.get(i + 1)? as usize;
        if n == 0 {
            return None;
        }
        i += 2;
        match token {
            TOKEN_ZEROS => out.resize(out.len() + n, 0),
            TOKEN_LITERAL => {
                out.extend_from_slice(input.get(i..i + n)?);
                i += n;
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Compresses `bytes` with zero-run RLE.
pub fn compress(env: &Env, bytes: &Bytes) -> Bytes {
    Bytes::from_slice(env, &rle_encode_buf(&to_buf(bytes)))
}

/// Inverse of [`compress`]. Returns `None` if `bytes` is not a valid stream.
pub fn decompress(env: &Env, bytes: &Bytes) -> Option<Bytes> {
    rle_decode_buf(&to_buf(bytes)).map(|buf| Bytes::from_slice(env, &buf))
}

/// Differentially compresses `next` against `prev`: XOR delta followed by
/// zero-run RLE. Identical inputs compress to an empty buffer.
pub fn diff_compress(env: &Env, prev: &Bytes, next: &Bytes) -> Bytes {
    let delta = xor_delta_buf(&to_buf(prev), &to_buf(next));
    Bytes::from_slice(env, &rle_encode_buf(&delta))
}

/// Inverse of [`diff_compress`]: reconstructs a value of length `len` from
/// `prev` and the compressed delta. Returns `None` if the delta is malformed
/// or does not decode to exactly `len` bytes.
pub fn diff_decompress(env: &Env, prev: &Bytes, delta: &Bytes, len: u32) -> Option<Bytes> {
    let decoded = rle_decode_buf(&to_buf(delta))?;
    if decoded.len() != len as usize {
        return None;
    }
    let next = xor_delta_buf(&to_buf(prev), &decoded);
    Some(Bytes::from_slice(env, &next))
}
