# `.3pk` Release Format — Specification v1

Plinth packs a release as a **`.3pk` manifest archive** plus **sibling
component archives** holding the heavy model files. The `.3pk` is the
portable source of truth for everything the catalog knows about a release:
raw STLs carry no metadata, so the manifest is what lets one user's
curation — names, poses, scale, supports, tags, and per-file pose
assignments — survive intact when another user scans the same release.

This document freezes **format version 1**. The manifest carries
`"version": 1`; readers reject unknown majors and ignore unknown fields
within a known major (forward-compatible).

## Physical layout

A packed release is a directory (the distributable unit):

```
Designer-05-2026-Dungeon Classics/
├── release.3pk              # zip: manifest.json + images/ + licence/
├── galeb duhr.zip           # component archive (one per group/model)
├── giant badger.zip
└── behir.zip
```

- **`release.3pk`** — a zip archive containing `manifest.json`, the
  release-level metadata jsons, preview images, and the creator's licence
  (packed under the canonical name `licence.<ext>` when the builder's
  licence toggle is on). Small; this is what a client fetches first to
  learn the release's shape.
- **Component archives** (`<component>.zip`, later `<component>.tar.zst`)
  — the model files (STL/OBJ/3MF/LYS/…) for one group/model. One archive
  per component so a client can fetch, verify, or update them
  independently (the Modular Package Strategy: update detection and
  selective import fall straight out of the per-component checksums —
  both shipped, see Import below).

The `.3pk` never embeds the heavy model files — only references them by
archive filename + checksum. A single-file distribution is a future
option; v1 is deliberately modular.

## `manifest.json` schema (v1)

```jsonc
{
  "format": "3pk",
  "version": 1,
  "generator": "plinth/<app-version>",

  "release": {
    "name": "Dungeon Classics",
    "designer": "Some Designer",
    "date": "2026-05", // YYYY-MM (canonical; input MM/YYYY is normalized)
    "version": "1.0.0",
    "description": "…",
    "tags": ["dungeon", "classic"],
    "images": ["images/cover.png"], // paths inside release.3pk
  },

  "components": [
    {
      "name": "galeb duhr", // the logical model / group
      "archive": "galeb duhr.zip", // sibling file, relative to the release dir
      "checksum": "blake3:9f86d0…", // of the archive bytes — drives update detection
      "size_bytes": 896812345,
      "dedup": true, // archive stores duplicate contents once (see Deduplication)

      "models": [
        {
          "id": "0e37…-uuid", // stable identity; survives moves/rescans
          "name": "galeb duhr", // scanner/base name
          "custom_name": null, // user override, if any (else null)
          "description": null,
          "group": "galeb duhr",
          "tags": ["earth-elemental"],

          "designer": "Some Studio", // the studio/brand (defaults from release, per-model override)
          "sculptor": "A. Artist", // the individual artist, if known

          "pose": "A", // model-level metadata (→ model_user_meta)
          "scale": "32mm",
          "support_status": "unsupported",
          "release_date": "2026-05",
          "preview": "images/galeb duhr A.png",

          "files": [
            // paths are relative to the component archive root
            {
              "name": "A/body.stl",
              "checksum": "blake3:…",
              "size_bytes": 1234,
              "pose": "A",
              "support_status": "unsupported",
            },
            {
              "name": "shared/base.stl",
              "checksum": "blake3:…",
              "size_bytes": 567,
              "pose": null,
              "support_status": null,
            },
          ],
        },
      ],
    },
  ],
}
```

### Field → catalog mapping

The manifest is the wire form of the catalog's rescan-safe tables. On
import the scanner restores:

| Manifest field                                                                       | Catalog destination                             |
| ------------------------------------------------------------------------------------ | ----------------------------------------------- |
| `models[].custom_name`, `pose`, `scale`, `support_status`, `release_date`, `preview` | `model_user_meta` (overrides scanner inference) |
| `models[].tags`                                                                      | `model_tags` (source `metadata`)                |
| `models[].files[].pose` / `support_status`                                           | `file_variants` (the dump-folder splits)        |
| `release.*`, `components[]`                                                          | `models.release_name/designer`, group identity  |

Because `file_variants` rides in the manifest, a folder someone split into
poses on their machine reappears already split on yours — the whole point
of making pose _metadata_ rather than folder structure.

### Checksums

- **Algorithm:** BLAKE3 (already shipped for duplicate detection), encoded
  `blake3:<hex>`.
- **Component `checksum`** (required): hash of the archive file's bytes.
  This is what update detection compares — a changed component is a
  changed hash, so a client re-imports only what moved.
- **File `checksum`** (recommended): hash of each model file's content,
  for granular integrity and cross-release dedup.

## Deduplication

A component archive MAY store each unique file content **once**. Creators
routinely repeat a base or body STL across weapon/pose variants; shipping
those bytes five times helps no one. The rules:

- The manifest lists **every** file name, each with its content checksum.
  Identical files simply share a checksum.
- The archive stores the bytes for each checksum at (at least) one of its
  names; other names with the same checksum may be absent from the archive.
- A component that elided anything sets `"dedup": true` (additive field;
  absent reads as false) so tooling knows plain unzipping is not enough.
- On extraction, a manifest name missing from the archive is
  **rematerialized** from an extracted file with the same checksum —
  hardlinked where the destination filesystem supports it (the release
  lands on disk already deduplicated), copied otherwise. Either way every
  listed name exists afterwards.

