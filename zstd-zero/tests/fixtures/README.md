# Truncated-literal regression inputs

Generated with libzstd 1.5.7 by a deterministic Casita mutation campaign using
seed 0xCA517A. These fixtures pin the existing strict-decoding behavior while
entropy storage moves into caller-owned buffers:

- `wrong-output.zst`: 461 bytes, mutation 3925 of a 1,023-byte skewed-literal
  payload compressed at level 19. Lenient decoding accepts but differs from
  libzstd output.
- `accepted-corruption.zst`: 32 bytes, mutation 27991 of a 31-byte skewed-literal
  payload compressed at level 19. Lenient decoding accepts; libzstd rejects.

Both are generated test data and must be rejected by the default decoder.
