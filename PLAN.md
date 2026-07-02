# ripe: a Yahoo Pipes clone in Rust + ratatui

A keyboard-driven terminal app for building data mashups by wiring modules into a
graph. Fetch feeds and APIs, pipe them through filters/sorts/joins, preview the
result at any node, save the pipe, and run it headless to emit RSS/JSON on a
schedule. Yahoo Pipes, reborn in the terminal.

Project name `ripe` matches this directory and reads as a backronym: **R**ust
**I**nternet **P**ipe **E**ngine. Rename later if something better lands.

---

## What we are cloning

Yahoo Pipes (2007-2015) was a visual dataflow editor. The mental model:

- A **pipe** is a directed acyclic graph (DAG) of **modules** wired together.
- Data on the wire is a **stream of items**: each item is a record (RSS-style
  fields: `title`, `link`, `description`, `pubDate`, `author`, plus arbitrary
  nested fields).
- **Sources** fetch data (RSS/Atom, JSON, CSV, scraped HTML). **Operators**
  transform the stream (filter, sort, truncate, unique, union, rename, regex).
  **Outputs** terminate the pipe. **Inputs** parameterize it.
- The editor showed a canvas of boxes connected by wires, an inspector for each
  module's settings, and a debugger that displayed the live output at any node.

We rebuild that as: (1) a TUI editor in `ratatui`, and (2) a headless runner so
saved pipes are useful from cron/scripts.

Note on the library: the user said "ratatouille"; the crate is **`ratatui`**
(pronounced like the dish, formerly `tui-rs`). Terminal backend is `crossterm`.

---

## Design mockup

`docs/mockup.png` (added 2026-07-02) is the visual target. What it fixes beyond
the original plan:

- **Layout:** top bar (menu labels, filename + dirty `*`, node count) —
  permanent **palette sidebar** (left, grouped NODES / SOURCES / SINKS) —
  **DAG canvas** (center, widest) — **preview** (right) — status line (mode +
  contextual keys). There is **no inspector panel**; params open as an overlay
  on `Enter`.
- **Canvas:** vertical top-down flow, numbered node badges, inline live status
  per node (`✓ 200 OK`, item counts, `✓ written`).
- **Preview:** tabs FEED / ITEMS (n) / RAW, auto-refresh toggle, "Rendered N
  items" footer.
- **Filters are expressions:** `item.pubDate > now - 2d`, `score > 100`.
- **Regex Extract:** named capture groups (`(?<score>\d+)`) become item fields.
- **Output node writes:** destination shown in-node (`temp://output.xml`,
  `✓ written`).
- File extension: `.pipe` (contents remain JSON).

Known mockup key conflicts: `q` is both insert-Unique and quit; `r` is both
insert-Regex and run. Resolution: inserts use a leader — `a` + palette letter —
so bare `r` runs and `q` quits.

---

## Scope

**MVP (must build):**
- Sources: Fetch Feed (RSS/Atom), Fetch JSON, Fetch CSV.
- Operators: Filter (expression rules), Sort, Limit, Tail, Unique, Reverse,
  Union, Transform (rename/copy/drop fields), Regex (replace + extract).
- Inputs: Text, URL, Number parameters.
- One Output node (format + destination: stdout or file path).
- DAG engine with caching, save/load, TUI editor, headless runner.

**Later (post-MVP):**
- Count (emits `Number` — deferred because nothing in the MVP can consume a
  scalar; see Decisions), Fetch Page (HTML scrape via CSS selectors), File
  source, generic HTTP Request source, HTTP POST sink, Merge strategies beyond
  union (zip/join), Sub-element extractor, String/Date/URL builders, Simple
  Math, Split.
- Mouse support, undo/redo, themes, command palette, canvas zoom.

**Stretch:**
- Import real Yahoo Pipes `.json` exports, plugin/module SDK, scheduling daemon,
  web/static export, Loop (sub-pipe per item).

---

## Tech stack

