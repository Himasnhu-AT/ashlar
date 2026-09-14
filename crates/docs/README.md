# ashlar-docs

A documentation library for agents. A folder of markdown is the library: folders are
categories (nested folders are subcategories), `.md` files are documents, served verbatim.

- Each folder may hold `_category.md`: frontmatter `title` and `description`, and a body that
  is the category's overview. It describes the folder and is not listed as a document. The root
  `_category.md` says what the library is and how to use it.
- Every document starts with YAML frontmatter: `title`, `description` (when to read it), and
  optionally `order` (an integer, lower first) and `tags` (`[a, b]`).
- [`embed`] runs in a build script: it walks the folder and writes a `(path, include_str!)`
  table into `OUT_DIR`, with `rerun-if-changed` on every folder and file, so adding a page
  needs no list to update.
- [`Library::overlay_dir`] adds a folder at run time (a user's own pages) on top: same path,
  the folder's page wins, and listings say which source each page came from.
- Symbolic links, dotfiles and non-markdown files are skipped; run-time folders are capped in
  depth, file count and file size.

```rust,ignore
// build.rs
fn main() {
    let dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../docs");
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("docs.rs");
    ashlar_docs::embed(&dir, &out).unwrap();
}
// lib.rs
include!(concat!(env!("OUT_DIR"), "/docs.rs")); // pub static FILES: &[(&str, &str)]
let library = ashlar_docs::Library::from_files(FILES.iter().copied());
```
