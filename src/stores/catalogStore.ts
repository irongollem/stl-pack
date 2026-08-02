import { confirm } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { acceptHMRUpdate, defineStore } from "pinia";
import { computed, ref, watch } from "vue";
import {
  type CatalogEntry,
  type CatalogFile,
  type CatalogGroup,
  type CatalogRootSummary,
  type CatalogStats,
  type DeleteSummary,
  type DesignerCount,
  type DuplicateGroup,
  type GroupOrigin,
  type ModelFileGeometry,
  type NormalizePlan,
  type RenderCandidate,
  type TagCount,
  commands,
} from "../bindings";
import { useBatchRender } from "../composables/useBatchRender";
import { useCatalogJobs } from "../composables/useCatalogJobs";
import { useFileSelect } from "../composables/useFileSelect";
import { usePackStatus } from "../composables/usePackStatus";
import { formatFileSize } from "../utils/format";
import { useReleasesStore } from "./releasesStore";
import { useToastStore } from "./toastStore";

const PAGE_SIZE = 60;
const orNull = (value: string) => value.trim() || null;
// Base sizes are canonical dimension strings: "25" for regular bases,
// "60x35" for ovals/rectangles — bare numbers, unit implied. Junk (units,
// words, zeros) parses to null rather than storing garbage. Mirrors the
// Rust boundary's canonical_mm.
const mmOrNull = (value: string) => {
  const parts = value.trim().toLowerCase().replace(/×/g, "x").split("x");
  const nums = parts.map((p: string) => Number.parseInt(p.trim(), 10));
  if (nums.some((n: number) => !Number.isFinite(n) || n <= 0)) return null;
  if (nums.length === 1) return String(nums[0]);
  if (nums.length === 2) return `${nums[0]}x${nums[1]}`;
  return null;
};

/* ---- designer › release sections, derived from the backend's order ---- */
type ReleaseBucket = {
  key: string;
  label: string | null; // null = no release header (flat mode)
  value: string | null; // raw metadata value; fallbacks such as "No release" aren't renameable
  date: string | null;
  groups: CatalogGroup[];
};
type DesignerSection = {
  key: string;
  designer: string | null; // null = no designer header (flat mode)
  designerValue: string | null;
  releases: ReleaseBucket[];
};

/* Pure, state-free helpers — kept at module scope so they're defined once
   rather than recreated on every store instantiation. */

// Merged (hardlinked) copies cost the disk nothing, so reclaimable space
// counts distinct physical copies — a fully shared group contributes 0.
// Packed copies aren't loose bytes either (they occupy compressed archive
// space and can't be merged/deleted), so they don't count as reclaimable.
const reclaimableBytes = (g: DuplicateGroup) => {
  const looseCopies = g.distinct_copies - (g.packed_paths?.length ?? 0);
  return g.size_bytes * Math.max(0, looseCopies - 1);
};

const sectionModelCount = (section: DesignerSection) =>
  section.releases.reduce((count, bucket) => count + bucket.groups.length, 0);

// A folder split into poses yields several members sharing one dir_path;
// their variant_key disambiguates. Fall back to dir_path for whole-folder
// members (variant_key null).
const memberKey = (entry: CatalogEntry) => entry.variant_key ?? entry.dir_path;

const basename = (path: string) => path.split(/[\\/]/).pop() ?? path;

/* "match" normalization: underscores and spaces count as the same
   separator, so bulk filing matches on facets regardless of the creator's
   naming convention. */
const normalizeForMatch = (value: string) =>
  value.toLowerCase().replace(/[_\s]+/g, " ");
const escapeRegExp = (value: string) =>
  value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

const tabLabel = (tab: string) => (tab === "" ? "other" : tab);
const variantLabel = (variant: string) => variant || "base";

const groupSummary = (group: CatalogGroup) => {
  const parts: string[] = [];
  if (group.pose_count > 1) parts.push(`${group.pose_count} poses`);
  if (group.support_statuses.length)
    parts.push(group.support_statuses.join(" / "));
  return parts.join(" · ");
};

const originLabel = (o: GroupOrigin) =>
  `${o.designer ?? "unknown designer"} / ${o.release_name ?? "unknown release"} (${o.model_count} model${o.model_count === 1 ? "" : "s"})`;

const opLabel = (from: string, to: string) => {
  // show the shared prefix only once — the interesting part is what changes
  const fromParts = from.split(/[/\\]/);
  const toParts = to.split(/[/\\]/);
  let shared = 0;
  while (
    shared < fromParts.length - 1 &&
    shared < toParts.length - 1 &&
    fromParts[shared] === toParts[shared]
  ) {
    shared++;
  }
  return `${fromParts.slice(shared).join("/")} → ${toParts.slice(shared).join("/")}`;
};

// packed_paths is additive in the bindings (older payloads omit it)
const packedIn = (group: DuplicateGroup) => group.packed_paths ?? [];

