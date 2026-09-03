# Conduit API licensing

This document is a practical map of the repository's license files. The
license texts themselves control if this summary and those texts differ.

## License scopes

- `crates/conduit-llm/`, `crates/conduit-pipeline/`, and
  `crates/conduit-transformers/` are licensed under `LGPL-3.0-only`. The
  authoritative text is in `LICENSES/LGPL-3.0-only.txt`.
- The rest of Conduit API is licensed under Apache-2.0 unless a file carries a
  different notice. The authoritative Apache-2.0 text is in `LICENSE`.
- Bundled and derived third-party components retain their own terms and
  attributions. See `NOTICE`, `frontend/NOTICE`, the generated web-console
  `THIRD_PARTY_LICENSES.md`, and source-file headers.

Contributions follow the license scope of the files they modify. Moving code
between the Apache-2.0 and LGPL-3.0-only scopes requires an explicit license
review.

## Redistribution and attribution

When redistributing Conduit API or a derivative, preserve the applicable
license texts and relevant copyright, patent, trademark, and attribution
notices. In particular:

1. Include `LICENSE`, `NOTICE`, `LICENSING.md`, `RELINKING.md`,
   `LICENSES/LGPL-3.0-only.txt`, and
   `LICENSES/RUST_THIRD_PARTY_LICENSES.html`.
2. If the web console is included, also include `frontend/NOTICE` and the
   build-generated `dist/licenses/frontend/THIRD_PARTY_LICENSES.md`.
3. Retain the relevant contents of `NOTICE` in a readable notice file,
   documentation, or an appropriate product notice display as required by
   Apache-2.0 section 4(d).
4. Mark modified Apache-licensed files with prominent change notices as
   required by Apache-2.0 section 4(b).
5. For distribution involving an LGPL-covered crate, satisfy LGPL-3.0's
   source, notice, reverse-engineering, and relinking requirements as they
   apply to the form of distribution. Rust builds commonly link these crates
   statically, so distributors should review the complete LGPL text rather
   than assuming dynamic-library rules apply.

Official GitHub Releases attach the exact tagged source as a separate asset and
place `SOURCE.md` plus `RELINKING.md` in each native binary archive.
Distributors of modified builds must provide corresponding source and
rebuilding/relinking materials for their own binaries rather than referring to
an unrelated official archive.

The production image places the license and relinking materials under
`/app/licenses`. The web build contains its applicable product and frontend
materials under `dist/licenses` so a standalone console distribution carries
the relevant notices. Vite generates
`dist/licenses/frontend/THIRD_PARTY_LICENSES.md` from the production modules
that are actually bundled; it is a build artifact and is not hand-maintained.
The committed Rust report is generated from the locked production graph with
`scripts/licenses/check-rust-third-party.sh --write`; CI reruns the generator
and rejects drift.

## Commercial use

Apache-2.0 and LGPL-3.0-only both permit commercial use, subject to their
conditions. Conduit API does not add a non-commercial restriction. A
non-commercial term would make the project source-available rather than
open-source under the commonly used OSI definition and would require a
separate license decision.

Network use by itself does not create an AGPL-style source-disclosure duty
under the licenses currently used here. Distribution can create obligations,
especially for LGPL-covered code.

This document is informational and is not legal advice.
