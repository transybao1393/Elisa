# Third-party notices

This repository's own code is under the terms in [LICENSE](LICENSE). The parts listed below come from, or are
adapted from, other projects and keep their own licenses. No third-party model weights are included in this repository.

## 1. Source code adapted from other projects

### MLX and mlx-lm (Apple), MIT License

Several Metal kernels reproduce the thread partition, reduction order and bf16 rounding points of MLX's Metal
kernels (and of mlx-lm's gated-delta kernel), so that the engine matches MLX bit for bit. The kernels are written
for this engine (different buffers, fused ops, multi-token dispatch), but the numerical structure follows MLX.

| File | Origin |
|---|---|
| [engine/shaders/attn_gated.metal](engine/shaders/attn_gated.metal) | MLX `mlx/backend/metal/kernels`: `rms_norm`, `rope`, `sdpa_vector.h` (`sdpa_vector`, `sdpa_vector_2pass`), unary `Sigmoid`; steel `BaseMMAFrag` fragment layout (`attn_flash_b16`) |
| [engine/shaders/gemv_q4.metal](engine/shaders/gemv_q4.metal) | MLX `quantized.h` `load_vector` / `qdot` method (pre-scaled x, unshifted nibble masks) |
| [engine/shaders/sparse_up.metal](engine/shaders/sparse_up.metal) | MLX `quantized.h` `load_vector` / `qdot` method, on gathered rows |
| [engine/qwen-dense/shaders/attn_hd128_sdpa.metal](engine/qwen-dense/shaders/attn_hd128_sdpa.metal) | MLX `sdpa_vector.h` (`sdpa_vector`, `sdpa_vector_2pass`) at head dim 128, and the host block-count rule of `scaled_dot_product_attention.cpp` |
| [engine/qwen-dense/shaders/attn_hd128.metal](engine/qwen-dense/shaders/attn_hd128.metal), [attn_hd128_flash.metal](engine/qwen-dense/shaders/attn_hd128_flash.metal) | Via `attn_gated.metal`: MLX `rms_norm`, `rope`, sdpa partition and rounding, steel `BaseMMAFrag` layout |
| [engine/shaders/deltanet.metal](engine/shaders/deltanet.metal) | mlx-lm `models/gated_delta.py` (packed Dk=128 gated-delta kernel), `models/qwen3_5.py` `GatedDeltaNet`; MLX `rms_norm`, `Sigmoid`, `log1p` |
| [engine/shaders/gemv_bf16.metal](engine/shaders/gemv_bf16.metal) | MLX `gemv.h` `GEMVKernel` |
| [engine/shaders/gemv_q4_bf16.metal](engine/shaders/gemv_q4_bf16.metal) | MLX `quantized.h` `qmv_fast` / `qmv`, `load_vector`, `qdot` |
| [engine/shaders/norm_bf16.metal](engine/shaders/norm_bf16.metal) | MLX `rms_norm` (`rms_single_row` / `rms_looped`) |
| [engine/shaders/spec.metal](engine/shaders/spec.metal) | MLX `rms_norm` (`rms_single_row` / `rms_looped`) |
| [engine/shaders/gemm_q4.metal](engine/shaders/gemm_q4.metal) | MLX steel `BaseMMAFrag` fragment coordinates; MLX affine 4-bit layout |
| [engine/shaders/gemv_mma_q4.metal](engine/shaders/gemv_mma_q4.metal) | MLX steel `BaseMMAFrag` fragment coordinates; MLX affine 4-bit layout |

Each of these files starts with a header naming the origin and license.

Sources: MLX <https://github.com/ml-explore/mlx>, mlx-lm <https://github.com/ml-explore/mlx-lm>. Both projects use
the same license text and copyright line (checked 2026-11-02):

```
MIT License

Copyright © 2023 Apple Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### Qwen3.8 chat template text, Apache-2.0

[engine/crates/streamdense-tokenizer/src/chat.rs](engine/crates/streamdense-tokenizer/src/chat.rs) renders the
Qwen3.8 chat format in Rust (no template engine) and reproduces the template's fixed strings word for word
(the tool-calling instructions `TOOLS_HEAD` / `TOOLS_TAIL` and the template's error messages), because the prompt must
tokenize exactly as the model expects. Source: `chat_template.jinja` of
[Qwen/Qwen3.8-27B](https://huggingface.co/Qwen/Qwen3.8-27B) (the copy in the
[p4ik/Qwen3.8-27B-MLX-4bit](https://huggingface.co/p4ik/Qwen3.8-27B-MLX-4bit) package says it is adapted from unsloth),
licensed under the Apache License 2.0: https://www.apache.org/licenses/LICENSE-2.0

### Reimplemented behavior (no code copied)

These parts are written from scratch to give the same results as another project. No source code was copied,
so no license notice is required. They are listed for transparency.

| File | Matches the behavior of | License of the reference |
|---|---|---|
| [engine/crates/streamdense-tokenizer/src/tokenizer.rs](engine/crates/streamdense-tokenizer/src/tokenizer.rs) | Hugging Face `tokenizers` byte-level BPE; the pre-tokenizer regex string that transformers' `Qwen3_5Tokenizer` installs (copied, a short regex); the GPT-2 byte-to-unicode table (OpenAI GPT-2, MIT) | Apache-2.0 ([tokenizers](https://github.com/huggingface/tokenizers), [transformers](https://github.com/huggingface/transformers)) |
| [elisa/mem/src/embed.rs](elisa/mem/src/embed.rs) | BERT WordPiece tokenizer and forward pass of all-MiniLM-L6-v2 (sentence-transformers mean pooling) | Apache-2.0 ([sentence-transformers](https://github.com/UKPLab/sentence-transformers)) |
| `engine/shaders/gemv_lut_q4.metal` | The T-MAC / LUT-GEMM lookup-table method (from the papers) | n/a (method only) |

## 2. Rust crate dependencies

Crates are fetched from crates.io at build time; this repository does not include their source. If you ship a
compiled binary, include the license texts of these crates with it (for example with `cargo about`).

Three separate Cargo workspaces (from `cargo metadata`, 2026-11-02, every target platform):

| Workspace | Direct dependencies | Crates in the lock graph |
|---|---|---|
| [engine/](engine/Cargo.toml) | libc, objc2, objc2-foundation, objc2-metal, regex, serde, serde_json, toml, unicode-normalization, ureq | 70 |
| [elisa/](elisa/Cargo.toml) | crossterm, libc, ratatui, regex, serde, serde_json, toml, ureq, walkdir, elisa-mem (local), tree-sitter + 7 grammars (below) | 132 |
| [elisa/mem/](elisa/mem/Cargo.toml) | crc32fast, libc, lz4_flex | 5 |

Almost every crate is `MIT OR Apache-2.0`, `MIT`, `Apache-2.0 OR MIT`, or `Unlicense OR MIT`. No GPL, LGPL, MPL
or other copyleft license appears. Crates under a license other than plain MIT / Apache-2.0 (all permissive):

| Crate | License | Note |
|---|---|---|
| ring 0.17 | Apache-2.0 AND ISC | TLS crypto (via ureq / rustls) |
| rustls-webpki 0.103, untrusted 0.9 | ISC | TLS (via ureq) |
| subtle 2.6 | BSD-3-Clause | via rustls |
| webpki-roots 1.0 | CDLA-Permissive-2.0 | Mozilla CA root list as data; permissive data license |
| foldhash 0.1 / 0.2, zlib-rs 0.6 | Zlib | |
| unicode-ident 1.0, unicode-normalization 0.1 | (MIT OR Apache-2.0) AND Unicode-3.0 | Unicode data tables |
| ryu 1.0 | Apache-2.0 OR BSL-1.0 | |
| adler2 2.0 | 0BSD OR MIT OR Apache-2.0 | |

Regenerate with, in each workspace:
`cargo metadata --format-version 1 | jq -r '.packages[] | select(.source != null) | "\(.name) \(.version) \(.license)"'`

Syntax-tree crates in [elisa/](elisa/Cargo.toml) (added 2026-11-03 for `elisa/src/ast.rs`; versions pinned with `=`; each grammar is
an optional cargo feature, all on by default). All MIT; the grammar crates compile their bundled C parser tables
(`parser.c`, plus `scanner.c` where present) under the same MIT license. Copyright lines from each crate's `LICENSE`
file, or from its `authors` field where the published crate ships no `LICENSE` file:

| Crate | Version | License | Copyright | Source |
|---|---|---|---|---|
| tree-sitter | 0.27.1 | MIT | Copyright (c) 2018 Max Brunsfeld | <https://github.com/tree-sitter/tree-sitter> |
| tree-sitter-language | 0.1.9 | MIT | Copyright (c) 2018 Max Brunsfeld | <https://github.com/tree-sitter/tree-sitter> |
| tree-sitter-rust | 0.24.2 | MIT | Copyright (c) 2017 Maxim Sokolov | <https://github.com/tree-sitter/tree-sitter-rust> |
| tree-sitter-python | 0.25.0 | MIT | Copyright (c) 2016 Max Brunsfeld | <https://github.com/tree-sitter/tree-sitter-python> |
| tree-sitter-swift | 0.7.4 | MIT | Copyright (c) 2021 alex-pinkus | <https://github.com/alex-pinkus/tree-sitter-swift> |
| tree-sitter-typescript | 0.23.2 | MIT | Max Brunsfeld, Amaan Qureshi and contributors (no LICENSE file in the crate) | <https://github.com/tree-sitter/tree-sitter-typescript> |
| tree-sitter-javascript | 0.25.0 | MIT | Copyright (c) 2014 Max Brunsfeld | <https://github.com/tree-sitter/tree-sitter-javascript> |
| tree-sitter-bash | 0.25.1 | MIT | Copyright (c) 2017 Max Brunsfeld | <https://github.com/tree-sitter/tree-sitter-bash> |
| tree-sitter-sequel (SQL) | 0.3.11 | MIT | Derek Stride (no LICENSE file in the crate) | <https://github.com/derekstride/tree-sitter-sql> |
| streaming-iterator (via tree-sitter) | 0.1.9 | MIT OR Apache-2.0 | Steven Fackler | <https://github.com/sfackler/streaming-iterator> |

Build-time only: `cc` (MIT OR Apache-2.0) compiles the grammars' C code. tree-sitter-sequel pins `cc ~1.2`, so the
elisa lock file moved `cc` from 1.10.0 to 1.2.67 (it was already a build dependency of `ring`). No crate in this
set has a license other than MIT / Apache-2.0.

Document crates in [elisa/](elisa/Cargo.toml) (added 2026-11-08 for `elisa/src/mcp_docs.rs`, the built-in documents
MCP server; versions pinned with `=`, default features). Pure Rust, no C code. The direct dependencies and the crates
they bring in that carry the document parsing:

| Crate | Version | License | Copyright | Source |
|---|---|---|---|---|
| pdf-extract | 0.12.1 | MIT | Jeff Muizelaar (no LICENSE file in the crate) | <https://github.com/jrmuizel/pdf-extract> |
| lopdf (via pdf-extract, no default features) | 0.42.0 | MIT | Copyright (c) 2016 Junfeng Liu | <https://github.com/J-F-Liu/lopdf> |
| adobe-cmap-parser, type1-encoding-parser (via pdf-extract) | 0.4.1, 0.1.1 | MIT | Jeff Muizelaar (no LICENSE file in the crate) | <https://github.com/jrmuizel> |
| cff-parser (via pdf-extract) | 0.2.0 | MIT OR Apache-2.0 | its authors (LICENSE-MIT / LICENSE-APACHE) | <https://crates.io/crates/cff-parser> |
| postscript (via pdf-extract) | 0.14.1 | Apache-2.0 OR MIT | Copyright 2015–2022 The postscript Developers | <https://github.com/bodoni/postscript> |
| pom (via the cmap / type1 parsers) | 1.1.0 | MIT | Copyright (c) 2016 Junfeng Liu | <https://github.com/J-F-Liu/pom> |
| nom (via lopdf) | 8.0.0 | MIT | Copyright (c) 2014-2019 Geoffroy Couprie | <https://github.com/rust-bakery/nom> |
| ecb (via lopdf) | 0.1.2 | MIT | Copyright (c) magic-akari | <https://github.com/RustCrypto/block-modes> |
| calamine | 0.38.0 | MIT | Copyright (c) 2016 Johann Tuffe | <https://github.com/tafia/calamine> |
| quick-xml (direct for HTML entities in fetched pages, src/web.rs, `escape-html` only; and via calamine) | 0.42.0 | MIT | Copyright (c) 2016 Johann Tuffe | <https://github.com/tafia/quick-xml> |
| zip (direct for .docx text in src/attach.rs, and via calamine; `deflate` only) | 9.4.2 | MIT | Copyright (c) 2014 Mathijs van de Nes | <https://github.com/zip-rs/zip2> |

The other crates they add are the usual `MIT OR Apache-2.0` set (RustCrypto aes / cbc / md-5 / sha2 for PDF
decryption, euclid, flate2, codepage, atoi_simd, fast-float2, rand, …) plus these permissive ones: encoding_rs 0.8
`(Apache-2.0 OR MIT) AND BSD-3-Clause` (WHATWG encoding data), unicode-properties 0.1 `(MIT OR Apache-2.0) AND
Unicode-3.0`, constant_time_eq 0.6 `CC0-1.0 OR MIT-0 OR Apache-2.0`, miniz_oxide 0.9 `MIT OR Zlib OR Apache-2.0`,
zopfli 0.8 `Apache-2.0`, nix 0.31 `MIT`, zlib-rs 0.6 `Zlib` (already listed above). No copyleft license. Release
binary growth (arm64): about 2.4 MB (21.9 MB → 24.4 MB).

Hashing in [elisa/](elisa/Cargo.toml) (added 2026-11-08 for `elisa/src/pins.rs`, MCP tool-list pinning, and
`elisa/src/audit.rs`, the audit log's hash chain; pinned with `=`). Already in the lock graph via lopdf above, so no
new crates and no binary growth to speak of:

| Crate | Version | License | Copyright | Source |
|---|---|---|---|---|
| sha2 | 0.10.9 | MIT OR Apache-2.0 | Copyright (c) 2006-2009 Graydon Hoare, 2009-2013 Mozilla Foundation, 2016 Artyom Pavlov (LICENSE-MIT) | <https://github.com/RustCrypto/hashes> |
| digest, block-buffer, crypto-common, generic-array, typenum, cpufeatures (via sha2) | 0.10.7, 0.10.4, 0.1.7, 0.14.7, 1.x, 0.2.17 | MIT OR Apache-2.0 (generic-array: MIT) | their authors | <https://github.com/RustCrypto> |

Expression crate in [elisa/](elisa/Cargo.toml) (added 2026-11-08 for `calc` in `elisa/src/mcp_reason.rs`, the
built-in reasoning server; pinned with `=`, no default features, no dependencies of its own; pure Rust):

| Crate | Version | License | Copyright | Source |
|---|---|---|---|---|
| evalexpr | 11.3.1 | MIT | Copyright (c) 2019 Sebastian Schmidt | <https://github.com/ISibboI/evalexpr> |

Pinned on purpose: 11.3.1 is evalexpr's last MIT release; 12.0.0 and later (from 2024-10-17) are `AGPL-3.0-only`,
a copyleft license that must not enter this proprietary build. Do not raise the pin without checking the license.

Security crates in [elisa/](elisa/Cargo.toml) (added 2026-11-08 for `elisa/src/mcp_sec.rs`, the built-in security MCP
server; pinned with `=`, minimal features; pure Rust, about 0.5 MB of release binary with the server's own code).

| Crate | Version | License | Copyright | Source |
|---|---|---|---|---|
| goblin | 0.10.7 | MIT | Copyright (c) m4b 2016-2024 | <https://github.com/m4b/goblin> |
| scroll, scroll_derive (via goblin) | 0.13.0, 0.13.2 | MIT | Copyright (c) m4b 2016 | <https://github.com/m4b/scroll> |
| plain (via goblin) | 0.2.3 | MIT OR Apache-2.0 | Copyright (c) 2017 Plain contributors | <https://github.com/randomites/plain> |

yara-x (malware scanning, with wasmtime / cranelift) was removed on 2026-11-08: it is no longer a dependency, optional
or not. It may return once the ratatui upgrade allows yara-x ≥ 1.19.

## 3. Swift packages

[app/Package.swift](app/Package.swift) has no package dependencies (Apple system frameworks only: SwiftUI, AppKit, IOKit).

## 3a. Fonts

The Elisa app ([app/ElisaApp](app/ElisaApp)) bundles the two typefaces named by the Elisa brand kit
(`elisa/design/Elisa Brand Kit/03-type`) as SwiftPM resources in
[app/ElisaApp/Sources/ElisaApp/Resources/Fonts/](app/ElisaApp/Sources/ElisaApp/Resources/Fonts). Each folder keeps the
font's own `OFL.txt`. The files are unmodified copies from the Google Fonts repository at commit
`2fe1e8c6b137fb13041a7cfca6fdcd8a910ea4dc` (fetched 2026-11-03).

| Font | Files | License | Copyright | Source |
|---|---|---|---|---|
| Silkscreen | `Silkscreen-Regular.ttf`, `Silkscreen-Bold.ttf`, `OFL.txt` | SIL Open Font License 1.1 | Copyright 2001 The Silkscreen Project Authors (https://github.com/googlefonts/silkscreen) | <https://github.com/google/fonts/tree/main/ofl/silkscreen> |
| Chivo Mono | `ChivoMono[wght].ttf` (variable, wght axis), `OFL.txt` | SIL Open Font License 1.1 | Copyright 2019 The Chivo Project Authors (https://github.com/Omnibus-Type/Chivo) | <https://github.com/google/fonts/tree/main/ofl/chivomono> |

SHA-256: Silkscreen-Regular.ttf `c845473330b94c2079ce9af01c51ac8ba2d99c24f4d14c039843bbb8e642ebd8`,
Silkscreen-Bold.ttf `768476aa712d4f5c3e18d3bce80f980a8bd3f72b7094d22ec5e768df3acfed61`,
ChivoMono[wght].ttf `725256f30b7b1b25dd001a96ff8d4a23773197bb886cd847a97ff8eabc9c1d9d`.
The OFL allows bundling with software; the fonts are not sold on their own and keep their Reserved Font Names.

## 4. Models

No model weights are distributed in this repository (`models/` is git-ignored). Users download them from Hugging Face
themselves (for example with `sd-fetch`) and must follow each model's license. Licenses below are the `license` field
of each model card, checked 2026-11-02.

| Model | Used for | License |
|---|---|---|
| [Qwen/Qwen3.8-27B](https://huggingface.co/Qwen/Qwen3.8-27B) | Main target (base model) | Apache-2.0 |
| [p4ik/Qwen3.8-27B-MLX-4bit](https://huggingface.co/p4ik/Qwen3.8-27B-MLX-4bit) | 4-bit MLX package the engine loads (`models/qwen38_27b_mlx4`) | Apache-2.0 (base_model Qwen/Qwen3.8-27B) |
| [sentence-transformers/all-MiniLM-L6-v2](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2) | Elisa memory embeddings (optional) | Apache-2.0 |
| [Qwen/Qwen3-8B](https://huggingface.co/Qwen/Qwen3-8B), [Qwen/Qwen3-14B](https://huggingface.co/Qwen/Qwen3-14B), [Qwen/Qwen3-32B](https://huggingface.co/Qwen/Qwen3-32B) | Config shapes in `engine/qwen-dense` | Apache-2.0 |
| [Qwen/Qwen2.5-0.5B-Instruct](https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct), [Qwen/Qwen2.5-7B-Instruct](https://huggingface.co/Qwen/Qwen2.5-7B-Instruct), [Qwen/Qwen2.5-14B-Instruct](https://huggingface.co/Qwen/Qwen2.5-14B-Instruct) | Config shapes in `engine/qwen-dense` | Apache-2.0 (note: some other Qwen2.5 sizes, e.g. 3B and 72B, use different Qwen licenses) |

Test data: [engine/testdata/vectors/](engine/testdata/vectors) holds random inputs at real shapes run through MLX
ops (no model weights); [refs/](refs) holds token ids, logits and activation dumps produced with MLX / mlx-lm from a
randomly initialised tiny model and from Qwen3.8-27B (model outputs, not weights).
