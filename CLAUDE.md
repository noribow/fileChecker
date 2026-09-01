# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

This repository is **pre-implementation**. It currently contains only a requirements
document (`docs/requirements.md`, written in Japanese) and no source code, build
configuration, or tests. There is nothing to build, lint, or test yet — do not invent
Cargo/npm/Tauri scaffolding or commands unless the user asks you to create them.

Before starting implementation work, read `docs/requirements.md` in full — it is the
single source of truth for scope and design intent for this project.

## What File Checker is

File Checker is a tool for verifying large file collections (photo/video archives,
external drives) against two problems:

- **Integrity check (整合性チェック)**: detect corrupted or missing files by
  recomputing hashes and comparing against a "reference set" (お手本セット) — a
  predefined list of expected filename/size/hash combinations — to reconstruct and
  validate whether a target folder's contents match what's expected.
- **Duplicate check (重複チェック)**: detect duplicate files across multiple
  user-specified folders and removable drives (external HDDs/USB drives). Removable
  media are scanned on connection and results are persisted, so subsequent duplicate
  comparisons can reuse a saved scan instead of requiring the media to be
  reconnected.

## Planned architecture (per requirements.md, subject to change)

- **Language/GUI framework**: Rust + Tauri, chosen for safe parallel processing and
  low-overhead binaries — non-functional requirements prioritize throughput and
  memory efficiency at scale (hundreds of thousands to millions of files, TB-scale
  data), with parallel I/O and parallel hashing assumed by design.
- **Shared core**: Integrity-check and duplicate-check logic is intended to live in a
  Rust crate shared by both the Tauri GUI and a CLI — most operations are exposed via
  GUI, with a subset also available from the CLI for scripting/CI use.
- **Reference set definition file**: JSON is the native format (the tool can scan a
  master folder and auto-generate one). An adapter mechanism is planned for reading
  external formats (CSV/XML, etc.) produced by other tools, with per-format field
  mapping.
- **Persistence**: Scan results (integrity results, duplicate results, per-removable-
  media check history) are stored in a local SQLite database. Removable media are
  identified so past scans/history can be matched up again without a rescan.
- **Platform tiers**: Windows is Tier-1 (primary support/validation target); macOS and
  Linux are Tier-2.
- **CLI output**: results to stdout plus exit codes reflecting
  success/failure/warning, for automation/CI use. **GUI output**: sortable/filterable
  result lists, with CSV/JSON/HTML export.

## Open questions (unresolved in requirements.md — check before assuming a design)

These are explicitly called out as undecided in the requirements doc; if a task
touches one of these areas, treat it as a design decision to raise, not something to
infer silently:

- CLI subcommand/option structure
- SQLite schema details (table structure, removable-media identification/history management)
- Field-mapping specifics for each external reference-set format (CSV/XML/etc.)
- GUI wireframes/screen flow
- Error-handling policy for inaccessible files, permission errors, I/O errors
- Fallback policy for removable-media identification when no stable/trustworthy identifier is
  available (the identification *mechanism* itself — a platform-abstracted, per-OS-swappable
  approach — is already decided, per requirements.md §10.4)
- Detailed spec for the reference-set-driven archive repack/write-out feature (trigger
  conditions, scope) — reading archive contents is already required (§3.3/§10.5); only the
  write-out/repack feature is undecided
