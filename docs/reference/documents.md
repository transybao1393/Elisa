# Documents and the result cache

Part of the Elisa reference. Start at the [terminal README](../../elisa/README.md); all topics: [index](README.md).

## Attached documents

The app's 📎 button and drag and drop (and `--attach FILE` on the command line) take documents as well as images:
PDF, Word `.docx`, spreadsheets (xlsx / xlsm / xlsb / xls / ods / csv / tsv) and text or code files (txt, md, json,
yaml, source files …). Images need a model with vision; documents work with any model. Code: `src/attach.rs`.

- **Only the path travels.** The app sends each file's path with the message (`send.documents`, JSONL protocol v2).
  Elisa copies the file into the session's upload area, `<project>/.elisa/uploads/<session-id>/<name>`, at most
  50 MB a file (a larger one is refused with `not attached: report.pdf is too large to attach (63.2 MB; the limit is
  50 MB per file)` and the message goes on without it). The copy is inside the project root, so the documents server
  and `read` reach it, and nothing outside the project is opened again later. An upload folder that resolves outside
  the project (a symlinked `.elisa`) is refused.
- **Elisa reads it, not the model.** The text is extracted before the model is asked, with the documents server's own
  parsers for PDF and spreadsheets (`mcp_docs::pdf_pages` / `sheet_rows`), `word/document.xml` for .docx, and as-is for
  text. A scanned PDF with no text, a password-protected file or a binary format says why (no OCR).
- **Intake, before Enter.** The moment a file is attached the app sends `prefetch`; Elisa checks it in the background
  (activity "Checking report.pdf…", then a `doc_info` event; cached by file content) and nothing is ever run:
  - *inspect*: the type from its magic bytes (not the name), size, pages / sheets and rows, words and tokens, language
    (en / vi), whether a PDF has a text layer;
  - *safety*: a password-protected PDF or Office file is refused; PDF JavaScript, launch actions or embedded files and
    Office macros (`vbaProject.bin`, .docm / .xlsm) are a warning ("⚠ has macros (not run; text only)") and only the
    text is read; zip bombs (over 400 MB unpacked or 20,000 entries) are refused before a parser opens them; the
    extraction has a 30 s limit; a name that does not match the content ("named .pdf but it is a text file") warns;
  - *category*, from cheap signals, no model call: paper, report, invoice, contract, cv, statement, slides, book, data,
    code, notes, other;
  - *structure*: sections with page ranges (the PDF outline when it has one, else numbered / named headings), tables;
  - *fit*: whole, overview or selection against the window's document budget, with the token numbers;
  - *readable parts* for the person, along sections or about 4–6 pages each: "show part 3" / "summarise part 2" answer
    from those pages.

  The app's chip shows the one-line result ("38 pages · research paper · 13 sections · fits as overview"); a click
  opens a panel with the details, warnings and parts, and clicking a part sends "Summarise part 2 (pages 4–6) of
  report.pdf". A terminal prints the line (`report.pdf: 38 pages · …`).