Non-dedup readers/writers interoperate: a v1 archive without elision is
just the degenerate case where every checksum is stored under every name.

## Signing (optional, additive)

A creator MAY sign a release. When a local signing key exists at pack
time, the packer writes `manifest.sig` — a detached JSON signature — next
to `manifest.json` inside `release.3pk`:

```jsonc
{
  "algo": "ed25519",
  "public_key": "<base64>",
  "key_fingerprint": "<first 16 hex chars of blake3(public_key)>",
  "signature": "<base64, over the exact manifest.json bytes as packed>",
}
```

The signature covers the raw bytes written to `manifest.json`, not the
parsed JSON value, so verification never depends on re-serializing
anything the same way the packer did. No key = no `manifest.sig`; every
reader, old or new, treats that as a normal unsigned pack — nothing about
`Manifest::is_readable` or the `version` field changes.

On inspect, a reader that finds `manifest.sig` verifies it against the
exact bytes read back for `manifest.json` and reports one of three
states:

- **Unsigned** — no `manifest.sig` entry. The common case; not an error.
- **Valid** — the signature checks out; the reader also gets the signer's
  `key_fingerprint` to display.
- **Invalid** — present but wrong (tampered manifest, wrong key, or a
  malformed `manifest.sig`). Reported, never silently dropped, but it
  does not block import — v1 has no key registry or trust pinning, so the
  user decides.

Signing keys are local-only in v1: one Ed25519 keypair generated on
demand from Settings per install, stored in the app data dir, never
uploaded anywhere. There is no CA, no revocation, and no cross-device
identity yet (see Out of scope below).

## Write path (packer)

At pack time the builder already stages models with their catalog
metadata. Packing then:

1. Groups staged models into components (by `group`, else per model).
2. Writes each component's model files into `<component>.zip`, hashing the
   archive → `checksum`, and each file → per-file `checksum`. Entries
   inside a component follow the canonical catalog layout
   (`Supported|Unsupported[/Variant]/files`, a `model.json` per leaf), so
   an imported release is already normal-form — the on-disk normalizer
   plans zero moves for it.
3. Emits `manifest.json` from the staged metadata **including
   `file_variants`** for any split folders, plus release-level info.
4. Zips `manifest.json` + release images + licence into `release.3pk`,
   signing `manifest.json`'s exact bytes into a sibling `manifest.sig`
   first when a local signing key exists (see Signing above).

Compression is ZIP in v1 (the only writer today); TAR+Zstd is a tracked
upgrade and only changes component `archive` extensions + the reader's
dispatch, not the manifest schema.

## Import (opening a `release.3pk`)

Opening or drag-dropping a `release.3pk` first **inspects** it — nothing
touches the disk until the user confirms:

1. Read `manifest.json`; reject unknown `version` majors.
2. Resolve the canonical landing spot (`Designer/YYYY-MM Release` under
   the library) and read the **local manifest** a previous import left
   there, if any.
3. Diff each incoming component's `checksum` against the local manifest:
   **new** (not imported before), **changed** (checksums differ),
   **unchanged** (identical), **packed** (the local copy is compressed at
   rest — unpack first), or **missing archive** (the sibling zip isn't
   next to the `.3pk`).
4. Classify `manifest.sig`, if present, against those same bytes (see
   Signing above) — surfaced to the import dialog, never blocking it.

The import dialog pre-selects new + changed components; the user can
deselect anything or re-select an unchanged component to repair deleted
files. The confirmed import then, per selected component:

1. Verifies the archive bytes against the manifest `checksum` — a
   truncated download or bit-rot is refused per component, the rest of
   the release still imports.
2. On an update, moves aside any file the user edited since the last
   import (bytes match neither the old nor the incoming checksum) as
   `<name> (edited).<ext>` — slicer-saved supports are never truncated.
3. Extracts, rematerializing dedup-elided names (see Deduplication).
4. On an update, deletes files the previous import wrote that the new
   manifest no longer lists, and sweeps emptied dirs. Files the user
   added themselves were never in a manifest and survive.

Finally the manifest is written into the release dir recording **what is
actually on disk**: new entries for components that imported, the
previous entry for ones that failed or were deselected. A partially
failed update therefore still reads as "changed" on the next inspect —
update detection stays truthful across partial runs. `manifest.sig`, when
present, lands in the release dir alongside it (part of the release-level
payload, like `release.json` and the images) so the provenance the import
verified stays with the local copy.

A catalog scan afterwards restores the packed curation from the
`model.json` sidecars. Legacy `release.json` / `model.json` sidecars
remain readable; the `.3pk` manifest supersedes them when both are
present.

## Versioning & compatibility

- `version` is a single integer. Same-major additions are additive fields
  (ignored by older readers); a breaking change bumps the major and
  readers refuse to guess.
- There is **no live release yet**, so v1 is defined freely here — no
  migration from a prior on-disk format is owed.

## Out of scope for v1 (tracked separately)

- Single self-contained `.3pk` container (v1 is modular).
- Partial download over a network (selective import of local components
  shipped; fetching only changed components from a host needs a
  distribution channel that doesn't exist yet).
- TAR+Zstd component compression (todo: replace ZIP for local
  compression/cataloging).
- Key registry / trust pinning for signed manifests (first-seen-key TOFU),
  and any CA or revocation story.
- OS keychain-backed private key storage (v1 keeps the signing key in the
  app data dir).
