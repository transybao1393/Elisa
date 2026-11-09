# Coding tools

Part of the Elisa reference. Start at the [terminal README](../../elisa/README.md); all topics: [index](README.md).

## Code by syntax tree

`outline`, `read_symbol`, `edit_symbol` and the parse guard.

Elisa parses code with tree-sitter (`src/ast.rs`) for Rust, Python, Swift, TypeScript / TSX, JavaScript / JSX, SQL and
bash (`.rs .py .pyi .swift .ts .mts .cts .tsx .js .mjs .cjs .jsx .sql .sh .bash`).

- `outline path`: a file's symbols, one line each with its line range and signature (fns, methods, structs, enums,
  traits, impls, classes, protocols, extensions, interfaces, top-level constants), members indented under their type,
  and the imports on one line. A directory gives one line of names per file. If the outline is over the output cap,
  only the top-level symbols show, with a member count.
- `read_symbol path name [line]`: one symbol's source, line-numbered, by `name`, `Type::method` or `Class.method`.
  Either separator works in any language. Doc comments, attributes and decorators belong to the symbol. A name that
  matches several symbols returns the candidates, and `line` picks one. `Foo` means the type, not its impl or
  extension blocks.
- `edit_symbol path name source [action]`: replace the symbol (`action` = `replace`, the default), insert `source`
  before or after it, or `delete` it. `source` may come at any indentation; it takes the symbol's indentation. The
  result goes through the parse guard.
- **Parse guard**: a `write`, `edit` or `edit_symbol` on a supported file is re-parsed first. If the new text has
  ERROR or MISSING nodes that the old text did not (matched by what they are, not by line), Elisa writes nothing and
  returns `refused: …` with `line:col`, the source line and a caret. Files that were already broken stay editable. The
  guard runs before check-before-done, so a refused edit never triggers a check.
- The system prompt says to outline first, read symbols, and read whole files only when they are small. The tool list
  and prompt are fixed per session, so they stay byte-stable for KV reuse; with `ast = false` both leave out the three
  tools and that prompt line. The three tools add about 250 tokens to the cached system turn.
- `/context` shows `ast: outline + read_symbol N calls · X tokens vs Y for reading those files whole`. Each file
  counts once; a directory outline counts only on the outline side.
- To compare outline + symbol reads with whole-file reads on real code: `cargo test --offline --test ast_measure -- --ignored --nocapture`.
- RAM: one parser and one tree at a time, nothing cached, and files over 1 MiB are not parsed. Peak RSS over all
  177 files was 28.8 MB for the whole debug test process.
- Each grammar is a cargo feature (`lang-rust`, `lang-python`,
  `lang-swift`, `lang-typescript`, `lang-javascript`, `lang-sql`, `lang-bash`, all on by default). Rust + Python
  only is 9.4 MB.
- Licences: every added crate (tree-sitter, tree-sitter-language and the seven grammars) is MIT. The build-time
  crates `cc` and `streaming-iterator` are MIT OR Apache-2.0. Nothing copyleft. Listed in `../THIRD_PARTY_NOTICES.md`.

## Check before done

After a turn in which the model wrote or edited files, Elisa runs the project's own checks before it finishes, fast
ones first, then cheap tests. If a check fails, the trimmed errors go back to the model as a `[elisa] check failed …`
note appended to the history, so the KV prefix still matches. The model then gets up to `check_rounds` (2) fix rounds.
Each step shows in the transcript, e.g. `check: cargo clippy ✗ 3 errors → fixing (1/2)`. Elisa stops and reports when
the same errors come back, the rounds are used up, or a fix round ends without an edit. A timed-out check is reported
and skips the rest without a fix round.

Elisa picks checks from the languages of the edited files. Rust runs `cargo check`, then `cargo clippy -- -D warnings`,
then `cargo test -q` for the edited crate. Python runs `py_compile`, `ruff` if it is installed, then `pytest -x` on
edited test files. With `ast_targeted_checks` (on by default) the tests narrow to the symbols the turn touched. In
Rust, `cargo test -q -- <names>` runs when every touched symbol is a test or has tests named after it, for example
`split_words` runs `split_words_multibyte`. A new file, a change outside any symbol, or a symbol without a named test
runs the crate's whole tests. In Python, pytest also runs the test files that import a touched symbol's
module and name the symbol. `.elisa/checks.toml` commands are never changed. Swift runs `swift build`. JS/TS runs `tsc --noEmit` when the project has a `tsconfig.json`, and `npm test` when the nearest `package.json` has a real test script. Go runs `go vet ./...` and `go test ./...` per `go.mod`. Other languages need `commands` / `tests` in `.elisa/checks.toml`. Checks
run without asking, like a build you started yourself. Turn them off with `--no-checks`, config `checks = false`, or
`enabled = false` in `.elisa/checks.toml`. That file can also set `rounds`, `timeout_s` and `test_timeout_s`, and
`commands = [...]` / `tests = [...]` replace detection. Shade shows building or testing, then error or a "tests green"
celebration. `--serve-jsonl` sends a `check` event (`src/jsonl.rs`).