| Concern | Crate | Why |
|---|---|---|
| TUI | `ratatui` | the standard Rust TUI; immediate-mode widgets |
| Terminal backend | `crossterm` | cross-platform input, mouse, resize |
| Async runtime | `tokio` | concurrent fetches, async module eval |
| HTTP | `reqwest` | feeds + JSON + scraping fetches |
| Feed parsing | `feed-rs` | one parser for RSS 0.9x/2.0 + Atom + JSON Feed |
| Feed emit | `rss`, `atom_syndication` | `feed-rs` is read-only; these write |
| XML/HTML | `quick-xml`, `scraper` | scraping + odd XML sources |
| CSV | `csv` | CSV source/output |
| JSON path | `serde_json_path` | sub-element extraction |
| Regex | `regex` | filter `matches` op + Regex replace/extract module |
| Filter expressions | hand-rolled parser | grammar is tiny (`field op value`, `now ± dur`); no crate needed |
| Dates | `chrono` | parse `pubDate`, date modules, sorting |
| Data model | `serde`, `serde_json` | item representation + persistence |
| Graph algos | `petgraph` | topo sort, cycle detection |
| CLI | `clap` | headless runner args |
| Text input widgets | `tui-textarea` | editable fields in the inspector |
| Config/paths | `directories` | save dir, config dir |
| Logging | `tracing` + `tracing-appender` | TUI owns stdout, so log to a file |
| Errors | `anyhow` (bins), `thiserror` (lib) | ergonomic + typed |
| Test: HTTP mock | `wiremock` | deterministic source tests |
| Test: snapshots | `insta` | engine output + UI render snapshots |

Pin a Rust edition (2021 or 2024) and an MSRV in M0.

---

## Architecture

### Workspace layout

```
ripe/
  Cargo.toml                # [workspace]
  rust-toolchain.toml
  crates/
    ripe-core/              # data model, graph, engine, modules, fetchers (no UI)
      src/
        item.rs             # Item, PortValue, PortType
        graph.rs            # Node, Edge, Pipe, validation
        engine.rs           # topo eval, caching, async scheduling
        module/             # one file per module kind
          mod.rs            # Module trait + registry
          source_feed.rs
          op_filter.rs
          ...
        fetch.rs            # shared HTTP client, timeouts, caching
        persist.rs          # serde (de)serialization of Pipe
        format.rs           # emit RSS / Atom / JSON / CSV
    ripe-tui/               # ratatui editor (bin: `ripe`)
      src/
        app.rs              # App state (the Model)
        event.rs            # input -> Msg
        update.rs           # Msg -> state change
        ui/                 # render fns per pane
          canvas.rs         # node + wire rendering (custom widget)
          layout.rs         # layered auto-layout (topo order -> rows/columns)
          palette.rs        # left sidebar: NODES/SOURCES/SINKS + insert keys
          params.rs         # param-edit modal overlay (no inspector panel)
          preview.rs        # right panel: FEED / ITEMS / RAW tabs (reuses format.rs)
    ripe-cli/               # headless runner (bin: `ripe-run`)
      src/main.rs
```

Rationale: keeping `ripe-core` UI-free lets the engine be unit-tested and reused
by both the TUI and the runner. The TUI never does I/O directly; it calls core.

### Data model

```rust
// An item is a JSON object: a feed entry or generic record.
// Newtype, not a bare alias, so helpers (path lookup, coercion,
// canonical content-hash) can hang off it.
pub struct Item(pub serde_json::Map<String, serde_json::Value>);

// A value carried on a wire.
pub enum PortValue {
    Items(Vec<Item>),  // the common case: a feed/stream
    Text(String),
    Number(f64),
    Url(String),
    Bool(bool),
}

pub enum PortType { Items, Text, Number, Url, Bool, Any }
```

Items as JSON objects (not a fixed struct) matches Pipes' "any field" behavior and
makes JSON sources trivial. RSS fields are just conventional keys.

Two constraints the model must own:

- **Hashing for memoization.** `serde_json::Value` implements neither `Hash` nor a
  usable `Eq` (`f64`, NaN), so cache keys cannot hash values directly. Content-hash
  by serializing to canonical JSON (sorted keys) and hashing the bytes.