- **Token saving: Elisa picks the text, not the model.** The document budget is about 45% of the window minus the
  conversation so far (at least 600–2,000 tokens). A document that fits goes into the turn whole, as `[Attached:
  name.pdf, 2 pages]` and its text, answered in one call with no tools. A larger one:
  - a vague request ("read this one for me", "summarise", no text) gets an **overview**: title, abstract, every heading
    with its first sentences, the conclusion, references and running headers left out — `[Attached: name.pdf, 38
    pages; overview below, ask about any part for detail]`;
  - a specific question gets **the pages that answer it**: pages, sections or parts it names first, then BM25 over the
    pages, blended with MiniLM embeddings once they are cached (`cache` kind `docs`, keyed by the file's hash) —
    `[Attached: name.pdf, 38 pages; pages 4, 9 below, picked for this question; …]`;
  - **the text never stays in the conversation.** The history, the saved session and fold notes keep only the
    reference line `[Attached: name.pdf, 38 pages · .elisa/uploads/<session>/name.pdf]`; the overview or pages are
    added to that turn's requests only (like the situation and follow-up hints). Every later turn about the document
    ("what about the risk team?", "continue", "and the Sharpe ratio for GOOGL?", "back to the paper") gets the pages
    it needs picked again, the last question's words joined to short or "and …" questions, within the budget left
    by the (now small) history; two turns on another topic and it stays out until it is named again. So folds stay
    rare and a fold never loses the paper: the notes keep its reference and the next question re-reads it;
  - **resuming** (`--continue`, `/resume`, "continue the last one", the dev reload) attaches every document the
    loaded history or notes name again from the upload area (intake from the cache), so the next question gets its
    pages; a copy that is gone gets "ta.pdf is no longer in this folder's uploads; attach it again". An older
    session's inline text is replaced by its reference; one folded before references were kept gets its upload back
    when the conversation names its title or file name;
  - **tables**: a result table that PDF extraction flattens into rows of bare numbers is written out with its group
    and column names ("TradingAgents — AAPL: CR% 26.62, ARR% 30.5, SR 8.21, MDD% 0.91; GOOGL: …"), the abbreviations
    the document defines named once ("SR = Sharpe Ratio");
  - the block names the full file's upload path, so the docs tools can still read any other page; they are not
    offered by default.
  - **Safety net**: a reply that says it cannot read the document (and called no tool) is withdrawn
    (`reply_retracted`), and the turn is asked once more with the text and a line saying it is right there, shown as
    an activity step "Giving the model the text of report.pdf again".
- **Outside text.** The extracted text has secrets hidden like tool output (config `redact`) and is fenced as
  `<untrusted source="attached name.pdf">`. Memory and fold notes keep only the `[Attached: …]` line, never the text.
- **Activity.** Reading shows as an activity step ("Reading report.pdf…" → "Read report.pdf · 12 pages").

## Result cache

Elisa keeps its own cache of results that are slow to make again, so a repeated question is quick on **any** model
backend: Ollama's KV prefix reuse is a server feature that llama.cpp, a raw GGUF and most other servers do not have,
and Elisa never relies on it. The cache lives in `~/.elisa/cache` (`src/cache.rs`) and holds results only — never a
model's answer or a chat reply.

| Kind | Key | Kept | Not cached |
|---|---|---|---|
| `web_fetch` | the normalised URL (scheme and host lower case, no fragment, no default port) and the size cap | 1 h | errors; a page sent with `Cache-Control: no-store` |
| `web_search` | engine (and SearXNG server) and the query (case and spacing folded); never the API key | 15 min | errors |
| `docs` | the file's content (sha256) and the extractor version: PDF pages, every sheet of a workbook, a .docx's text | until the file changes | CSV / TSV (cheap to parse) |

Never cached: anything that holds a secret (`src/secrets.rs` finds one: the result is not stored at all), local file
reads by `read` (cheap, and files change), model output. An entry is content-addressed (sha256 of kind, version and
key); its file holds the key, creation time and TTL, its mtime is its last use and its length its size. Past
`cache_max_mb` (200 MB) the least recently used entries go. `cache = false` switches it off.

**Several Elisa processes at once** (two app windows, the app and a terminal, each session's own MCP servers) share
it safely: an entry is written to a temp file and renamed into place, so a reader (who takes no lock) never sees half
an entry; writers take an flock on `~/.elisa/cache/lock` around rename, eviction and clearing (the memory store's
approach). A corrupt entry is ignored and removed. **In-flight dedup:** while one process or thread fetches a URL or
parses a document, another asking for the same key waits for it (per-key flock in `inflight/`, up to 30 s for web,
120 s for documents) and takes its result, so two windows asking for the same page make one request; if the first
leaves nothing (an error, `no-store`), the waiter does the work itself.

**Prefetch.** A file attached in the app is parsed into the cache in the background as soon as it is attached
(`prefetch` line, src/jsonl.rs → `cache::prefetch_docs`), so pressing Enter does not wait for the parse.
`cache::prefetch_url` does the same for a page the caller has already decided to read. `cache::docs_text(path)` is the
cached text of a PDF, spreadsheet or .docx for other code.

**Seeing it.** `/cache` lists each kind's entries, size and this session's hit rate (the session's MCP servers count
theirs in `~/.elisa/cache/stats/`); `/stats` ends with the hits and misses; with `ELISA_DEBUG=1` a hit shows as an
info line (`[cache hit: web_fetch]`). Also listed, and kept as they are: the MCP tool lists (`~/.elisa/mcp-tools.json`),
the sandbox dependency volumes (`elisa-deps-<language>` in Colima's VM) and the memory store's hot vectors and
embedding vocabulary.

**Privacy.** The cache directories are mode 0700 and every file 0600. Fetched page text and the text of your
documents are stored locally only, on this Mac, in plain text under `~/.elisa/cache` — the same machine the pages
were read on and the documents came from; nothing is sent anywhere. `/cache clear` removes it all.
