# Health Authority Reporting Validation System

> Merge the departments' dedicated regulatory reporting sheets into one template, click **Validate** once inside the original sheet, and four classes of problem — missing mandatory fields, out-of-range enums, unit mismatches, suspected duplicates — are flagged in place; after the preparer ticks each one to confirm, click **Submit** and the accepted rows are pushed to the hospital's regulatory gateway under an idempotency key, with the receipt written back into the sheet's status column.

## 1. What It Is and Who It Is For

`regval` is a validation engine plus an in-sheet plugin (Excel VBA / WPS JSA) for the dedicated regulatory reporting sheets that hospital departments fill in each reporting period. Departments keep working in the sheets they already own — no extra web portal, no separate dashboard, no system migration. The engine reads a merged sheet, evaluates it against versioned indicator rules, writes five new result columns back into that same sheet, and only pushes rows to the hospital's outbound regulatory gateway once a human has explicitly confirmed them. It is aimed at the hospital information department's data administrator who collects and checks departmental sheets, the quality-control officer who reviews suspected duplicates, and the department heads who need an audit trail of how a batch was submitted.

## 2. Core Capabilities

- **Works inside the original sheet.** Two buttons (Validate / Submit) and one status bar; one click writes the verdict into the five new columns on the right, and the status bar states how many rows were validated, how many rule issues were found, and how many suspected duplicates await confirmation.
- **Rules are versioned, and every finding cites its source.** A malformed rule never passes silently: the offending token and its position are reported, that one rule is marked inactive, and the rest keep running. A local rule-set revision does not take effect until the quality-control officer approves it, and a revision whose effective date has not arrived is not enabled either.
- **Duplicates are graded in four tiers and only flagged, never auto-merged.** Exact match → normalisation (full/half width, alias spellings) → edit-distance neighbours → model-based semantic grouping. Each tier reports its candidates and why they matched; everything is labelled "suspected", and merging is decided by the preparer's tick.
- **The model is optional at run time, and the fallback is recorded.** If the model endpoint is unreachable or times out, validation still runs and still produces results — the affected items are simply marked as coming only from the deterministic tiers, and every call is written to the local call-audit table. Pass `--require-model` to forbid that fallback entirely.
- **Submission never double-posts.** A repeated submit hits the idempotency key and issues no further request; rejected rows come back with an error code and field name; 5xx responses and timeouts are retried with backoff, and a row whose retries are exhausted moves to "pending retry" and onto the pending-submission list.
- **Only the gateway receipt advances a row's state.** Neither the model nor the local clock can move a row to "submitted". Reopening the same template next period shows only the rows that were not submitted, and who confirmed which row, when, and which receipt maps to which idempotency key can all be replayed per row and per batch — a damaged local state database can be restored to the last consistent state from the idempotency keys and receipt records.

## 3. Quick Start

```bash
cd regulatory-report-validate
export PATH="$HOME/.cargo/bin:$PATH"   # if cargo is not on PATH
cp .env.example .env                   # fill in the real in-house addresses (credentials are read from env vars only)
cargo build --release                  # binary at target/release/regval
```

```bash
cargo test                                              # unit + integration + contract tests
cargo run -- validate --input tests/fixtures/mini.csv --out /tmp/regval-result.json
cargo run --release -- doctor                          # check toolchain, state DB, rule directory, credentials
```

Expected first line of a successful run (the same sentence the in-sheet status bar shows):

```text
已校验 6 行 · 2 项口径问题 · 0 项疑似重复待确认 → /tmp/regval-result.json
行状态分布：待校验 2 行 · 校验通过 4 行
口径规则集：版本 2021.1 · 生效 11 条 · 指纹 8125c3aaa14219ba
```

To exercise the full submit path offline, point `REGULATORY_REPORT_BASE_URL` / `REGULATORY_REPORT_TOKEN` at the bundled protocol stub (`tests/gateway_stub/stub_server.py`, standard library only) and follow the four steps **validate → confirm each row → submit → export and check status**.

In the spreadsheet itself: import `macro/vba/RegVal.bas`, `macro/vba/RegValTokens.bas` and `macro/vba/RegValMessages.bas` into the workbook (all three modules are required), run `RegVal_InstallBar` with `Alt+F8`, and the **Validate** / **Submit** buttons appear on the right of the sheet. The original columns, formats and formulas are never modified — the engine only adds five columns.

## 4. Repository Layout

```
regulatory-report-validate/   # Rust engine, in-sheet macros, sample data, rule directory and docs
├── src/                      # engine source: ingest, rules, dupe, model gateway, push, state, report, ui
├── macro/                    # Excel VBA and WPS JSA plugin (all three entry points share one JSON contract)
├── rules/                    # versioned rule tables, indicator rule seeds, duplicate-detection thresholds
├── cases/                    # official-rule conformance case set (27 indicators, expected values live here only)
├── samples/                  # de-identified sample datasets (fixed seed, reproducible)
├── tests/                    # unit, integration, contract and scale-benchmark tests, plus the protocol stub
├── docs/                     # CLI reference, worked examples, architecture, benchmarks, concepts and screenshots
├── scripts/                  # offline build, packaging and screenshot capture scripts
├── Cargo.toml                # dependency manifest and binary name
└── README.md                 # full Chinese documentation
```

Full documentation: see README.md (Chinese).

---

## Follow Us

Scan the QR code to follow our official account for updates and community access:

![Follow Us](qrcode.jpg)