- **MVP wires carry `Items` only.** The scalar `PortValue` variants are reserved
  for later scalar wiring (Simple Math, Count consumers); no MVP module puts a
  scalar on a wire.

### Module contract

```rust
#[async_trait::async_trait]
pub trait Module: Send + Sync {
    fn kind(&self) -> &'static str;
    fn inputs(&self) -> &'static [PortSpec];   // name + PortType (+ variadic?)
    fn outputs(&self) -> &'static [PortSpec];
    fn param_schema(&self) -> ParamSchema;      // drives the inspector form
    async fn eval(&self, ctx: &EvalCtx, ins: Ins, params: &Params)
        -> anyhow::Result<Outs>;
}
```

A `ParamSchema` (typed field list: text/number/bool/enum/rule-list) is the single
source of truth: the inspector renders from it, persistence validates against it,
and the engine reads params through it. Build a `Registry` mapping `kind` to a
constructor so palette, loader, and engine share one list.

### Engine

- Validate graph is a DAG (`petgraph` cycle check) and ports are type-compatible.
- Topologically sort; evaluate nodes async.
- Run independent branches concurrently (`futures::stream::buffer_unordered` over
  a topo layering, or per-node `tokio::spawn` with dependency joins).
- **Memoize** each node's output by a cache key = hash of `(kind, params, upstream
  output hashes in input-port order)`. Order matters: sorting upstream hashes would
  conflate different wirings. Editing one node only reinvalidates its descendants.
  This makes live preview cheap and is the backbone of responsive editing.
- Sources have no upstreams, so `(kind, params)` alone would cache them forever:
  include a TTL bucket (fetch generation) in their key. Underneath, fetches honor
  HTTP caching (ETag/Last-Modified) to avoid hammering remotes during edits.
- **Partial-graph eval:** while editing, the graph is transiently incomplete. A
  node with unmet required inputs is marked `Unready` and skipped — per-node
  status, never a whole-pipe error.

### TUI architecture

The Elm Architecture (Model/Msg/update/view), which suits `ratatui`'s
immediate-mode redraw:

- **Model**: `App` (the pipe, selection, focused pane, viewport pan/zoom, dirty
  flag, per-node eval status/cache, modal state).
- **Msg**: keyboard/mouse/tick/resize plus async results (`EvalDone(NodeId,
  Result)`).
- **update**: pure-ish state transition; spawns async eval and returns; results
  arrive as `Msg` over an `mpsc` channel.
- **view**: render the focused layout from `App`.
- Event loop: `tokio::select!` over crossterm's `EventStream` (needs the
  `event-stream` feature), the async-result channel, and a tick timer (for
  spinners). Never block the UI thread on I/O.

---

## Milestones

Each milestone lists tasks and an acceptance bar. Ship in order; every milestone
should leave the tree compiling and tested.

### M0: Scaffolding and decisions
- [ ] `cargo new` workspace with the three crates above.
- [ ] Pin `rust-toolchain.toml` (edition 2024 + MSRV).
- [ ] Add `rustfmt.toml`, `clippy` config; wire `cargo fmt --check` + `clippy -D warnings`.
- [ ] Initialize git here (this dir is not its own repo yet), add `.gitignore`.
- [ ] `tracing` to a rotating file under the OS state dir; `RUST_LOG` honored.
- [ ] On-disk pipe format is JSON (decided); add a `version` field for migrations.
- **Done when:** empty workspace builds, lints clean, logs to file, CI green on push.

### M1: Core data model
- [ ] Define `Item`, `PortValue`, `PortType`, `PortSpec`.
- [ ] `Params` + `ParamSchema` (field kinds: Text, Number, Bool, Enum, FieldName, RuleList).
- [ ] Conversions/coercions between `PortValue` variants with clear errors.
- [ ] Unit tests for coercion and schema validation.
- **Done when:** model compiles, round-trips through serde, coercion tests pass.

### M2: Graph + engine (headless)
- [ ] `Node`, `Edge`, `Pipe` types with `NodeId`/`PortId` newtypes.
- [ ] Graph validation: DAG check, dangling-edge check, port type-compat check.
- [ ] Topo sort via `petgraph`.
- [ ] Async evaluator that walks the sorted graph and threads `PortValue`s.
- [ ] Output memoization keyed by `(kind, params, upstream hashes)`.
- [ ] Concurrency for independent branches.
- [ ] `EvalReport`: per-node status (Ok/Err/Cached/Unready), timing, item counts.
- [ ] Partial-graph eval: unmet required inputs -> `Unready`, skipped, no hard error.
- **Done when:** a hand-built 3-node pipe (passthrough source -> op -> output)
  evaluates correctly; cache hit avoids re-eval; cycle is rejected with a clear
  error; an incomplete pipe reports `Unready` nodes instead of failing.

### M3: Module library, sources
- [ ] `Module` trait + `Registry`.
- [ ] Shared `fetch.rs`: pooled `reqwest` client, timeouts, retry, HTTP cache, size cap.
- [ ] **Fetch Feed** (RSS/Atom/JSON Feed via `feed-rs` -> normalize to items).
- [ ] **Fetch JSON** (GET, optional `serde_json_path` to select the array).
- [ ] **Fetch CSV** (headers -> field names).
- [ ] Tests against `wiremock` fixtures (well-formed + malformed inputs).
- **Done when:** each source returns normalized items from a mocked endpoint and
  degrades gracefully on bad input.

### M4: Module library, operators
- [ ] **Filter**: rules are expression one-liners `field op value` — ops
      {`==`, `!=`, `>`, `<`, `>=`, `<=`, `contains`, `matches`}; values are string
      literals, numbers, or `now ± <n>[mhdw]` (relative dates). Permit/block,
      all/any combinator across rules.
- [ ] Expression parser: tiny hand-rolled grammar. Rules are stored as source
      text in the pipe file and parsed at load; parse failures are load-time /
      overlay validation errors. The grammar is frozen at comparisons +
      relative dates (see Risks).
- [ ] **Sort** (by field, asc/desc, numeric vs lexical vs date-aware).
- [ ] **Limit** (first N) and **Tail** (last N).
- [ ] **Unique** (dedupe by field).
- [ ] **Reverse**.
- [ ] **Union** (variadic Items inputs -> merged stream).
- [ ] **Transform** (copy/rename/drop fields).
- [ ] **Regex** (on a chosen field): replace mode + extract mode — named capture
      groups become new item fields (mockup: `(?<score>\d+)` -> `score`).
- [ ] Table-driven tests per operator, including expression parse errors.
- **Done when:** every MVP operator has passing tests including empty-input and
  missing-field edge cases.

### M5: Inputs + Output
- [ ] Pipe-level parameters: Text, URL, Number, with defaults. These are pipe
      metadata, **not canvas nodes** — no scalar wires in the MVP.
- [ ] Modules reference params via `${name}` interpolation in any param field;
      unknown names are a load-time validation error.
- [ ] **Output** node marks the terminal stream; params: format (rss/atom/json/
      csv) + destination (stdout or file path). The TUI's run command writes it
      and surfaces a `written` status; HTTP POST sink is post-MVP.
- **Done when:** a pipe with a URL parameter fetches a feed chosen at run time
  (`--param url=...` in the runner).

### M6: Persistence
- [ ] Serialize/deserialize `Pipe` (nodes, edges, params, meta, version — no
      positions; layout is computed, never persisted). Extension: `.pipe`.
- [ ] Schema-validate on load; tolerate unknown fields with a warning.
- [ ] Migration hook keyed off the version field.
- [ ] `insta` snapshot of a representative saved pipe.
- **Done when:** save then load reproduces an identical in-memory pipe (round-trip test).

### M7: Headless runner (`ripe-run`)
- [ ] `clap` CLI: `ripe-run -f <file.pipe> [-o <out>] [--format rss|atom|json|csv] [--param k=v ...]`.
- [ ] Flags: `-f/--pipe` = pipeline definition file (required, the input); `-o/--output`
      = destination (default stdout); `--format` defaults to `rss`. `-f` is the def, not "format".
- [ ] `format.rs`: emit RSS 2.0 (`rss`), Atom (`atom_syndication`), JSON, CSV.
- [ ] Non-zero exit + stderr diagnostics on eval failure.
- [ ] Integration test: fixture pipe -> expected RSS/JSON output.
- **Done when:** `ripe-run -f news.pipe` prints valid RSS to stdout (default format),
  and `-o out.xml` writes a file. This milestone alone makes ripe useful without any UI.

### M8: TUI skeleton
- [ ] App/Msg/update/view scaffolding; terminal init/teardown with panic-safe
      restore (raw mode off, leave alt-screen on crash).
- [ ] `tokio::select!` event loop (input + async results + tick).
- [ ] Layout per mockup: top bar (menu labels, filename + dirty marker, node
      count); **palette sidebar** (left, permanent, grouped NODES/SOURCES/SINKS
      with insert letters); **DAG canvas** (center, widest); **preview** (right);
      status line (mode + contextual keys). No inspector panel — params are a
      modal overlay (M11).
- [ ] Focus model + pane cycling; global quit/help.
- [ ] Minimum-size guard: below a floor (~80x24), render a "terminal too small"
      notice instead of a corrupted layout.
- **Done when:** app launches, panes render, resize is clean, quit restores the terminal.

### M9: Canvas rendering (auto-layout)
- [ ] **Auto-layout**: layered top-down placement computed from the graph
      (longest-path layering over the topo order; branch siblings side by side;
      stable sibling order so small edits don't reshuffle the picture). No
      manual positions; nothing persisted.
- [ ] Node boxes: numbered badge, icon + title, one detail line (param summary),
      one status line (eval state, item count, `✓ 200 OK` / `✓ written`).
- [ ] Wire routing between layers with box-drawing chars. With layout under our
      control this is verticals plus fan-out/fan-in elbows — not general
      Manhattan routing.
- [ ] Selection highlight; vertical scroll when the pipe overflows the panel;
      off-screen indicators.
- **Done when:** the 8-node mockup pipe renders recognizably close to
  `docs/mockup.png`; scroll works.

### M10: Editing interactions
- [ ] Insert node via `a` + palette letter; if a node or edge is selected,
      splice the new node after it / into it, else add it unwired.
- [ ] Delete node (and its edges); delete edge.
- [ ] Keyboard navigation: `j`/`k` walk the flow, `h`/`l` cross branches,
      number keys jump to a node's badge.
- [ ] **Connect**: select source node, press connect key, select target,
      validate type-compat, create edge; reject + explain on type mismatch or cycle.
- [ ] Dirty tracking + save/save-as/load; unsaved-changes guard on quit.
- **Done when:** the mockup pipe can be built from an empty canvas, saved, and
  reopened entirely from the keyboard.

### M11: Param editing (modal overlay)
- [ ] `Enter` on a node opens a centered modal overlay rendered from its
      `ParamSchema` (there is no inspector panel).
- [ ] Field widgets: text (`tui-textarea`), number, bool toggle, enum select,
      field-name picker, and an expression-list editor for Filter (one line per
      rule, parse-validated on entry).
- [ ] Edit -> update params -> invalidate that node's cache subtree.
- [ ] Inline validation messages (including expression parse errors).
- **Done when:** editing any MVP module's params via the overlay changes its output.

### M12: Live execution wiring
- [ ] Debounced async re-eval on edit; results delivered as `Msg`.
- [ ] Per-node loading/error/`Unready` states surfaced on the canvas.
- [ ] Cancel in-flight evals when params change again (supersede).
- [ ] Manual "run all" and "run to selected node" commands.
- **Done when:** editing a node re-evaluates its downstream without freezing the UI.

### M13: Preview panel (FEED / ITEMS / RAW)
- [ ] Three tabs over the Output node's stream: **ITEMS** — card list per item
      (title, domain, age, snippet), count in the tab label; **RAW** — the
      serialized feed, reusing `format.rs` from M7 so preview and `ripe-run`
      agree byte-for-byte; **FEED** — channel-level metadata (title, link,
      description, counts).
- [ ] Updates on every re-eval (wired in M12); scrollable; auto-refresh toggle;
      footer with "Rendered N items" + last eval time.
- [ ] On eval error, show the failing node and its message instead of stale output.
- [ ] Spinner while a source fetch is in flight.
- [ ] Later (optional): a debug mode that previews the *selected* node's stream as a
      table / JSON tree, for inspecting mid-pipe data Pipes-style.
- **Done when:** editing the pipe updates all three tabs without UI jank.

### M14: Polish
- [ ] Help overlay (keymap) and a status-line hint system.
- [ ] Command palette (fuzzy action search).
- [ ] Undo/redo (snapshot or command stack over graph mutations).
- [ ] Mouse support: click-select, drag-move, drag-to-connect.
- [ ] Theming (color config) + light/dark.
- [ ] Configurable keybindings.
- **Done when:** a new user can discover actions without reading the source.

### M15: Testing, CI, docs, release
- [ ] Engine + module unit tests; runner integration tests; UI render snapshots
      via `ratatui::TestBackend` + `insta`.
- [ ] GitHub Actions: fmt, clippy, test on stable; cache the cargo registry.
- [ ] README with screencast/asciinema, a tutorial pipe (the mockup's
      `news_pipeline.pipe`, checked in under `examples/`), and the module catalog.
- [ ] `cargo dist` or static musl builds for releases; publish `ripe-core` to crates.io if useful.
- **Done when:** `git clone && cargo run` gives a working editor; CI gates merges.

---

## Module catalog (target set)

| Module | Category | In | Out | Priority |
|---|---|---|---|---|
| Fetch Feed | source | Url | Items | MVP |
| Fetch JSON | source | Url | Items | MVP |
| Fetch CSV | source | Url | Items | MVP |
| Fetch Page (scrape) | source | Url | Items | later |
| File | source | - | Items | later |
| HTTP Request (generic) | source | Url | Items | later |
| Text/URL/Number param | input | - | scalar via `${name}` (not wired) | MVP |
| Filter | operator | Items | Items | MVP |
| Sort | operator | Items | Items | MVP |
| Limit / Tail | operator | Items | Items | MVP |
| Unique | operator | Items | Items | MVP |
| Reverse | operator | Items | Items | MVP |
| Count | operator | Items | Number | later |
| Union | operator | Items[] | Items | MVP |
| Merge (zip/join strategies) | operator | Items[] | Items | later |
| Transform (rename/copy/drop) | operator | Items | Items | MVP |
| Regex (replace / extract) | operator | Items | Items | MVP |
| Sub-element | operator | Items | Items | later |
| String/Date/URL builder | helper | scalars | scalar | later |
| Simple Math | helper | Number | Number | later |
| Split | operator | Items | Items, Items | later |
| Loop (sub-pipe per item) | operator | Items | Items | stretch |
| Output (format + stdout/file) | output | Items | - | MVP |
| HTTP POST sink | output | Items | - | later |

---

## Keybindings (draft, vim-flavored)

| Key | Action |
|---|---|
| `j k` / arrows | move selection along the flow |
| `h l` | move selection across branches |
| `1-9` | jump to node by badge number |
| `a` + palette letter | insert node (`a f` Fetch, `a t` Filter, `a r` Regex, `a m` Transform, `a s` Sort, `a u` Union, `a q` Unique, `a l` Limit, `a o` Output) |
| `d` | delete selected node |
| `c` | start connection from selected node |
| `x` | delete edge under cursor |
| `Enter` | open param overlay for selected node |
| `Tab` | cycle pane focus; `[ ]` switch preview tab |
| `r` | run all; `R` run to selected |
| `u` / `Ctrl-r` | undo / redo |
| `:` | command palette |
| `Ctrl-s` | save; `Ctrl-o` open |
| `?` | help overlay |
| `q` | quit (guard if dirty) |

---

## Testing strategy

- **Modules:** table-driven unit tests, including empty input, missing field, type
  coercion, and malformed source data.
- **Sources:** `wiremock` fixtures so no real network in tests; include broken XML/JSON.
- **Engine:** DAG validation, cycle rejection, topo order, cache hit/miss/invalidation.
- **Runner:** golden-file tests (fixture pipe -> expected RSS/JSON/CSV) with `insta`.
- **UI:** render against `ratatui::TestBackend`; snapshot buffers; simulate key
  sequences through `update` and assert resulting `App` state.
- **Determinism:** inject a clock and HTTP client so date/sort/fetch tests are stable.

---

## Risks and hard parts

1. **Layout stability, not wire routing.** Auto-layout (M9) shrinks routing to
   verticals plus fan-out/fan-in elbows and gives the canvas ~half the screen,
   retiring the original Manhattan-routing risk. The residual risk is
   *stability*: inserting or deleting a node must not reshuffle unrelated
   branches. Use a stable sibling order and snapshot-test layouts. Escape hatch
   remains a plain vertical list view.
2. **Connection UX from the keyboard.** Selecting a source port, then a target
   port, with type validation and clear rejection, needs a small state machine.
   Prototype the interaction in M10 with two dummy nodes first.
3. **Async eval without UI jank.** All I/O off the render thread; debounce edits;
   supersede stale evals. The memoization key design (M2) is what makes this
   tractable; get it right early.
4. **Item schema heterogeneity.** Field-name pickers must cope with items that do
   not share keys. Derive the union of keys seen in a sample for the inspector.
5. **Feed/output fidelity.** `feed-rs` reads many formats but does not write; RSS/
   Atom emission uses separate crates and needs round-trip tests.
6. **Expression grammar creep.** `field op value` plus `now ± duration` is small;
   a general expression language is not. The grammar is frozen at comparisons +
   relative dates for the MVP; anything more is post-MVP.

---

## Decisions (resolved)

- On-disk format: **JSON** (with a `version` field).
- Edition: **2024** (set MSRV alongside in M0).
- Runner flags: **`-f`** = pipeline definition (input), **`-o`** = output (default stdout),
  `--format` default `rss`.
- TUI layout (per `docs/mockup.png`): top bar / **palette sidebar** (left) /
  **DAG canvas** (center, widest) / **preview with FEED-ITEMS-RAW tabs** (right) /
  status line. No inspector panel; params edit in a modal overlay.
- Canvas: **auto-layout** — layered top-down, computed from the graph, never
  persisted. Manual node placement is out. Scroll only; zoom deferred.
- Filter rules: **expression one-liners** (`field op value`; values may be
  `now ± duration`), stored as text, parsed into the structured rule model.
- Pipe files: **`.pipe`** extension, JSON inside.
- Inputs: **pipe-level parameters** referenced as `${name}` in param fields — not
  canvas nodes; no scalar wires in the MVP.
- MVP wires carry **Items only**. Scalar wiring — and Count, which emits a `Number`
  nothing in the MVP can consume — is deferred.
- Cache keys: canonical-JSON content hashes; upstream hashes in input-port order;
  source keys include a TTL bucket.

## Open questions

- Yahoo Pipes `.json` import: worth it as a stretch, or is the format too dead to
  matter? (Archived exports exist; low priority.)
- Single binary with subcommands vs separate `ripe` / `ripe-run` binaries?
  Leaning separate, sharing `ripe-core`.

---

## First steps

1. M0 scaffolding (`cargo new` workspace, toolchain pin, lint+CI, tracing-to-file).
2. M1 + M2 in one push: model + engine, proven by a headless passthrough pipe test.
3. M3 Fetch Feed against a `wiremock` fixture; first real data through the engine.
4. Only then start the TUI (M8). Engine correctness before pixels.
