# ashlar-diff

Headless text diffs for ashlar apps: which lines changed, which words in them, and how to show
them with context.

- `FileDiff::from_texts(old, new)`: a line diff (Myers' algorithm) cut into hunks with three
  lines of context. A cost cap bounds the time and memory of one run; past it the diff splits at
  lines unique to both sides, and as a last resort replaces the rest wholesale (`coarse` is then
  set). `from_texts_with` takes the context size, the cap, and whether to compute word emphasis.
- Word-level emphasis: in a run of removed lines followed by added lines, the n-th removed line
  pairs with the n-th added one, and `Line::emphasis` holds the byte ranges that differ.
  `word_diff(old, new)` exposes the same for any two strings.
- `FileDiff::gaps()`: the unchanged ranges between hunks, which a view shows collapsed. They are
  expandable (`source_line`) when the new text is known: always for `from_texts`, and for a
  parsed patch after `with_source(new_text)`.
- `parse_patch(text)`: unified patches and `git diff` output, one `FileDiff` per file, with
  renames, copies, mode changes, binary markers (and `GIT binary patch` data), quoted paths and
  `\ No newline at end of file`. Malformed input is a `PatchError` with a line number, never a
  panic; nothing is allocated from a number the patch states. `write_patch` writes `git diff`
  output back byte for byte.
- `diff_ops(a, b, max_cost)`: the underlying edit script for any `Eq + Hash` sequence.

No dependencies, no UI; builds for `wasm32-unknown-unknown`. The view is
`ashlar_ui::diff_view::DiffView`.

```rust
use ashlar_diff::{FileDiff, LineKind, parse_patch, write_patch};

let d = FileDiff::from_texts("a\nb\n", "a\nc\n").with_paths("f.txt", "f.txt");
assert_eq!(d.stats(), (1, 1));
let files = parse_patch(&d.to_patch()).unwrap();
assert_eq!(files[0].hunks[0].lines[1].kind, LineKind::Removed);
assert_eq!(write_patch(&files), d.to_patch());
```