export const useCatalogStore = defineStore("catalog", () => {
  const toastStore = useToastStore();
  const releasesStore = useReleasesStore();
  const { selectDirectory, selectFiles } = useFileSelect();
  const {
    isScanning,
    scanProgress,
    scanError,
    scanCompletedCount,
    startScan,
    cancelScan,
    isFindingDuplicates,
    dupProgress,
    dupCompletedCount,
    startDuplicateScan,
    cancelDuplicateScan,
    isMiningGeometry,
    geoProgress,
    geoSummary,
    geoCompletedCount,
    startGeometryScan,
    cancelGeometryScan,
  } = useCatalogJobs();
  const {
    isPacking,
    packProgress,
    packError,
    packSummary,
    packCancelled,
    lastAction,
    packFinishedCount,
    startPack,
    startUnpack,
    cancelPack,
  } = usePackStatus();

  // One label for banners/progress: extraction rides the same event stream
  // as pack/unpack jobs and must not read as "Packing…" during a print
  const packJobLabel = computed(() => {
    if (lastAction.value === "unpack") return "Unpacking";
    if (lastAction.value === "extract") return "Extracting";
    return "Packing";
  });

  const {
    isBatchRendering,
    batchProgress,
    batchSummary,
    batchError,
    batchCancelled,
    batchFinishedCount,
    modelFinishedCount,
    modelErrors,
    startBatch,
    cancelBatch,
  } = useBatchRender();

  /* Catalog folders: the index is fed one folder at a time (huge collections
     scan incrementally), so the toolbar manages a LIST — each entry with its
     own footprint, staleness, and scan button. */
  const roots = ref<CatalogRootSummary[]>([]);
  const hasRoots = computed(() => roots.value.length > 0);
  const refreshRoots = async () => {
    const result = await commands.listCatalogRoots();
    if (result.status === "ok") roots.value = result.data;
  };
  /* Roots queued behind the currently running scan — folders scan strictly
     one at a time so the NAS isn't hit with parallel walks. */
  const scanQueue = ref<string[]>([]);

  const query = ref("");
  const viewMode = ref<"list" | "grid">("grid");
  const selectedTags = ref<string[]>([]);
  const allTags = ref<TagCount[]>([]);

  /* Ordering/grouping: flat A–Z by model name, or grouped designer › release
     with releases alphabetical or newest-first. The backend sorts (grouping
     must hold across pages); the view only draws headers where the designer
     or release changes between consecutive rows. */
  type GroupMode = "none" | "designer" | "designer-date";
  const SORT_FOR_MODE: Record<GroupMode, string> = {
    none: "name",
    designer: "designer",
    "designer-date": "designer_date",
  };
  const storedGroupMode = localStorage.getItem("catalogGroupMode");
  const groupMode = ref<GroupMode>(
    storedGroupMode === "designer" || storedGroupMode === "designer-date"
      ? storedGroupMode
      : "none",
  );
  watch(groupMode, (mode) => localStorage.setItem("catalogGroupMode", mode));
  // exact-match facet on top of the fuzzy text search; "" = all designers
  const designerFilter = ref("");
  // Mirrors the backend-owned, session-only mature-content access state.
  // Search commands consult the same state themselves; this ref is only for
  // frontend presentation and refreshing when Settings changes it.
  const showNsfw = ref(false);
  const designers = ref<DesignerCount[]>([]);
  // the browsable units: one group per logical model
  const groups = ref<CatalogGroup[]>([]);
  const total = ref(0);
  const stats = ref<CatalogStats | null>(null);
  // drill-down state: group -> its variant entries -> the active one
  const selectedGroup = ref<CatalogGroup | null>(null);
  const drawerLoadError = ref<string | null>(null);
  const members = ref<CatalogEntry[]>([]);
  const activeSupport = ref("");
  // second navigation tier: within a support build, which variant is shown
  const activeVariant = ref("");
  const selected = ref<CatalogEntry | null>(null);
  const files = ref<CatalogFile[]>([]);
  // Mined per-file geometry (issue #15) for the selected member's dir_path —
  // empty until a geometry scan has mined that folder's files.
  const modelGeometry = ref<ModelFileGeometry[]>([]);
  const newTag = ref("");
  const dupGroups = ref<DuplicateGroup[]>([]);
  const showDups = ref(false);
  const show3d = ref(false);
  // per-group hash -> path the user wants to keep (defaults to the first)
  const keepChoice = ref<Record<string, string>>({});
  const reclaimBusy = ref(false);
  // group names ticked for a batch move or combine
  const checkedGroups = ref<string[]>([]);
  const combining = ref(false);
  const combineName = ref("");
  const renamingGroup = ref(false);
  const groupNameDraft = ref("");
  const show3dModal = ref(false);
  const showImageModal = ref(false);
  const metaDraft = ref({
    name: "",
    variant: "",
    pose: "",
    scale: "",
    support_status: "",
    release_date: "",
    designer: "",
    sculptor: "",
    release_name: "",
    base_round_mm: "",
    base_square_mm: "",
  });

  // A synthesized member's variant_key is `dir\u{1f}variant\u{1f}pose`; keep the
  // format in one place. Empty variant AND pose is the residual/unassigned pool.
  const KEY_SEP = "\u{1f}";
  const variantKeyFor = (dir: string, variant: string, pose: string) =>
    `${dir}${KEY_SEP}${variant}${KEY_SEP}${pose}`;

  /* Resizable detail drawer — width persists so it survives navigation. */
  const DRAWER_MIN = 300;
  const DRAWER_MAX = 720;
  const drawerWidth = ref(
    Math.min(
      DRAWER_MAX,
      Math.max(
        DRAWER_MIN,
        Number(localStorage.getItem("catalogDrawerWidth")) || 420,
      ),
    ),
  );
  const startDrawerResize = (event: MouseEvent) => {
    const startX = event.clientX;
    const startWidth = drawerWidth.value;
    const onMove = (moveEvent: MouseEvent) => {
      // the drawer sits on the right, so dragging left widens it
      const delta = startX - moveEvent.clientX;
      drawerWidth.value = Math.min(
        DRAWER_MAX,
        Math.max(DRAWER_MIN, startWidth + delta),
      );
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      localStorage.setItem("catalogDrawerWidth", String(drawerWidth.value));
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const visibleTags = computed(() => {
    const top = allTags.value.slice(0, 12);
    // keep selected tags visible even when they fall outside the top list
    for (const tag of selectedTags.value) {
      if (!top.some((t) => t.tag === tag)) {
        const known = allTags.value.find((t) => t.tag === tag);
        top.push(known ?? { tag, count: 0 });
      }
    }
    return top;
  });

  const stlPaths = computed(() =>
    files.value.filter((f) => f.extension === "stl").map((f) => f.path),
  );

  // Aggregate line for the drawer's geometry section — tallest file, total
  // resin volume, total triangle budget across every mined file.
  const geometryTallestMm = computed(() =>
    modelGeometry.value.reduce((max, f) => Math.max(max, f.z_mm), 0),
  );
  const geometryTotalVolumeMl = computed(
    () => modelGeometry.value.reduce((sum, f) => sum + f.volume_mm3, 0) / 1000,
  );
  const geometryTotalTriCount = computed(() =>
    modelGeometry.value.reduce((sum, f) => sum + f.tri_count, 0),
  );

  const wastedBytes = computed(() =>
    dupGroups.value.reduce((sum, g) => sum + reclaimableBytes(g), 0),
  );

  // Groups whose names still occupy more than one copy; fully shared groups
  // stay visible in the panel (as "shared") but out of the headline count
  const reclaimableGroups = computed(() =>
    dupGroups.value.filter((g) => g.distinct_copies > 1),
  );

  const lastScanLabel = computed(() => {
    if (!stats.value?.last_scan_epoch) return null;
    return new Date(stats.value.last_scan_epoch * 1000).toLocaleString();
  });

  const runSearch = async (append = false) => {
    const offset = append ? groups.value.length : 0;
    const result = await commands.searchCatalogGroups(
      query.value,
      selectedTags.value,
      designerFilter.value || null,
      SORT_FOR_MODE[groupMode.value],
      PAGE_SIZE,
      offset,
    );
    if (result.status === "ok") {
      groups.value = append
        ? [...groups.value, ...result.data.groups]
        : result.data.groups;
      total.value = result.data.total;
      // keep the drawer header's aggregates fresh (poses/sizes may change)
      if (selectedGroup.value) {
        const current = selectedGroup.value.group_name.toLowerCase();
        const fresh = groups.value.find(
          (g) => g.group_name.toLowerCase() === current,
        );
        if (fresh) selectedGroup.value = fresh;
      }
    } else {
      toastStore.reportError("Search failed", result.error);
    }
  };

  const loadMore = () => runSearch(true);

  let searchTimeout: number | null = null;
  watch([query, selectedTags, designerFilter, groupMode], () => {
    if (searchTimeout) clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => runSearch(), 250) as unknown as number;
  });

  // One pass over the loaded page(s): a new header opens whenever the
  // designer or release changes between consecutive rows — safe because the
  // backend sorts by exactly (designer, release, name). Flat mode is the
  // same structure with a single headerless section, so the template never
  // branches on the mode.
  const sections = computed<DesignerSection[]>(() => {
    if (groupMode.value === "none") {
      return [
        {
          key: "all",
          designer: null,
          designerValue: null,
          releases: [
            {
              key: "all",
              label: null,
              value: null,
              date: null,
              groups: groups.value,
            },
          ],
        },
      ];
    }
    const out: DesignerSection[] = [];
    for (const group of groups.value) {
      const designerValue = group.designer?.trim() || null;
      const designer = designerValue || "Unknown designer";
      let section = out[out.length - 1];
      // compare case-insensitively, matching the backend's NOCASE ordering
      if (section?.designer?.toLowerCase() !== designer.toLowerCase()) {
        section = {
          key: designer.toLowerCase(),
          designer,
          designerValue,
          releases: [],
        };
        out.push(section);
      }
      const releaseValue = group.release_name?.trim() || null;
      const label = releaseValue || "No release";
      let bucket = section.releases[section.releases.length - 1];
      if (bucket?.label?.toLowerCase() !== label.toLowerCase()) {
        bucket = {
          key: `${section.key}\u{1f}${label.toLowerCase()}`,
          label,
          value: releaseValue,
          date: group.release_date,
          groups: [],
        };
        section.releases.push(bucket);
      }
      bucket.groups.push(group);
    }
    return out;
  });

  const refreshMeta = async () => {
    const [tagsResult, statsResult, dupResult, designerResult] =
      await Promise.all([
        commands.getCatalogTags(),
        commands.getCatalogStats(),
        commands.getDuplicateGroups(),
        commands.getCatalogDesigners(),
      ]);
    if (tagsResult.status === "ok") allTags.value = tagsResult.data;
    if (statsResult.status === "ok") stats.value = statsResult.data;
    if (dupResult.status === "ok") dupGroups.value = dupResult.data;
    if (designerResult.status === "ok") designers.value = designerResult.data;
  };

  /** Facet headers are metadata, not folder names. Rename the metadata in one
   * transaction; the existing Clean up flow then offers the corresponding
   * canonical folder moves, keeping potentially large disk changes explicit. */
  const renameDesignerFacet = async (oldName: string, newName: string) => {
    const next = newName.trim();
    if (!next || next === oldName) return next === oldName;
    const confirmed = await confirm(
      `Rename designer “${oldName}” to “${next}” across every assigned model?\n\nThis updates their metadata now. Use Clean up afterward to make their folders follow the new name.`,
      { title: "Rename designer", kind: "warning" },
    );
    if (!confirmed) return false;
    const result = await commands.renameCatalogDesigner(oldName, next);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to rename designer", result.error);
      return false;
    }
    if (designerFilter.value.toLowerCase() === oldName.toLowerCase()) {
      designerFilter.value = next;
    }
    await Promise.all([runSearch(), refreshMeta()]);
    if (selectedGroup.value) await selectGroup(selectedGroup.value);
    toastStore.addToast(`Designer renamed to “${next}”`, "success");
    return true;
  };

  const renameReleaseFacet = async (
    designer: string,
    oldName: string,
    newName: string,
  ) => {
    const next = newName.trim();
    if (!next || next === oldName) return next === oldName;
    const confirmed = await confirm(
      `Rename release “${oldName}” to “${next}” for ${designer}?\n\nThis updates every matching model's metadata now. Use Clean up afterward to make their folders follow the new name.`,
      { title: "Rename release", kind: "warning" },
    );
    if (!confirmed) return false;
    const result = await commands.renameCatalogRelease(designer, oldName, next);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to rename release", result.error);
      return false;
    }
    await Promise.all([runSearch(), refreshMeta()]);
    if (selectedGroup.value) await selectGroup(selectedGroup.value);
    toastStore.addToast(`Release renamed to “${next}”`, "success");
    return true;
  };

  const toggleTag = (tag: string) => {
    selectedTags.value = selectedTags.value.includes(tag)
      ? selectedTags.value.filter((t) => t !== tag)
      : [...selectedTags.value, tag];
  };

  const toggleDups = () => {
    if (reclaimableGroups.value.length) showDups.value = !showDups.value;
  };

  // 3D preview is opt-in PER MEMBER. Leaving it latched meant every pose or
  // model click immediately parsed multi-million-triangle STLs on the main
  // thread — browsing became a chain of UI freezes with no way to turn the
  // viewer off mid-load. Selection changes drop back to the image; the user
  // re-opens 3D deliberately. (Moving the parse into a Worker is tracked in
  // the todolist; this removes the accidental triggers.)
  watch(selected, (next, prev) => {
    const nextKey = next ? memberKey(next) : null;
    const prevKey = prev ? memberKey(prev) : null;
    if (nextKey !== prevKey) close3d();
  });

  /* On a packed member the viewer's STLs are extracted first (the viewport
     readFile()s real paths); closing the viewer takes the copies back. */
  const viewer3dBusy = ref(false);
  const viewerExtracted = ref<string[]>([]);

  const toggle3d = async () => {
    if (show3d.value) {
      close3d();
      return;
    }
    if (selected.value?.packed) {
      // extraction takes real time on a big model — if the user has moved to
      // another member meanwhile, opening the viewer would show the NEW
      // selection with the OLD selection's files attributed to it
      const key = memberKey(selected.value);
      viewer3dBusy.value = true;
      const extracted = await ensureLoose(stlPaths.value);
      viewer3dBusy.value = false;
      if (extracted === null) return;
      if (!selected.value || memberKey(selected.value) !== key) {
        if (extracted.length) cleanupEphemeralSafe(extracted);
        return;
      }
      viewerExtracted.value = extracted;
    }
    show3d.value = true;
  };

  const close3d = () => {
    show3d.value = false;
    show3dModal.value = false;
    if (viewerExtracted.value.length && packCleanupAfter.value) {
      // the viewport holds the geometry in memory; the files can go
      cleanupEphemeralSafe(viewerExtracted.value);
    }
    viewerExtracted.value = [];
  };

  // Selection requests overlap easily when browsing quickly or switching
  // support/pose tabs. Epochs make late IPC responses harmless instead of
  // letting an old card overwrite the current drawer.
  let groupSelectionEpoch = 0;
  let entrySelectionEpoch = 0;

  const selectEntry = async (entry: CatalogEntry) => {
    const epoch = ++entrySelectionEpoch;
    const entryKey = memberKey(entry);
    selected.value = entry;
    files.value = [];
    modelGeometry.value = [];
    // A synthesized pose member carries a variant_key; pass it so we list
    // only that pose's files. Whole-folder members send null (all files).
    // Geometry is mined per dir_path (no variant_key split), so it's fetched
    // alongside the file list even though it's the same call across poses.
    const [fileResult, variantResult, geometryResult] = await Promise.all([
      commands.getCatalogModelFiles(entry.dir_path, entry.variant_key),
      commands.getFileVariants(entry.dir_path),
      commands.getModelGeometry(entry.dir_path),
    ]);
    if (
      epoch !== entrySelectionEpoch ||
      !selected.value ||
      memberKey(selected.value) !== entryKey
    ) {
      return;
    }
    if (fileResult.status === "ok") files.value = fileResult.data;
    if (variantResult.status === "ok") {
      const map: Record<string, string> = {};
      for (const v of variantResult.data) {
        const label = [v.variant, v.pose].filter(Boolean).join(" · ");
        if (label) map[v.path] = label;
      }
      fileVariantMap.value = map;
    }
    if (geometryResult.status === "ok")
      modelGeometry.value = geometryResult.data;
  };

  /* ---- assign files in a dump folder to variant/pose buckets ---- */
  // checked file paths in the drawer's file list, and the facets to file them under
  const checkedFiles = ref<string[]>([]);
  const variantAssignDraft = ref("");
  const poseAssignDraft = ref("");
  // path -> "variant · pose" label, so already-sorted files show a badge
  const fileVariantMap = ref<Record<string, string>>({});

  const toggleCheckedFile = (path: string) => {
    checkedFiles.value = checkedFiles.value.includes(path)
      ? checkedFiles.value.filter((p) => p !== path)
      : [...checkedFiles.value, path];
  };

  /* "match": tick every file whose name carries the typed facets, so bulk
     filing is type -> match -> file instead of a hundred checkbox clicks.
     The pose token demands word boundaries so pose "a" doesn't match every
     file with an 'a' in it. */

  const selectMatchingFiles = () => {
    const variant = normalizeForMatch(variantAssignDraft.value.trim());
    const pose = normalizeForMatch(poseAssignDraft.value.trim());
    if (!variant && !pose) return;
    const poseRe = pose
      ? new RegExp(`(^|[^a-z0-9])${escapeRegExp(pose)}([^a-z0-9]|$)`)
      : null;
    const matches = files.value
      .filter((file) => {
        const name = normalizeForMatch(file.file_name);
        return (
          (!variant || name.includes(variant)) && (!poseRe || poseRe.test(name))
        );
      })
      .map((file) => file.path);
    checkedFiles.value = matches;
    if (!matches.length) {
      toastStore.addToast("No file names match those facets", "info");
    }
  };

  /** Reload the open group's members and select a sensible one — used after a
   *  split changes the member set. Prefers `preferKey` when it still exists. */
  const reloadMembers = async (preferKey?: string) => {
    const group = selectedGroup.value;
    if (!group) return;
    const result = await commands.getCatalogGroupMembers(group.group_name);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to reload variants", result.error);
      return;
    }
    members.value = result.data;
    const firstTab = supportTabs.value[0] ?? "";
    const next =
      (preferKey
        ? members.value.find((m) => memberKey(m) === preferKey)
        : undefined) ??
      members.value.find((m) => (m.support_status ?? "") === firstTab) ??
      members.value[0];
    // move the support + variant tiers to wherever we landed, so it's visible
    activeSupport.value = next?.support_status ?? firstTab;
    activeVariant.value = resolveVariant(next?.variant ?? "");
    if (next) await selectEntry(next);
  };

  const assignChecked = async () => {
    const dir = selected.value?.dir_path;
    const variant = variantAssignDraft.value.trim();
    const pose = poseAssignDraft.value.trim();
    // need at least one facet to file under, and files to file
    if (!dir || (!variant && !pose) || !checkedFiles.value.length) return;
    const count = checkedFiles.value.length;
    const result = await commands.assignFilesToPose(
      checkedFiles.value,
      variant || null,
      pose || null,
      null,
    );
    if (result.status !== "ok") {
      toastStore.reportError("Failed to assign files", result.error);
      return;
    }
    const label = [variant, pose].filter(Boolean).join(" · ");
    toastStore.addToast(
      `Filed ${count} file${count === 1 ? "" : "s"} under "${label}"`,
      "success",
    );
    checkedFiles.value = [];
    // the variant sticks for the next round — filing five poses of one
    // spear type means retyping only the pose letter
    poseAssignDraft.value = "";
    // Stay on the unassigned pool so the remaining files are still in front of
    // you to keep filing. When the last file is filed the pool is gone and
    // reloadMembers falls back to a real member.
    await Promise.all([runSearch(), reloadMembers(variantKeyFor(dir, "", ""))]);
  };

  const clearChecked = async () => {
    if (!checkedFiles.value.length) return;
    const result = await commands.clearFilePose(checkedFiles.value);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to clear assignment", result.error);
      return;
    }
    // 0 = the selection was never filed to a pose — a success toast here
    // would claim an effect that didn't happen (files that LIVE in another
    // folder can't be unfiled out of this model; that's split or move)
    if (result.data === 0) {
      toastStore.addToast(
        "Nothing to unfile — these files aren't assigned to a pose",
        "info",
      );
      return;
    }
    toastStore.addToast(
      `Unfiled ${result.data} file assignment${result.data === 1 ? "" : "s"}`,
      "success",
    );
    checkedFiles.value = [];
    await Promise.all([runSearch(), reloadMembers()]);
  };

  // Support statuses present among the members, stable order; "" = untagged
  const supportTabs = computed(() => {
    const seen = new Set(members.value.map((m) => m.support_status ?? ""));
    const ordered = ["supported", "unsupported"].filter((s) => seen.has(s));
    for (const status of seen) {
      if (!ordered.includes(status)) ordered.push(status);
    }
    return ordered;
  });

  // members in the active support build (used to derive the variant tier)
  const supportMembers = computed(() =>
    members.value.filter(
      (m) => (m.support_status ?? "") === activeSupport.value,
    ),
  );

  // distinct variants within the active support build, in the backend's
  // bucket order; "" = no variant. Only shown when there's more than one.
  // Case-insensitive on purpose: new writes are Title Cased by convention,
  // but legacy members may still carry "sword" beside "Sword" until their
  // metadata is re-saved — one chip, not two.
  const variantsInTab = computed(() => {
    const seen: string[] = [];
    for (const member of supportMembers.value) {
      const variant = member.variant ?? "";
      if (!seen.some((v) => v.toLowerCase() === variant.toLowerCase())) {
        seen.push(variant);
      }
    }
    return seen;
  });

  // Something for "dump into one box" to undo: the card carries more than one
  // member (poses/variants/fanned files), or any member wears a scanner-guessed
  // variant/pose. A single plain folder has nothing to flatten.
  const hasAutoSplit = computed(
    () =>
      members.value.length > 1 ||
      members.value.some(
        (m) => (m.variant ?? "") !== "" || (m.pose ?? "") !== "",
      ),
  );

  // the pose members within the active (support, variant) bucket
  const tabMembers = computed(() =>
    supportMembers.value.filter(
      (m) =>
        (m.variant ?? "").toLowerCase() === activeVariant.value.toLowerCase(),
    ),
  );

  // pick a variant present in the active support build, preferring `prefer`
  // (case-insensitively — the chip's spelling wins over the member's)
  const resolveVariant = (prefer: string) =>
    variantsInTab.value.find((v) => v.toLowerCase() === prefer.toLowerCase()) ??
    variantsInTab.value[0] ??
    "";

  const setSupportTab = (tab: string) => {
    // keep the pose/variant when hopping between builds — you're looking at the
    // same mini, just the other build of it
    const currentPose = selected.value?.pose ?? null;
    const currentVariant = selected.value?.variant ?? "";
    activeSupport.value = tab;
    activeVariant.value = resolveVariant(currentVariant);
    const next =
      (currentPose
        ? tabMembers.value.find((m) => m.pose === currentPose)
        : undefined) ?? tabMembers.value[0];
    if (next) selectEntry(next);
  };

  const setVariant = (variant: string) => {
    const currentPose = selected.value?.pose ?? null;
    activeVariant.value = variant;
    const next =
      (currentPose
        ? tabMembers.value.find((m) => m.pose === currentPose)
        : undefined) ?? tabMembers.value[0];
    if (next) selectEntry(next);
  };

  // The scanner-level groups behind the selected card; more than one means
  // it was combined and offers "split" in the drawer
  const groupSources = ref<string[]>([]);

  const selectGroup = async (group: CatalogGroup) => {
    const epoch = ++groupSelectionEpoch;
    // Any file request belonging to the previous group is stale now, even
    // before this group's first member has been chosen.
    entrySelectionEpoch += 1;
    selectedGroup.value = group;
    drawerLoadError.value = null;
    renamingGroup.value = false;
    members.value = [];
    selected.value = null;
    files.value = [];
    groupSources.value = [];
    commands.getCatalogGroupSources(group.group_name).then((sources) => {
      if (
        epoch === groupSelectionEpoch &&
        selectedGroup.value?.group_name === group.group_name &&
        sources.status === "ok"
      ) {
        groupSources.value = sources.data;
      }
    });
    checkStructure(group.group_name);
    const result = await commands.getCatalogGroupMembers(group.group_name);
    if (
      epoch !== groupSelectionEpoch ||
      selectedGroup.value?.group_name !== group.group_name
    ) {
      return;
    }
    if (result.status !== "ok") {
      toastStore.reportError("Failed to load model variants", result.error);
      drawerLoadError.value = "Couldn’t load this model’s variants.";
      return;
    }
    if (!result.data.length) {
      drawerLoadError.value =
        "This model no longer has any variants available to browse.";
      return;
    }
    members.value = result.data;
    const firstTab = supportTabs.value[0] ?? "";
    activeSupport.value = firstTab;
    const first =
      members.value.find((m) => (m.support_status ?? "") === firstTab) ??
      members.value[0];
    // through resolveVariant so the active chip carries the CHIP's spelling
    activeVariant.value = resolveVariant(first?.variant ?? "");
    if (first) await selectEntry(first);
  };

  const startRenameGroup = () => {
    groupNameDraft.value = selectedGroup.value?.group_name ?? "";
    renamingGroup.value = true;
  };

  // The selected variant's image becomes the group card's face — stored as
  // WHICH member, so a re-render of that member updates the card too
  const useAsCardImage = async () => {
    const group = selectedGroup.value;
    const entry = selected.value;
    if (!group || !entry) return;
    const result = await commands.setGroupCover(
      group.group_name,
      entry.dir_path,
      entry.variant_key ?? null,
    );
    if (result.status !== "ok") {
      toastStore.reportError("Failed to set card image", result.error);
      return;
    }
    toastStore.addToast("Card image updated", "success");
    await runSearch();
  };

  // Surgical combine-undo: pull ONE mis-combined model back out of this card
  // (one checkbox too many happens); the rest of the combination stays
  const detachSelectedSource = async () => {
    const group = selectedGroup.value;
    const source = selected.value?.source_group;
    if (!group || !source) return;
    const confirmed = await confirm(
      `Remove "${source}" from "${group.group_name}"?\n\nIt comes back as its own model; nothing on disk moves.`,
      { title: "Remove from model", kind: "warning" },
    );
    if (!confirmed) return;
    const result = await commands.detachCatalogGroupSource(
      group.group_name,
      source,
    );
    if (result.status !== "ok") {
      toastStore.reportError("Failed to remove from model", result.error);
      return;
    }
    toastStore.addToast(`"${source}" is its own model again`, "success");
    await Promise.all([runSearch(), refreshMeta()]);
    // the card still exists (other sources remain) — reload it in place
    await selectGroup(group);
  };

  // group_renames has no root/designer scoping (scanner-derived group names
  // are bare strings), so a generic name like "Spear" can already silently
  // span more than one designer/release before the user ever touches it.
  // Check each name a rename/combine is about to reach and let the user bail
  // if that turns out to be a surprise, rather than merging it invisibly.
  const confirmRenameAmbiguity = async (
    names: string[],
    actionLabel: string,
  ) => {
    const results = await Promise.all(
      names.map(async (name) => ({
        name,
        origins: await commands.getGroupRenameOrigins(name),
      })),
    );
    const ambiguous = results.filter(
      (r) => r.origins.status === "ok" && r.origins.data.length > 1,
    );
    if (!ambiguous.length) return true;
    const detail = ambiguous
      .map(({ name, origins }) =>
        origins.status === "ok"
          ? `"${name}" also includes:\n${origins.data.map((o) => `  · ${originLabel(o)}`).join("\n")}`
          : "",
      )
      .join("\n\n");
    return confirm(
      `This name is shared with models from other designers/releases:\n\n${detail}\n\n${actionLabel} anyway?`,
      { title: "Ambiguous group name", kind: "warning" },
    );
  };

  // Undo for combine (and for a rename collision that merged two models):
  // clearing the name overrides brings every source group back as its own
  // card, named after its folder again. Nothing on disk moves.
  const splitGroup = async () => {
    const group = selectedGroup.value;
    if (!group || groupSources.value.length < 2) return;
    const confirmed = await confirm(
      `Split "${group.group_name}" back into ${groupSources.value.length} separate models?\n\n${groupSources.value.join("\n")}`,
      { title: "Split model", kind: "warning" },
    );
    if (!confirmed) return;
    const result = await commands.renameCatalogGroup(group.group_name, "");
    if (result.status !== "ok") {
      toastStore.reportError("Failed to split model", result.error);
      return;
    }
    toastStore.addToast(
      `Split into ${groupSources.value.length} models`,
      "success",
    );
    selectedGroup.value = null;
    selected.value = null;
    members.value = [];
    groupSources.value = [];
    await Promise.all([runSearch(), refreshMeta()]);
  };

  // Escape hatch for a wrong auto-config: drop the scanner's guessed
  // variant/pose on every member and every per-file pose assignment, so the
  // card collapses to one flat file list to re-file by hand with the
  // assignment bar. Supported/unsupported builds stay split — those are real,
  // not a guess. Nothing on disk moves; the clear survives rescans.
  const isFlattening = ref(false);
  const flattenGroup = async () => {
    const group = selectedGroup.value;
    if (!group || isFlattening.value) return;
    const confirmed = await confirm(
      `Dump every file in "${group.group_name}" into one box?\n\nThe auto-detected variant and pose tags are cleared across the whole model and won't come back on a rescan — you re-file the files yourself. Nothing on disk moves.`,
      { title: "Reset to one box", kind: "warning" },
    );
    if (!confirmed) return;
    isFlattening.value = true;
    try {
      const result = await commands.flattenCatalogGroup(group.group_name);
      if (result.status !== "ok") {
        toastStore.reportError("Failed to reset the model", result.error);
        return;
      }
      toastStore.addToast(
        result.data
          ? `Reset to one box · ${result.data} assignment${result.data === 1 ? "" : "s"} cleared`
          : "Reset to one box",
        "success",
      );
      checkedFiles.value = [];
      await Promise.all([runSearch(), reloadMembers()]);
    } finally {
      isFlattening.value = false;
    }
  };

  const renameGroup = async () => {
    const group = selectedGroup.value;
    renamingGroup.value = false;
    if (!group) return;
    const newName = groupNameDraft.value.trim();
    if (newName === group.group_name) return;
    if (
      newName &&
      !(await confirmRenameAmbiguity([group.group_name], "Rename"))
    ) {
      return;
    }
    const result = await commands.renameCatalogGroup(group.group_name, newName);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to rename model", result.error);
      return;
    }
    toastStore.addToast(
      newName ? `Renamed to "${newName}"` : "Name reset to the folder name",
      "success",
    );
    await Promise.all([runSearch(), refreshMeta()]);
    const found = newName
      ? groups.value.find(
          (g) => g.group_name.toLowerCase() === newName.toLowerCase(),
        )
      : undefined;
    if (found) {
      await selectGroup(found);
    } else {
      selectedGroup.value = null;
      selected.value = null;
      members.value = [];
    }
  };

  // Tags apply to the whole group: a tag describes the mini, so tagging the
  // supported and unsupported builds separately was busywork that drifted
  const addTag = async () => {
    const group = selectedGroup.value;
    if (!group || !selected.value || !newTag.value.trim()) return;
    const result = await commands.addGroupTag(group.group_name, newTag.value);
    if (result.status === "ok") {
      newTag.value = "";
      await refreshSelected();
      await refreshMeta();
    } else {
      toastStore.reportError("Failed to add tag", result.error);
    }
  };

  const removeTag = async (tag: string) => {
    const group = selectedGroup.value;
    if (!group || !selected.value) return;
    const result = await commands.removeGroupTag(group.group_name, tag);
    if (result.status === "ok") {
      await refreshSelected();
      await refreshMeta();
    } else {
      toastStore.reportError("Failed to remove tag", result.error);
    }
  };

  /** Re-fetch the group's members so tag/detail edits show up immediately. */
  const refreshSelected = async () => {
    const group = selectedGroup.value;
    const key = selected.value ? memberKey(selected.value) : undefined;
    await runSearch();
    if (!group) return;
    const result = await commands.getCatalogGroupMembers(group.group_name);
    if (result.status !== "ok") return;
    members.value = result.data;
    if (!members.value.length) {
      selectedGroup.value = null;
      selected.value = null;
      files.value = [];
      show3d.value = false;
      return;
    }
    const updated = key
      ? members.value.find((m) => memberKey(m) === key)
      : undefined;
    selected.value = updated ?? members.value[0] ?? null;
  };

  /* ---- transparent use: materialize packed bytes just-in-time ---- */
  // Resolves when every path is readable on disk. Returns the ephemeral
  // extracts (what cleanup may take back afterwards), or null on failure.
  // Progress/cancel ride the same PackStatus stream as pack jobs.
  const ensureLoose = async (paths: string[]): Promise<string[] | null> => {
    if (!paths.length) return [];
    const result = await commands.ensureModelFiles(paths);
    if (result.status !== "ok") {
      toastStore.reportError(
        "Failed to extract from the pack archive",
        result.error,
      );
      return null;
    }
    return result.data.extracted;
  };

  // The user's cleanup-after preference, mirrored from settings; the print
  // modal's checkbox writes it back. Default true — "print straight from the
  // bundle" is the point of packing.
  const packCleanupAfter = ref(true);
  const initPackCleanupPref = async () => {
    const result = await commands.getSettings();
    if (result.status === "ok") {
      packCleanupAfter.value = result.data.pack_cleanup_after ?? true;
    }
  };
  const persistCleanupAfter = async () => {
    const result = await commands.getSettings();
    if (result.status !== "ok") return;
    await commands.setSettings({
      ...result.data,
      pack_cleanup_after: packCleanupAfter.value,
    });
  };

  // Paths a handed-off render still depends on: Blender reads them from disk
  // in the Render tab for minutes, so print/3D cleanups of the same member
  // must not touch them. Held until app exit (the exit sweep owns them) —
  // there is no render-finished signal in this tab.
  const renderHeldPaths = ref<Set<string>>(new Set());

  // Every cleanup in this view goes through here so render-held paths are
  // exempt in ONE place instead of at each call site
  const cleanupEphemeralSafe = async (paths: string[]) => {
    const safe = paths.filter((p) => !renderHeldPaths.value.has(p));
    if (!safe.length) return;
    const result = await commands.cleanupEphemeralFiles(safe);
    if (result.status === "ok") {
      for (const kept of result.data.errors) toastStore.addToast(kept, "info");
    }
  };

  // Deleting the copies right after openWithDefaultApp returns would race the
  // slicer's own read. The delay covers a normal open; a file the slicer still
  // holds (Windows) refuses deletion and stays registered for the exit sweep,
  // and the size+mtime guard keeps anything the slicer saved over.
  const SLICER_CLEANUP_DELAY_MS = 15_000;
  const scheduleSlicerCleanup = (paths: string[]) => {
    setTimeout(() => {
      cleanupEphemeralSafe(paths);
    }, SLICER_CLEANUP_DELAY_MS);
  };

  /* ---- bulk pack: compress everything under the current scope ---- */
  // Scope order: explicit card selection > the designer facet > the whole
  // catalog. The backend job is sequential and per-folder atomic, so a
  // cancelled designer-wide run resumes by clicking Pack… again.
  const bulkPack = async (groupNames: string[] = []) => {
    if (isPacking.value) return;
    const candidates = await commands.getPackCandidates(
      groupNames.length ? null : designerFilter.value || null,
      groupNames,
    );
    if (candidates.status !== "ok") {
      toastStore.reportError(
        "Failed to list pack candidates",
        candidates.error,
      );
      return;
    }
    const dirs = candidates.data;
    if (!dirs.length) {
      toastStore.addToast(
        "Nothing to pack — everything in scope is already packed",
        "info",
      );
      return;
    }
    const scope = groupNames.length
      ? `${groupNames.length} selected model${groupNames.length === 1 ? "" : "s"}`
      : designerFilter.value
        ? `every ${designerFilter.value} model`
        : "the whole catalog";
    const confirmed = await confirm(
      `Compress ${dirs.length} folder${dirs.length === 1 ? "" : "s"} — ${scope} — into pack archives?\n\n` +
        "Runs one folder at a time and is safe to cancel: finished folders stay packed, and re-running Pack… resumes where it left off.",
      { title: "Pack models", kind: "info" },
    );
    if (!confirmed) return;
    const result = await startPack(dirs);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to start packing", result.error);
    }
  };

  // "60.2x35.1x88.7" → "60.2 × 35.1 × 88.7 mm · 3 parts" (machine facts the
  // render pipeline measured; absent until the model was rendered once)
  const measuredLabel = computed(() => {
    const entry = selected.value;
    if (!entry?.dims_mm) return null;
    const dims = entry.dims_mm.split("x").join(" × ");
    const parts = entry.part_count
      ? ` · ${entry.part_count} part${entry.part_count === "1" ? "" : "s"}`
      : "";
    return `${dims} mm${parts}`;
  });

  /* ---- batch preview rendering: one Blender launch for the whole scope ---- */
  // Scope resolution mirrors bulkPack: explicit card selection > designer
  // facet > whole catalog. Candidates come back whole so the modal can do the
  // missing/existing/packed math without another round trip.
  const showBatchRender = ref(false);
  const batchCandidates = ref<RenderCandidate[]>([]);
  const batchRerenderExisting = ref(false);
  const batchLoading = ref(false);

  const batchMissing = computed(() =>
    batchCandidates.value.filter((c) => !c.has_preview && !c.packed),
  );
  const batchExisting = computed(() =>
    batchCandidates.value.filter((c) => c.has_preview && !c.packed),
  );
  const batchPackedSkipped = computed(() =>
    batchCandidates.value.filter((c) => c.packed),
  );

  const openBatchRender = async (groupNames: string[] = []) => {
    if (isBatchRendering.value) return;
    batchLoading.value = true;
    showBatchRender.value = true;
    batchRerenderExisting.value = false;
    batchCandidates.value = [];
    const result = await commands.getRenderCandidates(
      groupNames.length ? null : designerFilter.value || null,
      groupNames,
    );
    batchLoading.value = false;
    if (result.status !== "ok") {
      showBatchRender.value = false;
      toastStore.reportError("Failed to list render candidates", result.error);
      return;
    }
    batchCandidates.value = result.data;
  };

  const startBatchRender = async () => {
    const chosen = batchRerenderExisting.value
      ? [...batchMissing.value, ...batchExisting.value]
      : batchMissing.value;
    if (!chosen.length) return;
    const result = await startBatch(
      chosen.map((c) => ({
        dir_path: c.dir_path,
        variant_key: c.variant_key,
        name: c.name,
        parts: c.parts,
        rotation: c.rotation,
      })),
    );
    if (result.status !== "ok") {
      toastStore.reportError("Failed to start batch render", result.error);
      return;
    }
    showBatchRender.value = false;
  };

  // Previews land incrementally — refresh the grid as they arrive so the
  // sweep is visible, not a single reveal at the end
  watch(modelFinishedCount, async () => {
    await runSearch();
  });
  watch(batchFinishedCount, async () => {
    if (batchSummary.value) {
      const { succeeded, failed } = batchSummary.value;
      toastStore.addToast(
        `Rendered ${succeeded} preview${succeeded === 1 ? "" : "s"}${failed ? ` — ${failed} failed` : ""}`,
        failed ? "info" : "success",
      );
    } else if (batchError.value) {
      toastStore.reportError("Batch render failed", batchError.value);
    } else if (batchCancelled.value) {
      toastStore.addToast(
        `Cancelled — ${batchCancelled.value.succeeded} preview${batchCancelled.value.succeeded === 1 ? "" : "s"} already rendered`,
        "info",
      );
    }
    for (const error of modelErrors.value.slice(0, 3)) {
      toastStore.addToast(error, "error");
    }
    await Promise.all([runSearch(), refreshMeta()]);
    if (selected.value) await selectEntry(selected.value);
  });

  /* ---- compressed at rest: pack/unpack the open model ---- */
  // Packing is per member FOLDER (nested variant folders pack themselves), so
  // the group-level action fans out to every member dir in the relevant state.
  const packableDirs = computed(() => [
    ...new Set(members.value.filter((m) => !m.packed).map((m) => m.dir_path)),
  ]);
  const packedDirs = computed(() => [
    ...new Set(members.value.filter((m) => m.packed).map((m) => m.dir_path)),
  ]);

  const packSelectedGroup = async () => {
    const group = selectedGroup.value;
    if (!group || !packableDirs.value.length || isPacking.value) return;
    const confirmed = await confirm(
      `Compress "${group.group_name}" (${formatFileSize(group.total_size_bytes)}) into pack archives?\n\n` +
        "The model stays in the catalog and unpacks on demand; printing, 3D preview and rendering need an unpack first.",
      { title: "Pack model", kind: "info" },
    );
    if (!confirmed) return;
    const result = await startPack(packableDirs.value);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to start packing", result.error);
    }
  };

  const unpackSelectedGroup = async () => {
    if (!packedDirs.value.length || isPacking.value) return;
    const result = await startUnpack(packedDirs.value);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to start unpacking", result.error);
    }
  };

  // Any terminal state changed disk state for the folders that DID finish —
  // refresh files, members and stats regardless of how the job ended
  watch(packFinishedCount, async () => {
    // extractions ride the same event stream but change nothing the index
    // knows about, and their outcome is handled where ensureLoose was awaited
    // — refreshing here would blank the drawer mid-print
    if (lastAction.value === "extract") return;
    if (packSummary.value) {
      const { action, succeeded, kept_files } = packSummary.value;
      toastStore.addToast(
        `${action === "unpack" ? "Unpacked" : "Packed"} ${succeeded} folder${succeeded === 1 ? "" : "s"}`,
        "success",
      );
      // files that changed between compression and delete stay loose — say so
      for (const kept of kept_files) {
        toastStore.addToast(
          `Kept on disk (changed while packing): ${kept}`,
          "info",
        );
      }
    } else if (packError.value) {
      toastStore.reportError("Pack job failed", packError.value);
    } else if (packCancelled.value) {
      toastStore.addToast(
        `Cancelled — ${packCancelled.value.succeeded} folder${packCancelled.value.succeeded === 1 ? "" : "s"} already done (re-run to resume)`,
        "info",
      );
    }
    await refreshSelected();
    await refreshMeta();
    if (selected.value) await selectEntry(selected.value);
  });

  // Carries the model's dir_path AND variant_key so the finished render comes
  // back as THIS pose's preview, not the whole folder's (poses in one dump
  // folder share a dir_path — only the variant_key tells them apart)
  const renderSelected = async () => {
    if (!selected.value) return;
    if (selected.value.packed) {
      // Blender reads the STLs from disk in the Render tab, so they must
      // exist before the handoff. No active cleanup here: the render needs
      // them until it finishes elsewhere — the exit sweep takes them back,
      // and marking them held stops a 3D-close or print cleanup of the same
      // member from deleting them mid-render.
      const extracted = await ensureLoose(stlPaths.value);
      if (extracted === null) return;
      for (const path of extracted) renderHeldPaths.value.add(path);
    }
    releasesStore.requestRender(
      stlPaths.value,
      selected.value.dir_path,
      undefined,
      selected.value.variant_key,
    );
  };

  /* ---- print: pick exactly which files go to the slicer ---- */
  // Print-ready scene files beat raw geometry: a .lys/.chitu already carries
  // supports and plate layout, so when a member has both, those are what
  // the modal pre-checks. Machine output (g-code, vendor resin plates) is
  // offered too but pre-checked only when it's all there is — nothing about
  // it can still be edited, only reprinted.
  const SLICED_EXTS = ["lys", "chitu", "chitubox"];
  const RAW_EXTS = ["stl", "obj", "3mf"];
  const MACHINE_EXTS = [
    "gcode",
    "bgcode",
    "goo",
    "ctb",
    "cbddlp",
    "photon",
    "pwmx",
    "pwmo",
    "pwms",
    "pw0",
  ];

  // What the modal offers: everything a slicer could eat. Images, licences
  // and archives stay out — offering them would only invite mis-ticks.
  const printCandidates = computed(() =>
    files.value.filter((f) =>
      [...SLICED_EXTS, ...RAW_EXTS, ...MACHINE_EXTS].includes(f.extension),
    ),
  );

  const printablePaths = computed(() => {
    const pool = [SLICED_EXTS, RAW_EXTS, MACHINE_EXTS]
      .map((exts) =>
        printCandidates.value.filter((f) => exts.includes(f.extension)),
      )
      .find((tier) => tier.length);
    return (pool ?? []).map((f) => f.path);
  });

  const showPrintModal = ref(false);
  const printSelection = ref<string[]>([]);
  const printBusy = ref(false);

  // any ticked file still inside the archive → the modal offers cleanup-after
  const printSelectionPacked = computed(() =>
    files.value.some((f) => f.packed && printSelection.value.includes(f.path)),
  );

  const togglePrintFile = (path: string) => {
    printSelection.value = printSelection.value.includes(path)
      ? printSelection.value.filter((p) => p !== path)
      : [...printSelection.value, path];
  };

  const printModel = async () => {
    if (!selected.value) return;
    const settingsResult = await commands.getSettings();
    const action =
      (settingsResult.status === "ok" && settingsResult.data.print_action) ||
      "open-in-slicer";
    // Reveal-folder users keep the direct flow: reveal takes no file list,
    // so a picker would be a pointless extra click for them. Same fallback
    // when there's nothing a slicer could open. A packed file's path has no
    // bytes on disk to reveal — fall through to a loose file or the folder.
    if (action === "reveal-folder" || !printCandidates.value.length) {
      await reveal(
        files.value.find((f) => !f.packed)?.path ?? selected.value.dir_path,
      );
      return;
    }
    printSelection.value = printablePaths.value;
    showPrintModal.value = true;
  };

  const sendToSlicer = async () => {
    if (!printSelection.value.length) return;
    // Snapshot: extraction takes real time, and the modal's checkboxes stay
    // live — what opens must be exactly what was extracted
    const selection = [...printSelection.value];
    printBusy.value = true;
    try {
      // Packed files materialize just-in-time — only the ticked entries are
      // pulled from the archive, "print straight from the bundle"
      const extracted = await ensureLoose(selection);
      if (extracted === null) return;
      if (!showPrintModal.value) {
        // the user cancelled the modal mid-extraction — don't surprise them
        // with a slicer window; just take the copies back
        if (extracted.length) cleanupEphemeralSafe(extracted);
        return;
      }
      // Our own command, not the opener plugin: its open_path is
      // fire-and-forget and reports success even when the OS has no app
      // for the file type — a print button that silently does nothing
      const result = await commands.openWithDefaultApp(selection);
      if (result.status === "ok") {
        showPrintModal.value = false;
        if (extracted.length && packCleanupAfter.value) {
          scheduleSlicerCleanup(extracted);
        }
        return;
      }
      // No slicer owns the extension: show why, then still be useful —
      // the modal stays open with Reveal folder one click away
      toastStore.reportError("Couldn't open in a slicer", result.error);
      toastStore.addToast(
        "Associate the files with your slicer, or use Reveal folder below",
        "info",
      );
    } catch (error) {
      toastStore.reportError("Failed to send to slicer", error);
    } finally {
      printBusy.value = false;
    }
  };

  const revealFromPrintModal = async () => {
    // packed selections point at paths with no bytes on disk — reveal a
    // loose file when there is one, else the model folder itself
    const target =
      printSelection.value.find(
        (path) => !files.value.some((f) => f.path === path && f.packed),
      ) ??
      files.value.find((f) => !f.packed)?.path ??
      selected.value?.dir_path;
    showPrintModal.value = false;
    if (target) await reveal(target);
  };

  const reveal = async (path: string) => {
    try {
      await revealItemInDir(path);
    } catch (error) {
      toastStore.reportError("Failed to reveal file", error);
    }
  };

  /* ---- normalizer: make the disk match the curated catalog ---- */
  // null = still checking (or not checked yet) — the drawer button shows a
  // disabled "checking…" state rather than flashing dirty-then-clean.
  const structureClean = ref<boolean | null>(null);

  /** Dry-run the plan for one model to decide the drawer's status. The
   * normalizer derives its canonical target from the effective metadata, so
   * an empty plan means exactly "folders match metadata". Sidecar rebuilding
   * is separate maintenance and must not make a green state look actionable. */
  const checkStructure = async (groupName: string) => {
    structureClean.value = null;
    if (!hasRoots.value) return;
    const result = await commands.planNormalize(null, groupName);
    // the user may have clicked to a different model while this was in
    // flight — a stale answer must never paint over the new selection
    if (selectedGroup.value?.group_name !== groupName) return;
    // Fail open on an error: showing the "fix" button is harmless (the plan
    // dialog will just fail again with the same error visible), but getting
    // stuck on "checking…" forever would hide a real problem
    structureClean.value =
      result.status === "ok" ? result.data.groups.length === 0 : false;
  };

  /* Re-run finalize WITHOUT moving anything: re-writes model.json for
     already-clean models from current catalog state, then rescans so the
     catalog re-reads them. The repair path when a Plinth update improves
     what the sidecar carries (e.g. the image lookup that used to write
     empty images lists) — otherwise clean models could never heal, since
     the normal flow only finalizes groups that had moves. */
  const refreshingSidecars = ref(false);
  const refreshSidecars = async (groupNames: string[]) => {
    const names = groupNames.filter(Boolean);
    if (!names.length || !hasRoots.value || refreshingSidecars.value) return;
    refreshingSidecars.value = true;
    try {
      const result = await commands.finalizeNormalize(names, []);
      if (result.status !== "ok") {
        toastStore.reportError("Failed to refresh metadata", result.error);
        return;
      }
      for (const warning of result.data) toastStore.addToast(warning, "error");
      toastStore.addToast(
        `Metadata re-written for ${names.length} model${names.length === 1 ? "" : "s"}`,
        "success",
      );
      // the sidecars changed on disk — only a rescan makes the catalog see it
      await scanAll();
    } finally {
      refreshingSidecars.value = false;
    }
  };

  // Everything is planned read-only first and shown as a reviewable move
  // list; nothing touches the NAS until "Move" is clicked. Ops are applied
  // in chunks so big batches show progress instead of a silent hang.
  const showNormalize = ref(false);
  const normalizePlanData = ref<NormalizePlan | null>(null);
  const normalizePlanning = ref(false);
  const normalizeChecked = ref<string[]>([]);
  const normalizeBusy = ref(false);
  const normalizeDone = ref(0);
  const normalizeTotal = ref(0);
  const normalizeIssues = ref<string[]>([]);
  const expandedPlanGroup = ref<string | null>(null);
  // non-null = the drawer asked to clean ONE model; null = whole catalog
  const normalizeScope = ref<string | null>(null);

  const openNormalize = async (group?: string) => {
    if (!hasRoots.value) {
      toastStore.addToast("Choose a catalog folder first", "info");
      return;
    }
    normalizeScope.value = group ?? null;
    showNormalize.value = true;
    normalizePlanData.value = null;
    normalizePlanning.value = true;
    normalizeIssues.value = [];
    normalizeDone.value = 0;
    normalizeTotal.value = 0;
    // the dry run respects the toolbar's designer facet (whole-catalog mode
    // only — a model cleanup must not be excluded by an unrelated filter),
    // so a NAS cleanup can proceed one designer at a time
    const result = await commands.planNormalize(
      group ? null : designerFilter.value || null,
      group ?? null,
    );
    normalizePlanning.value = false;
    if (result.status !== "ok") {
      toastStore.reportError("Failed to plan the cleanup", result.error);
      showNormalize.value = false;
      return;
    }
    normalizePlanData.value = result.data;
    normalizeChecked.value = result.data.groups.map((g) => g.group_name);
  };

  const toggleNormalizeGroup = (name: string) => {
    normalizeChecked.value = normalizeChecked.value.includes(name)
      ? normalizeChecked.value.filter((n) => n !== name)
      : [...normalizeChecked.value, name];
  };

  const allPlanChecked = computed(
    () =>
      !!normalizePlanData.value?.groups.length &&
      normalizeChecked.value.length === normalizePlanData.value.groups.length,
  );

  const toggleAllPlan = () => {
    normalizeChecked.value = allPlanChecked.value
      ? []
      : (normalizePlanData.value?.groups.map((g) => g.group_name) ?? []);
  };

  const applyNormalizePlan = async () => {
    const plan = normalizePlanData.value;
    if (!plan || normalizeBusy.value) return;
    const chosen = plan.groups.filter((g) =>
      normalizeChecked.value.includes(g.group_name),
    );
    const ops = chosen.flatMap((g) => g.ops);
    if (!ops.length) return;
    normalizeBusy.value = true;
    normalizeTotal.value = ops.length;
    normalizeDone.value = 0;
    normalizeIssues.value = [];
    try {
      const CHUNK = 100;
      // Sequential on purpose: moves must land in plan order (a folder
      // rename precedes the file moves inside it), and the chunking exists
      // to surface progress — parallelizing would break both.
      for (let i = 0; i < ops.length; i += CHUNK) {
        // oxlint-disable-next-line no-await-in-loop
        const result = await commands.applyNormalize(ops.slice(i, i + CHUNK));
        if (result.status !== "ok") {
          toastStore.reportError("Cleanup stopped", result.error);
          return;
        }
        normalizeIssues.value.push(...result.data.errors);
        normalizeDone.value = Math.min(ops.length, i + CHUNK);
      }
      const finalize = await commands.finalizeNormalize(
        chosen.map((g) => g.group_name),
        chosen.flatMap((g) => g.old_dirs),
      );
      if (finalize.status === "ok") {
        normalizeIssues.value.push(...finalize.data);
      } else {
        toastStore.reportError("Cleanup bookkeeping failed", finalize.error);
      }
      toastStore.addToast(
        `Cleaned up ${chosen.length} model${chosen.length === 1 ? "" : "s"}`,
        "success",
      );
      // the rescan re-reads the fresh model.json sidecars — completion also
      // refreshes search/stats via the existing scanCompletedCount watcher
      await scanAll();
      if (!normalizeIssues.value.length) {
        showNormalize.value = false;
      } else {
        normalizePlanData.value = null;
      }
    } finally {
      normalizeBusy.value = false;
    }
  };

  // The keeper must be a loose path: a packed keeper can't donate a hardlink
  // (merge refuses it) and 'delete copies' would remove every loose copy. A
  // stored choice that has since been packed is ignored, not honored.
  const keepFor = (group: DuplicateGroup) => {
    const stored = keepChoice.value[group.hash];
    if (stored && !packedIn(group).includes(stored)) return stored;
    return (
      group.paths.find((path) => !packedIn(group).includes(path)) ??
      group.paths[0]
    );
  };

  // What merge/delete can actually touch: everything except the keeper and
  // the packed copies (those have no loose bytes on disk)
  const actionableOthers = (group: DuplicateGroup) =>
    group.paths.filter(
      (path) => path !== keepFor(group) && !packedIn(group).includes(path),
    );

  // A packed path has no file on disk to reveal — show its folder instead
  const revealDupPath = (group: DuplicateGroup, path: string) =>
    reveal(
      packedIn(group).includes(path)
        ? (path.replace(/[\\/][^\\/]*$/, "") ?? path)
        : path,
    );

  // Probed with a real hardlink attempt next to the first duplicate (NAS and
  // exFAT support can't be guessed from names) — gates the merge buttons so
  // link-less volumes get delete-only instead of a button that can't work.
  const linkSupport = ref<boolean | null>(null);
  watch(showDups, async (open) => {
    const probePath = dupGroups.value[0]?.paths[0];
    if (!open || linkSupport.value !== null || !probePath) return;
    const result = await commands.supportsFileLinks(probePath);
    linkSupport.value = result.status === "ok" ? result.data : false;
  });

  const runMerge = async (group: DuplicateGroup) => {
    const keep = keepFor(group);
    const others = actionableOthers(group);
    if (!others.length) return 0;
    const result = await commands.mergeDuplicateFiles(keep, others);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to merge duplicates", result.error);
      return 0;
    }
    for (const error of result.data.errors) toastStore.addToast(error, "error");
    return result.data.succeeded;
  };

  const mergeGroup = async (group: DuplicateGroup) => {
    const confirmed = await confirm(
      `Merge ${group.paths.length} identical files so they share one copy on disk?\n\nEvery variant keeps a working file — ${formatFileSize(reclaimableBytes(group))} is freed.`,
      { title: "Merge duplicates", kind: "info" },
    );
    if (!confirmed) return;
    reclaimBusy.value = true;
    try {
      const merged = await runMerge(group);
      if (merged) {
        toastStore.addToast(
          `Merged into one shared copy — ${formatFileSize(reclaimableBytes(group))} freed`,
          "success",
        );
      }
      await refreshMeta();
    } finally {
      reclaimBusy.value = false;
    }
  };

  const mergeAllGroups = async () => {
    const targets = reclaimableGroups.value;
    const confirmed = await confirm(
      `Merge all ${targets.length} duplicate groups so identical files share one copy on disk?\n\nNothing disappears from any folder — ${formatFileSize(wastedBytes.value)} is freed.`,
      { title: "Merge all duplicates", kind: "info" },
    );
    if (!confirmed) return;
    reclaimBusy.value = true;
    try {
      let merged = 0;
      // Sequential on purpose: every merge re-hashes whole files on the same
      // disk (often a NAS) — concurrency would only add seek thrash
      // oxlint-disable-next-line no-await-in-loop
      for (const group of targets) merged += await runMerge(group);
      if (merged) {
        toastStore.addToast(
          `Merged ${merged} duplicate file${merged === 1 ? "" : "s"} into shared copies`,
          "success",
        );
      }
      await refreshMeta();
    } finally {
      reclaimBusy.value = false;
    }
  };

  const reclaimGroup = async (group: DuplicateGroup) => {
    const keep = keepFor(group);
    const doomed = actionableOthers(group);
    if (!doomed.length) return;
    const confirmed = await confirm(
      `Delete ${doomed.length} duplicate file${doomed.length === 1 ? "" : "s"} and keep:\n${keep}`,
      { title: "Reclaim duplicates", kind: "warning" },
    );
    if (!confirmed) return;
    reclaimBusy.value = true;
    try {
      const result = await commands.deleteDuplicateFiles(doomed);
      if (result.status === "ok") {
        const { succeeded, errors } = result.data;
        if (succeeded) {
          toastStore.addToast(
            `Reclaimed ${succeeded} duplicate file${succeeded === 1 ? "" : "s"}`,
            "success",
          );
        }
        for (const error of errors) toastStore.addToast(error, "error");
        // the backend pruned the index, so groups/stats/sizes are already fresh
        await Promise.all([runSearch(), refreshMeta()]);
      } else {
        toastStore.reportError("Failed to delete duplicates", result.error);
      }
    } finally {
      reclaimBusy.value = false;
    }
  };

  const toggleCheckedGroup = (groupName: string) => {
    checkedGroups.value = checkedGroups.value.includes(groupName)
      ? checkedGroups.value.filter((g) => g !== groupName)
      : [...checkedGroups.value, groupName];
  };

  const clearSelection = () => {
    checkedGroups.value = [];
    combining.value = false;
  };

  const startCombine = () => {
    combineName.value = checkedGroups.value[0] ?? "";
    combining.value = true;
  };

  // The manual counterpart to folder inference: creators structure their
  // libraries every which way, so combining can never depend on the scanner
  // having guessed right — pick the cards, give them one name.
  const combineChecked = async () => {
    const names = [...checkedGroups.value];
    const target = combineName.value.trim();
    if (!target || names.length < 2) return;
    if (!(await confirmRenameAmbiguity(names, "Combine"))) return;
    const result = await commands.combineCatalogGroups(names, target);
    combining.value = false;
    if (result.status !== "ok") {
      toastStore.reportError("Failed to combine models", result.error);
      return;
    }
    toastStore.addToast(
      `Combined ${names.length} models into "${target}"`,
      "success",
    );
    checkedGroups.value = [];
    await Promise.all([runSearch(), refreshMeta()]);
    const merged = groups.value.find(
      (g) => g.group_name.toLowerCase() === target.toLowerCase(),
    );
    if (merged) await selectGroup(merged);
  };

  // The selected pose's own image, else the SAME pose from another support
  // variant — nobody renders the supported copy separately, so supported/
  // unsupported share pictures automatically. Sharing stops at the pose
  // boundary: pose B never borrows pose A's picture, they're different minis.
  const drawerPreview = computed(() => {
    const entry = selected.value;
    if (!entry) return null;
    if (entry.preview_path) return entry.preview_path;
    const poseKey = entry.pose ?? entry.name;
    return (
      members.value.find(
        (m) => m.preview_path && (m.pose ?? m.name) === poseKey,
      )?.preview_path ?? null
    );
  });

  const moveChecked = async () => {
    const dest = await selectDirectory({ title: "Move selected models into…" });
    if (!dest) return;
    // A checked group is already visible under the backend's current access
    // state, so resolve exactly the variants the user was allowed to browse.
    const memberResults = await Promise.all(
      checkedGroups.value.map((name) => commands.getCatalogGroupMembers(name)),
    );
    const dirs = memberResults.flatMap((result) =>
      result.status === "ok" ? result.data.map((m) => m.dir_path) : [],
    );
    const sep = dest.includes("\\") ? "\\" : "/";
    const operations = dirs
      .map((from) => ({
        from,
        to: `${dest}${sep}${from.split(/[\\/]/).pop()}`,
      }))
      .filter((op) => op.from !== op.to);
    if (!operations.length) {
      toastStore.addToast("Those models are already in that folder", "warning");
      return;
    }
    const confirmed = await confirm(
      `Move ${operations.length} folder${operations.length === 1 ? "" : "s"} (${checkedGroups.value.length} model${checkedGroups.value.length === 1 ? "" : "s"}) into:\n${dest}`,
      { title: "Reorganize models", kind: "warning" },
    );
    if (!confirmed) return;
    const result = await commands.batchMoveModels(operations);
    if (result.status === "ok") {
      const { succeeded, errors } = result.data;
      if (succeeded) {
        toastStore.addToast(
          `Moved ${succeeded} folder${succeeded === 1 ? "" : "s"}`,
          "success",
        );
      }
      for (const error of errors) toastStore.addToast(error, "error");
      checkedGroups.value = [];
      // the selected entries' dir_paths may have just changed
      selectedGroup.value = null;
      selected.value = null;
      members.value = [];
      files.value = [];
      await Promise.all([runSearch(), refreshMeta()]);
    } else {
      toastStore.reportError("Failed to move models", result.error);
    }
  };

  /* ---- deletion: catalog and/or disk, always through the modal ---- */

  const showDeleteModal = ref(false);
  const deleteBusy = ref(false);
  // Disk deletion defaults ON: the whole point of deleting a model is that
  // it stops taking space. It's a trash move (recoverable), not an unlink.
  const deleteAlsoFromDisk = ref(true);
  const deleteTargetNames = ref<string[]>([]);
  const deleteTargetDirs = ref<string[]>([]);
  const deleteSummary = ref<DeleteSummary | null>(null);

  const openDeleteModal = async (names: string[]) => {
    if (!names.length) return;
    // Resolve exactly the currently accessible variants behind each card.
    const memberResults = await Promise.all(
      names.map((name) => commands.getCatalogGroupMembers(name)),
    );
    const dirs = [
      ...new Set(
        memberResults.flatMap((result) =>
          result.status === "ok" ? result.data.map((m) => m.dir_path) : [],
        ),
      ),
    ];
    if (!dirs.length) {
      toastStore.addToast("Nothing to delete for that selection", "warning");
      return;
    }
    deleteTargetNames.value = names;
    deleteTargetDirs.value = dirs;
    deleteAlsoFromDisk.value = true;
    deleteSummary.value = null;
    showDeleteModal.value = true;
    // Counted by the backend with the same scoping the delete will use,
    // so the dialog can't promise less than what actually goes
    const summary = await commands.summarizeModelDirs(dirs);
    if (summary.status === "ok") deleteSummary.value = summary.data;
  };

  const confirmDelete = async () => {
    if (deleteBusy.value || !deleteTargetDirs.value.length) return;
    deleteBusy.value = true;
    try {
      const result = await commands.deleteModels(
        deleteTargetDirs.value,
        deleteAlsoFromDisk.value,
      );
      if (result.status !== "ok") {
        toastStore.reportError("Failed to delete models", result.error);
        return;
      }
      const { succeeded, hard_deleted, errors } = result.data;
      if (succeeded) {
        toastStore.addToast(
          deleteAlsoFromDisk.value
            ? `Deleted ${succeeded} folder${succeeded === 1 ? "" : "s"} — recoverable from the system trash`
            : `Removed ${succeeded} folder${succeeded === 1 ? "" : "s"} from the catalog — files kept, future scans skip them`,
          "success",
        );
      }
      // The dialog promised the trash; where a volume couldn't deliver
      // (no trash on most NAS shares) the user hears what happened instead
      if (hard_deleted) {
        toastStore.addToast(
          `${hard_deleted} folder${hard_deleted === 1 ? "" : "s"} had no trash available and ${hard_deleted === 1 ? "was" : "were"} deleted permanently`,
          "warning",
        );
      }
      for (const error of errors) toastStore.addToast(error, "error");
      showDeleteModal.value = false;
      checkedGroups.value = checkedGroups.value.filter(
        (name) => !deleteTargetNames.value.includes(name),
      );
      // the open drawer may be showing what was just deleted
      selectedGroup.value = null;
      selected.value = null;
      members.value = [];
      files.value = [];
      await Promise.all([runSearch(), refreshMeta()]);
    } finally {
      deleteBusy.value = false;
    }
  };

  /* ---- 18+ marking: a display filter, not encryption — scans still index
     everything; Settings' "Show 18+" toggle is what hides it from browsing */
  // Toggle behavior: if EVERY named group is already flagged, the action
  // unmarks all of them; otherwise it marks all of them — one button for
  // both directions, same as how the batch bar's other togglers behave.
  const toggleGroupNsfw = async (names: string[]) => {
    if (!names.length) return;
    // groups.value first (what the spec asks); fall back to the open
    // drawer's own group, which may not be in the current page if it's
    // currently hidden by the browse filter itself
    const findGroup = (name: string) =>
      groups.value.find(
        (g) => g.group_name.toLowerCase() === name.toLowerCase(),
      ) ??
      (selectedGroup.value?.group_name.toLowerCase() === name.toLowerCase()
        ? selectedGroup.value
        : undefined);
    const targeted = names.map(findGroup).filter((g): g is CatalogGroup => !!g);
    const target = !(targeted.length && targeted.every((g) => g.nsfw));
    const result = await commands.setGroupNsfw(names, target);
    if (result.status !== "ok") {
      toastStore.reportError("Failed to update the 18+ flag", result.error);
      return;
    }
    toastStore.addToast(
      target
        ? `Marked ${names.length} model${names.length === 1 ? "" : "s"} 18+`
        : `Removed 18+ mark from ${names.length} model${names.length === 1 ? "" : "s"}`,
      "success",
    );
    await Promise.all([runSearch(), refreshMeta()]);
    if (
      selectedGroup.value &&
      names.some(
        (name) =>
          name.toLowerCase() === selectedGroup.value?.group_name.toLowerCase(),
      )
    ) {
      await refreshSelected();
    }
  };

  watch(selected, (entry) => {
    metaDraft.value = {
      // NAME is the card/sort name — i.e. the GROUP name — not the per-variant
      // name. Variants are told apart by their pose, so this one field renames
      // the whole model regardless of how many poses it has.
      name: selectedGroup.value?.group_name ?? entry?.name ?? "",
      variant: entry?.variant ?? "",
      pose: entry?.pose ?? "",
      scale: entry?.scale ?? "",
      support_status: entry?.support_status ?? "",
      release_date: entry?.release_date ?? "",
      designer: entry?.designer ?? "",
      sculptor: entry?.sculptor ?? "",
      release_name: entry?.release_name ?? "",
      base_round_mm: entry?.base_round_mm ?? "",
      base_square_mm: entry?.base_square_mm ?? "",
    };
    // fresh member: drop any ticks, and seed the assign boxes with this member's
    // facets so filing more files under the same bucket is one tap
    checkedFiles.value = [];
    variantAssignDraft.value = entry?.variant ?? "";
    poseAssignDraft.value = entry?.pose ?? "";
  });

  const metaDirty = computed(() => {
    const entry = selected.value;
    if (!entry) return false;
    const draft = metaDraft.value;
    return (
      draft.name !== (selectedGroup.value?.group_name ?? entry.name) ||
      draft.variant !== (entry.variant ?? "") ||
      draft.pose !== (entry.pose ?? "") ||
      draft.scale !== (entry.scale ?? "") ||
      draft.support_status !== (entry.support_status ?? "") ||
      draft.release_date !== (entry.release_date ?? "") ||
      draft.designer !== (entry.designer ?? "") ||
      draft.sculptor !== (entry.sculptor ?? "") ||
      draft.release_name !== (entry.release_name ?? "") ||
      draft.base_round_mm !== (entry.base_round_mm ?? "") ||
      draft.base_square_mm !== (entry.base_square_mm ?? "")
    );
  });

  const saveMetadata = async () => {
    const entry = selected.value;
    const group = selectedGroup.value;
    if (!entry || !group) return false;
    const draft = metaDraft.value;
    // A file-split member's variant/pose/support live in file_variants, not
    // model_user_meta — writing them there would silently revert on reload.
    const isVariant = !!entry.variant_key;
    const newVariant = draft.variant.trim();
    const newPose = draft.pose.trim();
    const bucketChanged =
      isVariant &&
      (newVariant !== (entry.variant ?? "") ||
        newPose !== (entry.pose ?? "") ||
        orNull(draft.support_status) !== (entry.support_status ?? null));

    if (bucketChanged) {
      // re-file this member's files under the edited facets (or unfile them
      // back to the pool when both variant and pose are cleared)
      const paths = files.value.map((file) => file.path);
      const refiled =
        newVariant || newPose
          ? await commands.assignFilesToPose(
              paths,
              newVariant || null,
              newPose || null,
              orNull(draft.support_status),
            )
          : await commands.clearFilePose(paths);
      if (refiled.status !== "ok") {
        toastStore.reportError("Failed to re-file member", refiled.error);
        return false;
      }
    }

    // Model-level metadata (shared by every member of the folder). custom_name
    // is preserved — NAME drives the group name below. For a variant member
    // variant/pose/support are null here; they went to file_variants.
    const result = await commands.updateModelMetadata(entry.dir_path, {
      custom_name: entry.custom_name ?? null,
      variant: isVariant ? null : orNull(draft.variant),
      pose: isVariant ? null : orNull(draft.pose),
      scale: orNull(draft.scale),
      support_status: isVariant ? null : orNull(draft.support_status),
      release_date: orNull(draft.release_date),
      designer: orNull(draft.designer),
      sculptor: orNull(draft.sculptor),
      release_name: orNull(draft.release_name),
      base_round_mm: mmOrNull(draft.base_round_mm),
      base_square_mm: mmOrNull(draft.base_square_mm),
    });
    if (result.status !== "ok") {
      toastStore.reportError("Failed to save details", result.error);
      return false;
    }
    // variant/pose/scale were also applied to this sculpt's other support
    // builds (exact folder twins) — say so, since the user didn't click them
    const twinCount = result.data;
    const savedToast = () =>
      toastStore.addToast(
        twinCount
          ? `Details saved · also applied to ${twinCount} matching build${twinCount === 1 ? "" : "s"}`
          : "Details saved",
        "success",
      );

    // NAME edits the group/card name (the sort key) for every model.
    const newName = draft.name.trim();
    if (newName && newName !== group.group_name) {
      if (!(await confirmRenameAmbiguity([group.group_name], "Rename"))) {
        savedToast();
        return false;
      }
      const renamed = await commands.renameCatalogGroup(
        group.group_name,
        newName,
      );
      if (renamed.status !== "ok") {
        toastStore.reportError(
          "Saved details, but rename failed",
          renamed.error,
        );
        await refreshSelected();
        return false;
      }
      savedToast();
      // the card moved to its new name — re-open it there
      await Promise.all([runSearch(), refreshMeta()]);
      const found = groups.value.find(
        (g) => g.group_name.toLowerCase() === newName.toLowerCase(),
      );
      if (found) await selectGroup(found);
      return true;
    }

    savedToast();
    // land on the re-filed bucket (or the pool, if both facets were cleared)
    if (bucketChanged) {
      await Promise.all([
        runSearch(),
        reloadMembers(variantKeyFor(entry.dir_path, newVariant, newPose)),
      ]);
    } else {
      await refreshSelected();
    }
    const currentGroupName = selectedGroup.value?.group_name;
    if (currentGroupName) await checkStructure(currentGroupName);
    return true;
  };

  /** The drawer's expected workflow: metadata defines the destination.
   * Commit any edits first, then calculate a fresh reviewable move plan from
   * that committed state. A failed/cancelled save never opens a stale plan. */
  const cleanUpSelectedGroup = async () => {
    const intendedName =
      metaDraft.value.name.trim() || selectedGroup.value?.group_name || "";
    if (metaDirty.value && !(await saveMetadata())) return;
    const groupName =
      groups.value.find(
        (group) =>
          group.group_name.toLowerCase() === intendedName.toLowerCase(),
      )?.group_name ??
      selectedGroup.value?.group_name ??
      intendedName;
    if (groupName) await openNormalize(groupName);
  };

  const pickPreviewImage = async () => {
    const entry = selected.value;
    if (!entry) return;
    const picked = await selectFiles({
      accept: "image/*",
      multiple: false,
      title: "Choose a preview image",
    });
    const image = picked?.[0];
    if (!image) return;
    // The backend copies the file into the app's previews dir, so the
    // catalog doesn't break if the original moves or gets deleted. variant_key
    // keeps the pick on this pose alone when the folder holds several.
    const result = await commands.setModelPreview(
      entry.dir_path,
      image.path,
      entry.variant_key,
    );
    if (result.status === "ok") {
      toastStore.addToast("Preview updated", "success");
      await refreshSelected();
    } else {
      toastStore.reportError("Failed to set preview", result.error);
    }
  };

  const displayPath = computed(() => {
    const entry = selected.value;
    if (!entry) return "";
    const owner = roots.value.find(
      (r) =>
        entry.dir_path === r.root ||
        entry.dir_path.startsWith(`${r.root}/`) ||
        entry.dir_path.startsWith(`${r.root}\\`),
    );
    return owner
      ? entry.dir_path.slice(owner.root.length).replace(/^[/\\]/, "")
      : entry.dir_path;
  });

  /**
   * Stage a catalog model using its source paths. The release builder copies
   * it only after the user has chosen the release details.
   */
  const addToDraftRelease = async () => {
    if (!selected.value) return;
    try {
      const entry = selected.value;
      const groupName = selectedGroup.value?.group_name ?? entry.name;
      const variants = members.value.length ? members.value : [entry];
      // Staging copies the loose files (file/stage.rs), which a packed
      // member doesn't have — same refusal as normalize/move/dup-merge
      const packedSkipped = variants.filter((variant) => variant.packed);
      const newVariants = variants.filter(
        (variant) =>
          !variant.packed &&
          !releasesStore.models.some(
            (draft) => draft.source_dir === variant.dir_path,
          ),
      );
      const fileResults = await Promise.all(
        newVariants.map((variant) =>
          commands.getCatalogModelFiles(variant.dir_path, variant.variant_key),
        ),
      );
      // Per-file pose assignments ride along so a curated dump folder
      // reappears already split on the receiving side (docs/3PK.md)
      const assignmentResults = await Promise.all(
        newVariants.map((variant) =>
          commands.getFileVariants(variant.dir_path),
        ),
      );
      for (const [index, variant] of newVariants.entries()) {
        const fileResult = fileResults[index];
        if (fileResult.status !== "ok") throw fileResult.error;
        const fileNames = new Set(
          fileResult.data.map((file) => file.file_name),
        );
        const assignments = assignmentResults[index];
        const filePoses = (assignments.status === "ok" ? assignments.data : [])
          .filter((assignment) => fileNames.has(basename(assignment.path)))
          .map((assignment) => ({
            name: basename(assignment.path),
            variant: assignment.variant,
            pose: assignment.pose,
            support_status: assignment.support_status,
          }));
        const poseKey = variant.pose ?? variant.name;
        // Mirror the catalog drawer's preview resolution so the render the user
        // sees on the card actually rides along: the pose's own image, else a
        // sibling variant sharing the pose, else the group's aggregate preview.
        const preview =
          variant.preview_path ??
          variants.find(
            (candidate) =>
              candidate.preview_path &&
              (candidate.pose ?? candidate.name) === poseKey,
          )?.preview_path ??
          selectedGroup.value?.preview_path ??
          null;
        releasesStore.models.push({
          id: `draft-${Date.now()}-${releasesStore.models.length}`,
          name: variant.name,
          description: variant.description,
          tags: [...variant.tags],
          images: preview ? [preview] : [],
          model_files: fileResult.data.map((file) => file.path),
          group: variants.length > 1 ? groupName : null,
          source_dir: variant.dir_path,
          source_group: groupName,
          // The full curation travels: model.json → manifest → another
          // user's catalog (the whole point of the 3pk format)
          variant: variant.variant,
          pose: variant.pose,
          scale: variant.scale,
          support_status: variant.support_status,
          release_date: variant.release_date,
          designer: variant.designer,
          sculptor: variant.sculptor,
          release_name: variant.release_name,
          base_round_mm: variant.base_round_mm,
          base_square_mm: variant.base_square_mm,
          file_poses: filePoses,
        });
      }
      if (packedSkipped.length) {
        toastStore.addToast(
          `📦 ${packedSkipped.length} pose${packedSkipped.length === 1 ? "" : "s"} skipped — packed (unpack first to add to a release)`,
          "warning",
        );
      }
      if (newVariants.length || !packedSkipped.length) {
        toastStore.addToast(
          newVariants.length
            ? `Added "${groupName}" with ${newVariants.length} pose${newVariants.length === 1 ? "" : "s"}`
            : `"${groupName}" is already in the release`,
          newVariants.length ? "success" : "info",
        );
      }
    } catch (error) {
      toastStore.reportError("Failed to add model to release", error);
    }
  };

  /** Queue-preserving worker — the completed-watcher chains through this. */
  const startRootScan = async (root: string) => {
    const result = await startScan(root);
    if (result.status === "error") {
      scanQueue.value = [];
      toastStore.reportError("Failed to start scan", result.error);
    }
  };

  /** Single-folder scan from the dropdown. Drops any leftover batch queue
      first — a stale queue would chain unrelated folders after this one. */
  const scanRoot = async (root: string) => {
    scanQueue.value = [];
    await startRootScan(root);
  };

  /** Rescan every folder, strictly one at a time — the queue drains in the
      scan-completed watcher so the NAS never sees two walks at once. */
  const scanAll = async () => {
    if (!roots.value.length) return;
    scanQueue.value = roots.value.slice(1).map((r) => r.root);
    await startRootScan(roots.value[0].root);
  };

  /** Pick-and-scan: adding a folder immediately indexes it. */
  const addFolder = async () => {
    const dir = await selectDirectory({ title: "Add catalog folder" });
    if (!dir) return;
    const added = await commands.addCatalogRoot(dir);
    if (added.status !== "ok") {
      toastStore.reportError("Couldn't add folder", added.error);
      return;
    }
    await refreshRoots();
    await scanRoot(dir);
  };

  const removeRoot = async (root: string) => {
    const confirmed = await confirm(
      `Remove this folder from the catalog?\n\n${root}\n\nFiles on disk are untouched; its models leave the catalog until you add the folder back and rescan.`,
      { title: "Remove catalog folder", kind: "warning" },
    );
    if (!confirmed) return;
    const result = await commands.removeCatalogRoot(root);
    if (result.status !== "ok") {
      toastStore.reportError("Couldn't remove folder", result.error);
      return;
    }
    await Promise.all([refreshRoots(), runSearch(), refreshMeta()]);
  };

  /** Star a folder as the staging target: Clean up then moves every
      folder's models into it. Starring the current primary unsets it —
      back to each folder cleaning up in place. */
  const togglePrimary = async (target: CatalogRootSummary) => {
    const result = await commands.setPrimaryCatalogRoot(
      target.primary ? null : target.root,
    );
    if (result.status !== "ok") {
      toastStore.reportError("Couldn't set primary folder", result.error);
      return;
    }
    await refreshRoots();
  };

  const cancelAllScans = async () => {
    // drop the queue FIRST or the completed-watcher starts the next folder
    scanQueue.value = [];
    await cancelScan();
  };

  // a failed scan ends the batch — silently continuing would report
  // "scan complete" over a folder that never got indexed
  watch(scanError, (error) => {
    if (error) scanQueue.value = [];
  });

  watch(scanCompletedCount, async () => {
    // a queued folder keeps the pipeline going; the summary refresh waits
    // until the whole batch is done
    const next = scanQueue.value.shift();
    if (next) {
      await refreshRoots();
      await startRootScan(next);
      return;
    }
    toastStore.addToast("Catalog scan complete", "success");
    await Promise.all([refreshRoots(), runSearch(), refreshMeta()]);
    // The open drawer shows pre-scan members otherwise — a rescan that
    // regroups models (or removes dirs) must be visible immediately, not
    // after a reopen
    const openGroup = selectedGroup.value;
    if (!openGroup) return;
    const fresh = groups.value.find(
      (g) => g.group_name.toLowerCase() === openGroup.group_name.toLowerCase(),
    );
    if (fresh) {
      await selectGroup(fresh);
    } else {
      selectedGroup.value = null;
      selected.value = null;
      members.value = [];
    }
  });

  watch(dupCompletedCount, async () => {
    const dupResult = await commands.getDuplicateGroups();
    if (dupResult.status === "ok") {
      dupGroups.value = dupResult.data;
      // Same filter as the footer and the panel: already-merged (shared)
      // groups are done, not news — counting them here made the toast and
      // the summary disagree after a few merges
      const actionable = reclaimableGroups.value.length;
      const shared = dupGroups.value.length - actionable;
      showDups.value = actionable > 0;
      toastStore.addToast(
        actionable
          ? `Found ${actionable} duplicate group${actionable === 1 ? "" : "s"}${shared ? ` (${shared} already merged)` : ""}`
          : "No duplicates found",
        actionable ? "warning" : "success",
      );
    }
  });

  watch(geoCompletedCount, async () => {
    const summary = geoSummary.value;
    if (summary) {
      const { mined, already_known, failed, skipped_too_large } = summary;
      toastStore.addToast(
        `Mined geometry for ${mined} file${mined === 1 ? "" : "s"}` +
          (already_known ? ` · ${already_known} already known` : "") +
          (failed ? ` · ${failed} failed` : "") +
          (skipped_too_large
            ? ` · ${skipped_too_large} skipped (too large)`
            : ""),
        failed ? "warning" : "success",
      );
    }
    // The open drawer shows pre-scan (empty) geometry otherwise — refresh it
    // in place so the section appears without reopening the model.
    if (selected.value) {
      const result = await commands.getModelGeometry(selected.value.dir_path);
      if (result.status === "ok") modelGeometry.value = result.data;
    }
  });

  // Mirrors the backend session into this store; called before the refreshes
  // below so a change made in Settings is already in effect by the time
  // they run, not one visit later.
  const syncShowNsfw = async () => {
    const result = await commands.getNsfwAccessState();
    if (result.status === "ok") showNsfw.value = result.data.unlocked;
  };

  /** Orchestrated once by the Catalog view's onMounted. */
  const init = async () => {
    await Promise.all([initPackCleanupPref(), syncShowNsfw()]);
    await refreshRoots();
    const repair = await commands.repairPlinthBaseExports();
    if (repair.status === "ok") {
      for (const warning of repair.data.warnings) {
        toastStore.addToast(warning, "warning", 0);
      }
      if (repair.data.repaired > 0) {
        toastStore.addToast(
          `Repaired metadata for ${repair.data.repaired} existing Plinth base${repair.data.repaired === 1 ? "" : "s"}; rescanning the catalog`,
          "success",
        );
        await scanAll();
      }
    } else {
      toastStore.reportError(
        "Failed to repair existing Plinth base metadata",
        repair.error,
      );
    }
    await Promise.all([runSearch(), refreshMeta()]);
  };

  /** Orchestrated by the Catalog view's onActivated (KeepAlive re-entry). */
  const onReactivated = async () => {
    await syncShowNsfw();
    await Promise.all([
      selectedGroup.value ? refreshSelected() : runSearch(),
      refreshMeta(),
    ]);
  };

  return {
    // jobs
    isScanning,
    scanProgress,
    scanError,
    isFindingDuplicates,
    dupProgress,
    startDuplicateScan,
    cancelDuplicateScan,
    isMiningGeometry,
    geoProgress,
    startGeometryScan,
    cancelGeometryScan,
    isPacking,
    packProgress,
    packJobLabel,
    cancelPack,
    isBatchRendering,
    batchProgress,
    cancelBatch,
    // roots / folders
    roots,
    hasRoots,
    refreshRoots,
    scanRoot,
    scanAll,
    addFolder,
    removeRoot,
    togglePrimary,
    cancelAllScans,
    // search / filters
    query,
    viewMode,
    selectedTags,
    allTags,
    visibleTags,
    toggleTag,
    groupMode,
    designerFilter,
    designers,
    groups,
    total,
    stats,
    sections,
    sectionModelCount,
    renameDesignerFacet,
    renameReleaseFacet,
    runSearch,
    loadMore,
    lastScanLabel,
    // selection / drawer
    selectedGroup,
    drawerLoadError,
    members,
    activeSupport,
    activeVariant,
    selected,
    files,
    newTag,
    selectGroup,
    selectEntry,
    groupSummary,
    memberKey,
    supportTabs,
    tabLabel,
    variantsInTab,
    variantLabel,
    hasAutoSplit,
    tabMembers,
    setSupportTab,
    setVariant,
    groupSources,
    startRenameGroup,
    renamingGroup,
    groupNameDraft,
    renameGroup,
    useAsCardImage,
    detachSelectedSource,
    splitGroup,
    isFlattening,
    flattenGroup,
    addTag,
    removeTag,
    refreshSelected,
    drawerPreview,
    displayPath,
    measuredLabel,
    stlPaths,
    modelGeometry,
    geometryTallestMm,
    geometryTotalVolumeMl,
    geometryTotalTriCount,
    drawerWidth,
    startDrawerResize,
    // 3D viewer
    show3d,
    show3dModal,
    showImageModal,
    viewer3dBusy,
    toggle3d,
    // file assignment
    checkedFiles,
    variantAssignDraft,
    poseAssignDraft,
    fileVariantMap,
    toggleCheckedFile,
    selectMatchingFiles,
    assignChecked,
    clearChecked,
    // metadata form
    metaDraft,
    metaDirty,
    saveMetadata,
    pickPreviewImage,
    addToDraftRelease,
    // structure / normalize
    structureClean,
    refreshingSidecars,
    refreshSidecars,
    showNormalize,
    normalizePlanData,
    normalizePlanning,
    normalizeChecked,
    normalizeBusy,
    normalizeDone,
    normalizeTotal,
    normalizeIssues,
    expandedPlanGroup,
    normalizeScope,
    openNormalize,
    cleanUpSelectedGroup,
    toggleNormalizeGroup,
    allPlanChecked,
    toggleAllPlan,
    opLabel,
    applyNormalizePlan,
    // packing
    bulkPack,
    packSelectedGroup,
    unpackSelectedGroup,
    packableDirs,
    packedDirs,
    // batch render
    showBatchRender,
    batchCandidates,
    batchRerenderExisting,
    batchLoading,
    batchMissing,
    batchExisting,
    batchPackedSkipped,
    openBatchRender,
    startBatchRender,
    // print
    MACHINE_EXTS,
    showPrintModal,
    printSelection,
    printBusy,
    printCandidates,
    printSelectionPacked,
    packCleanupAfter,
    persistCleanupAfter,
    togglePrintFile,
    printModel,
    sendToSlicer,
    revealFromPrintModal,
    reveal,
    renderSelected,
    // batch selection / combine / move
    checkedGroups,
    combining,
    combineName,
    toggleCheckedGroup,
    clearSelection,
    startCombine,
    combineChecked,
    moveChecked,
    // deletion
    showDeleteModal,
    deleteBusy,
    deleteAlsoFromDisk,
    deleteTargetNames,
    deleteTargetDirs,
    deleteSummary,
    openDeleteModal,
    confirmDelete,
    // 18+ marking
    showNsfw,
    toggleGroupNsfw,
    // duplicates
    dupGroups,
    showDups,
    toggleDups,
    keepChoice,
    reclaimBusy,
    linkSupport,
    wastedBytes,
    reclaimableGroups,
    reclaimableBytes,
    packedIn,
    keepFor,
    actionableOthers,
    revealDupPath,
    mergeGroup,
    mergeAllGroups,
    reclaimGroup,
    // meta refresh
    refreshMeta,
    // lifecycle orchestration
    init,
    onReactivated,
  };
});

// Preserve newly added state/actions when this store changes during `vite dev`.
if (import.meta.hot) {
  import.meta.hot.accept(acceptHMRUpdate(useCatalogStore, import.meta.hot));
}
