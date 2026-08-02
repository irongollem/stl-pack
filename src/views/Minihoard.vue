<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import {
  type MinihoardEntry,
  type MinihoardError,
  type MinihoardHealth,
  commands,
  events,
} from "../bindings";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { MinihoardObject } from "../bindings";
import { useMinihoard } from "../composables/useMinihoard";
import { useMinihoardDownload } from "../composables/useMinihoardDownload";
import { useToastStore } from "../stores/toastStore";
import { formatFileSize } from "../utils/format";

/* Phase 3 of docs/MINIHOARD.md: the easter-egg console becomes a clickable
   library once the sibling CLI speaks --json (>= 0.4.0). Below that gate
   there's nothing to browse — minihoard only prints human text — so that
   case keeps the old terminal-proxy UI with an update hint. The raw
   console/event-stream machinery (lines, run, cancel) is shared by both
   branches: the legacy UI is built entirely on it, and the modern UI keeps
   it around, demoted to a collapsed log, for `sync-cookie`. */

const { info } = useMinihoard();
const toastStore = useToastStore();
const download = useMinihoardDownload();

const isCookieError = (err: MinihoardError) =>
  err.kind === "cookie_missing" || err.kind === "cookie_expired";

/* ---------- shared raw console (legacy UI body + modern debug log) ---------- */

type ConsoleLine = { text: string; isErr: boolean };
const lines = ref<ConsoleLine[]>([]);
const activeJobId = ref<string | null>(null);
/* The process can print before runMinihoard's response delivers the job
   id, so events can't be matched by id alone. The backend allows exactly
   one run at a time — while this view is busy, every event is ours. */
const launching = ref(false);
const busy = computed(() => launching.value || !!activeJobId.value);
const consoleEl = ref<HTMLElement | null>(null);

const appendLine = (line: ConsoleLine) => {
  lines.value.push(line);
  // keep the console from growing unbounded over a long session
  if (lines.value.length > 2000)
    lines.value.splice(0, lines.value.length - 2000);
  nextTick(() => {
    consoleEl.value?.scrollTo({ top: consoleEl.value.scrollHeight });
  });
};

const run = async (args: string[]) => {
  if (!info.value || busy.value) return;
  launching.value = true;
  legacyCookieHint.value = false;
  lines.value = [];
  appendLine({ text: `$ minihoard ${args.join(" ")}`, isErr: false });
  const result = await commands.runMinihoard(info.value.path, args);
  if (result.status === "ok") {
    // a fast command can finish before this response lands — the listener
    // already cleared launching in that case, and the run is over
    if (launching.value) activeJobId.value = result.data;
  } else {
    launching.value = false;
    toastStore.reportError("Failed to launch minihoard", result.error);
  }
};

const cancel = async () => {
  if (!activeJobId.value) return;
  await commands.cancelMinihoard(activeJobId.value);
};

let unlisten: (() => void) | undefined;
onMounted(async () => {
  unlisten = await events.minihoardStatus.listen((event) => {
    if (!busy.value) return; // stragglers from a cancelled run
    const status = event.payload;
    if ("Line" in status) {
      const { line } = status.Line;
      if (COOKIE_FAILURE_SIGNATURES.some((sig) => line.includes(sig))) {
        legacyCookieHint.value = true;
      }
      appendLine({ text: line, isErr: status.Line.is_err });
    } else if ("Finished" in status) {
      activeJobId.value = null;
      launching.value = false;
      if (status.Finished.error) {
        appendLine({ text: status.Finished.error, isErr: true });
      }
      appendLine({
        text: status.Finished.success ? "— done —" : "— stopped —",
        isErr: !status.Finished.success,
      });
    }
  });
});
onUnmounted(() => unlisten?.());

/* Auto-retry the list once a "sync cookie" run (fired from either the
   modern banner/header button or a manual raw run) settles — whichever
   button started it, this is the one place that reacts to completion. */
const syncingCookie = ref(false);
const syncCookie = () => {
  syncingCookie.value = true;
  run(["sync-cookie"]);
};
watch(busy, (isBusy, wasBusy) => {
  if (wasBusy && !isBusy && syncingCookie.value) {
    syncingCookie.value = false;
    if (info.value?.supports_json) refreshList();
  }
});

/* ---------- legacy (< 0.4.0) fallback ---------- */

/* The one place the legacy console peeks at minihoard's text: its
   cookie-auth failures have a stable phrasing, and recognizing them lets
   us offer the fix (`sync-cookie`, non-interactive) as a button instead of
   telling the user to go find a terminal. Degrades gracefully — if the
   phrasing ever changes we just don't show the button; the raw error still
   streams in. Only the legacy path needs this: runMinihoard reports plain
   AppError, not the typed MinihoardError kinds the modern commands use. */
const COOKIE_FAILURE_SIGNATURES = [
  "session cookie is missing or expired",
  "no session cookie",
];
const legacyCookieHint = ref(false);
const fetchInput = ref("");

const currentMonth = () => {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}`;
};

const presets = [
  { label: "What's new", args: ["list", "--undownloaded"] },
  { label: "This month", args: ["list", "--month", currentMonth()] },
  { label: "Account", args: ["whoami"] },
  { label: "Folders", args: ["config"] },
];

const fetchTargets = async () => {
  const targets = fetchInput.value.trim().split(/\s+/).filter(Boolean);
  if (!targets.length) return;
  await run(["download", ...targets]);
};

/* ---------- modern (>= 0.4.0) library UI ---------- */

const health = ref<MinihoardHealth | null>(null);
const statusLoading = ref(false);
const listLoading = ref(false);
const entries = ref<MinihoardEntry[]>([]);
/* Set only for cookie-shaped errors (list or a download run) — anything
   else goes straight to a toast. */
const listError = ref<MinihoardError | null>(null);

const refreshStatus = async () => {
  if (!info.value) return;
  statusLoading.value = true;
  const result = await commands.minihoardStatus(info.value.path);
  statusLoading.value = false;
  if (result.status === "ok") {
    health.value = result.data;
  } else {
    health.value = null;
    // status never proves cookie validity (presence/age only, per
    // MinihoardHealth's doc comment) — only the list call's real request
    // is trusted to raise the cookie banner.
    if (!isCookieError(result.error)) {
      toastStore.addToast(`Minihoard: ${result.error.message}`, "error", 0);
    }
  }
};

const refreshList = async () => {
  if (!info.value) return;
  listLoading.value = true;
  const result = await commands.minihoardList(info.value.path);
  listLoading.value = false;
  if (result.status === "ok") {
    entries.value = result.data;
    listError.value = null;
  } else {
    entries.value = [];
    if (isCookieError(result.error)) {
      listError.value = result.error;
    } else {
      listError.value = null;
      toastStore.addToast(`Minihoard: ${result.error.message}`, "error", 0);
    }
  }
};

const refreshAll = async () => {
  await refreshStatus();
  await refreshList();
};

watch(
  () => info.value,
  (val) => {
    if (val?.supports_json) refreshAll();
  },
  { immediate: true },
);

const accountLabel = computed(() => {
  if (!health.value) return statusLoading.value ? "checking…" : "—";
  if (!health.value.oauth_ok || !health.value.username) return "not logged in";
  return `@${health.value.username}`;
});

/* --- filters (all client-side over the buffered list) --- */

const search = ref("");
const creatorFilter = ref("");
const monthFilter = ref(""); // "" = all, "undated" = null bucket, else "YYYYMM"
const sourceFilter = ref("");
const notDownloadedOnly = ref(false);

const distinctSorted = (values: Iterable<string>) =>
  [...new Set(values)].toSorted((a, b) => a.localeCompare(b));

const creators = computed(() =>
  distinctSorted(
    entries.value.map((e) => e.creator).filter((c): c is string => !!c),
  ),
);
const sources = computed(() =>
  distinctSorted(
    entries.value.map((e) => e.source).filter((s): s is string => !!s),
  ),
);
const months = computed(() => {
  const set = new Set<string>();
  let hasUndated = false;
  for (const e of entries.value) {
    const m = entryMonth(e);
    if (m) set.add(m);
    else hasUndated = true;
  }
  return { sorted: [...set].toSorted().toReversed(), hasUndated };
});

/** "YYYYMM" -> "YYYY-MM". */
const formatMonth = (ym: string) => `${ym.slice(0, 4)}-${ym.slice(4, 6)}`;

/* The best "when is this from?" month for an entry: the tribe release month if
   present (only ~12% of the library), else the publication date, else the
   creation date (always present). So the column shows a real date instead of a
   dash for the Patreon/Kickstarter shares and purchases that carry no release
   month. */
const isoToYearMonth = (iso: string) => iso.slice(0, 4) + iso.slice(5, 7);
const entryMonth = (e: MinihoardEntry): string | null => {
  if (e.yearmonth) return e.yearmonth;
  if (e.published_at) return isoToYearMonth(e.published_at);
  if (e.created_at) return isoToYearMonth(e.created_at);
  return null;
};
/* Which date entryMonth came from, so a release month and a publication date
   aren't silently conflated — surfaced as a tooltip on the cell. */
const monthTitle = (e: MinihoardEntry): string => {
  if (e.yearmonth) return "Tribe release month";
  if (e.published_at) return `Published ${e.published_at.slice(0, 10)}`;
  if (e.created_at) return `Created ${e.created_at.slice(0, 10)}`;
  return "no date";
};

/* Source channel codes → friendlier labels; the raw code stays as a tooltip.
   Unknown codes pass through as-is. */
const SOURCE_LABELS: Record<string, string> = {
  TRIBE: "Tribes",
  USER_GROUP: "Shared",
  PURCHASE: "Purchase",
  FRONTIER: "Frontier",
};
const sourceLabel = (s: string) => SOURCE_LABELS[s] ?? s;
const sourceTitle = (s: string) =>
  s === "USER_GROUP"
    ? "Shared by the creator (usually Patreon/Kickstarter) — USER_GROUP"
    : s === "TRIBE"
      ? "MyMiniFactory Tribes — TRIBE"
      : s;

const filteredEntries = computed(() => {
  const q = search.value.trim().toLowerCase();
  return entries.value.filter((e) => {
    if (notDownloadedOnly.value && e.downloaded) return false;
    if (creatorFilter.value && e.creator !== creatorFilter.value) return false;
    if (sourceFilter.value && e.source !== sourceFilter.value) return false;
    if (monthFilter.value === "undated") {
      if (entryMonth(e)) return false;
    } else if (monthFilter.value && entryMonth(e) !== monthFilter.value) {
      return false;
    }
    if (q) {
      const haystack =
        `${e.name} ${e.creator ?? ""} ${e.tags.join(" ")}`.toLowerCase();
      if (!haystack.includes(q)) return false;
    }
    return true;
  });
});

/* --- incremental paging (no virtualization; 100 at a time) --- */

const PAGE_SIZE = 100;
const visibleCount = ref(PAGE_SIZE);
watch(
  [search, creatorFilter, monthFilter, sourceFilter, notDownloadedOnly],
  () => {
    visibleCount.value = PAGE_SIZE;
  },
);
const visibleEntries = computed(() =>
  filteredEntries.value.slice(0, visibleCount.value),
);
const showMore = () => {
  visibleCount.value += PAGE_SIZE;
};

/* --- selection --- */

const selectedIds = ref<Set<number>>(new Set());
const isSelected = (id: number) => selectedIds.value.has(id);
const toggleSelect = (id: number) => {
  if (selectedIds.value.has(id)) selectedIds.value.delete(id);
  else selectedIds.value.add(id);
};
const selectAllFiltered = () => {
  for (const e of filteredEntries.value) selectedIds.value.add(e.id);
};
const clearSelection = () => selectedIds.value.clear();
const selectedCount = computed(() => selectedIds.value.size);

/* --- row expansion: lazy per-object detail (page url + preview image) --- */

type ObjectDetail = {
  loading: boolean;
  data: MinihoardObject | null;
  error: boolean;
};
const expandedIds = ref<Set<number>>(new Set());
const objectCache = ref<Record<number, ObjectDetail>>({});

const toggleExpand = (id: number) => {
  if (expandedIds.value.has(id)) {
    expandedIds.value.delete(id);
    return;
  }
  expandedIds.value.add(id);
  // The listing has no image or page url — fetch detail once per row, on
  // demand (never an eager sweep over the whole library).
  if (!objectCache.value[id] && info.value) {
    fetchObjectDetail(id);
  }
};

const fetchObjectDetail = async (id: number) => {
  if (!info.value) return;
  objectCache.value[id] = { loading: true, data: null, error: false };
  const result = await commands.minihoardObject(info.value.path, id);
  objectCache.value[id] =
    result.status === "ok"
      ? { loading: false, data: result.data, error: false }
      : { loading: false, data: null, error: true };
};

const openObjectPage = (id: number) => {
  const url = objectCache.value[id]?.data?.url;
  if (url) openUrl(url);
};

/* --- download queue --- */

const startDownload = async () => {
  if (!info.value || download.isRunning.value || selectedCount.value === 0)
    return;
  const ids = [...selectedIds.value];
  const result = await download.start(info.value.path, ids);
  if (result.status !== "ok") {
    if (isCookieError(result.error)) {
      listError.value = result.error;
    } else {
      toastStore.addToast(`Minihoard: ${result.error.message}`, "error", 0);
    }
    return;
  }
  clearSelection();
};

const cancelDownload = () => download.cancel();

/* Catalog hand-off: minihoard lands releases in its own library dir, which
   Plinth doesn't watch. After a run drops files, offer to fold that folder
   into the catalog — start_catalog_scan registers the root itself (and
   rejects an overlap), so this is one call, not add-then-scan. */
const libraryDir = computed(() => health.value?.library_dir ?? null);
const scanningCatalog = ref(false);
const canScanIntoCatalog = computed(
  () =>
    !!libraryDir.value &&
    !!download.finishedSummary.value &&
    download.finishedSummary.value.ok > 0,
);
const scanIntoCatalog = async () => {
  if (!libraryDir.value || scanningCatalog.value) return;
  scanningCatalog.value = true;
  // start_catalog_scan takes AppError, not MinihoardError, so reportError's
  // describeError handles it correctly here (unlike the typed commands).
  const result = await commands.startCatalogScan(libraryDir.value);
  scanningCatalog.value = false;
  if (result.status === "ok") {
    toastStore.addToast(
      "Scanning your library into the catalog — watch the Catalog tab for progress.",
      "success",
    );
  } else {
    toastStore.reportError("Couldn't scan into the catalog", result.error);
  }
};

const queueDoneCount = computed(
  () => download.queue.value.filter((i) => i.done || i.failed).length,
);
const showQueue = computed(
  () =>
    download.queue.value.length > 0 ||
    download.isRunning.value ||
    !!download.finishedSummary.value ||
    download.cancelled.value,
);

watch(
  () => download.status.value,
  (status) => {
    if (!status) return;
    if ("Finished" in status) {
      const finishedIds = new Set(download.doneIds.value);
      for (const e of entries.value) {
        if (finishedIds.has(e.id)) e.downloaded = true;
      }
      const { ok, failed } = status.Finished;
      toastStore.addToast(
        `Minihoard: ${ok} downloaded${failed ? `, ${failed} failed` : ""}`,
        failed ? "warning" : "success",
      );
    } else if ("Failed" in status) {
      const err = status.Failed.error;
      if (isCookieError(err)) {
        listError.value = err;
      } else {
        toastStore.addToast(`Minihoard: ${err.message}`, "error", 0);
      }
    } else if ("Cancelled" in status) {
      toastStore.addToast("Minihoard: download cancelled", "info");
    }
  },
);
</script>

<template>
  <!-- no binary: the sidebar already gates the tab on detection, this is
       just a defensive fallback if the view ever mounts without it -->
  <div
    v-if="!info"
    class="flex flex-col h-full min-h-0 p-6 items-center justify-center text-base-content/40 text-[12px]"
  >
    Minihoard not detected.
  </div>

  <!-- old binary: nothing to browse, --json doesn't exist yet -->
  <div
    v-else-if="!info.supports_json"
    class="flex flex-col h-full min-h-0 p-6 gap-4"
  >
    <div class="flex items-baseline gap-3">
      <h1 class="font-display text-[17px] tracking-wider">MINIHOARD</h1>
      <span class="font-mono text-[11px] text-base-content/40"
        >v{{ info.version }}</span
      >
    </div>
    <div class="alert alert-warning text-xs">
      Update minihoard to v0.4+ to use the library browser — this build (v{{
        info.version
      }}) only speaks plain text, so Plinth falls back to the raw console below.
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <button
        v-for="preset in presets"
        :key="preset.label"
        type="button"
        class="btn btn-sm"
        :disabled="busy"
        @click="run(preset.args)"
      >
        {{ preset.label }}
      </button>

      <span class="flex-1"></span>

      <input
        v-model="fetchInput"
        type="text"
        placeholder="object ids or names, e.g. 806054 or “dragon”"
        class="input input-sm input-bordered w-72 font-mono text-[12px]"
        :disabled="busy"
        @keydown.enter="fetchTargets"
      />
      <button
        type="button"
        class="btn btn-sm btn-primary"
        :disabled="busy || !fetchInput.trim()"
        @click="fetchTargets"
      >
        Fetch
      </button>
      <button
        v-if="activeJobId"
        type="button"
        class="btn btn-sm btn-error"
        @click="cancel"
      >
        Stop
      </button>
    </div>

    <div
      v-if="legacyCookieHint && !busy"
      class="alert alert-warning text-xs items-center"
    >
      <span class="flex-1">
        Your MyMiniFactory session has expired. Plinth can pull a fresh cookie
        from a browser you're logged in to — or run
        <code>minihoard set-cookie</code> in a terminal to paste one manually.
      </span>
      <button type="button" class="btn btn-sm btn-primary" @click="syncCookie">
        Sync cookie from browser
      </button>
    </div>

    <div
      ref="consoleEl"
      class="flex-1 min-h-0 overflow-y-auto bg-base-300 border border-base-content/10 rounded-box p-3 font-mono text-[11.5px] leading-[1.65] whitespace-pre-wrap"
    >
      <div v-if="!lines.length" class="text-base-content/35">
        Pick an action above — output streams here, exactly as the CLI prints
        it.
      </div>
      <div
        v-for="(line, index) in lines"
        :key="index"
        :class="line.isErr ? 'text-base-content/45' : ''"
      >
        {{ line.text }}
      </div>
      <div v-if="busy" class="text-primary animate-pulse">▍</div>
    </div>
  </div>

  <!-- modern (>= 0.4.0): the clickable library -->
  <div v-else class="flex flex-col h-full min-h-0 p-6 gap-3">
    <div class="flex items-baseline gap-3 flex-wrap">
      <h1 class="font-display text-[17px] tracking-wider" :title="info.path">
        MINIHOARD
      </h1>
      <span class="font-mono text-[11px] text-base-content/40"
        >v{{ info.version }}</span
      >
      <span class="flex-1"></span>
      <span
        class="font-mono text-[11px]"
        :class="
          health?.oauth_ok && health.username
            ? 'text-success'
            : 'text-base-content/40'
        "
        >{{ accountLabel }}</span
      >
      <button
        type="button"
        class="btn btn-xs"
        :disabled="busy"
        @click="syncCookie"
      >
        Sync cookie
      </button>
      <button
        type="button"
        class="btn btn-xs"
        :disabled="statusLoading || listLoading"
        @click="refreshAll"
      >
        Refresh
      </button>
    </div>

    <div
      v-if="listError && isCookieError(listError)"
      class="alert alert-warning text-xs items-center"
    >
      <span class="flex-1">
        Your MyMiniFactory session
        {{
          listError.kind === "cookie_missing"
            ? "has no stored cookie"
            : "has expired"
        }}. Plinth can pull a fresh cookie from a browser you're logged in to.
      </span>
      <button
        type="button"
        class="btn btn-sm btn-primary"
        :disabled="busy"
        @click="syncCookie"
      >
        Sync cookie from browser
      </button>
    </div>

    <!-- filters -->
    <div class="flex flex-wrap items-center gap-2">
      <input
        v-model="search"
        type="search"
        placeholder="search name, tags, creator…"
        class="input input-sm input-bordered flex-1 min-w-48 font-mono text-[12px]"
      />
      <select v-model="creatorFilter" class="select select-sm w-40 text-[11px]">
        <option value="">All creators</option>
        <option v-for="c in creators" :key="c" :value="c">{{ c }}</option>
      </select>
      <select v-model="monthFilter" class="select select-sm w-36 text-[11px]">
        <option value="">All months</option>
        <option v-if="months.hasUndated" value="undated">Undated</option>
        <option v-for="m in months.sorted" :key="m" :value="m">
          {{ formatMonth(m) }}
        </option>
      </select>
      <select v-model="sourceFilter" class="select select-sm w-36 text-[11px]">
        <option value="">All sources</option>
        <option v-for="s in sources" :key="s" :value="s">
          {{ sourceLabel(s) }}
        </option>
      </select>
      <label class="flex items-center gap-1.5 text-[11px] text-base-content/60">
        <input
          v-model="notDownloadedOnly"
          type="checkbox"
          class="toggle toggle-sm"
        />
        not downloaded only
      </label>
    </div>

    <!-- selection / download bar -->
    <div class="flex items-center gap-2 text-[12px]">
      <button
        type="button"
        class="btn btn-xs"
        :disabled="!filteredEntries.length"
        @click="selectAllFiltered"
      >
        Select all filtered ({{ filteredEntries.length }})
      </button>
      <button
        type="button"
        class="btn btn-xs"
        :disabled="!selectedCount"
        @click="clearSelection"
      >
        Clear
      </button>
      <span class="flex-1"></span>
      <span v-if="selectedCount" class="text-base-content/60"
        >{{ selectedCount }} selected</span
      >
      <button
        type="button"
        class="btn btn-sm btn-primary"
        :disabled="!selectedCount || download.isRunning.value"
        @click="startDownload"
      >
        Download
      </button>
      <button
        v-if="download.isRunning.value"
        type="button"
        class="btn btn-sm btn-error"
        @click="cancelDownload"
      >
        Cancel
      </button>
    </div>

    <!-- download queue -->
    <div
      v-if="showQueue"
      class="rounded-box border border-base-content/10 bg-base-200/30 p-3 max-h-56 overflow-y-auto flex flex-col gap-1.5 shrink-0"
    >
      <div
        class="flex items-center justify-between text-[11px] text-base-content/50 mb-1"
      >
        <span
          >Download queue — {{ queueDoneCount }} /
          {{ download.total.value }}</span
        >
        <div class="flex items-center gap-2">
          <span v-if="download.finishedSummary.value">
            {{ download.finishedSummary.value.ok }} done,
            {{ download.finishedSummary.value.failed }} failed
          </span>
          <span v-else-if="download.cancelled.value" class="text-warning"
            >cancelled</span
          >
          <button
            v-if="canScanIntoCatalog"
            type="button"
            class="btn btn-xs btn-primary"
            :disabled="scanningCatalog"
            @click="scanIntoCatalog"
          >
            Scan into catalog
          </button>
        </div>
      </div>
      <div
        v-for="item in download.queue.value"
        :key="item.id"
        class="flex items-center gap-2"
      >
        <span class="flex-1 truncate text-[11.5px]" :title="item.name">{{
          item.name
        }}</span>
        <span
          v-if="item.failed"
          class="text-error text-[10.5px]"
          :title="item.reason ?? undefined"
          >failed</span
        >
        <span v-else-if="item.done" class="text-success text-[10.5px]">✓</span>
        <template v-else>
          <progress
            class="progress progress-primary w-32"
            :value="item.bytesTotal ? item.bytesDone : undefined"
            :max="item.bytesTotal ?? undefined"
          ></progress>
          <span
            class="font-mono text-[10px] text-base-content/40 w-16 text-right"
          >
            {{
              item.bytesTotal
                ? `${Math.round((item.bytesDone / item.bytesTotal) * 100)}%`
                : formatFileSize(item.bytesDone)
            }}
          </span>
        </template>
      </div>
    </div>

    <!-- rows -->
    <div
      class="flex-1 min-h-0 overflow-y-auto rounded-box border border-base-content/10"
    >
      <table v-if="visibleEntries.length" class="table table-xs w-full">
        <thead class="sticky top-0 bg-base-200 z-10">
          <tr>
            <th class="w-6"></th>
            <th>Name</th>
            <th>Creator</th>
            <th>Month</th>
            <th>Source</th>
            <th class="w-6"></th>
          </tr>
        </thead>
        <tbody>
          <template v-for="e in visibleEntries" :key="e.id">
            <tr
              class="hover cursor-pointer"
              :class="expandedIds.has(e.id) ? 'bg-base-200/40' : ''"
              @click="toggleExpand(e.id)"
            >
              <td @click.stop>
                <input
                  type="checkbox"
                  class="checkbox checkbox-xs"
                  :checked="isSelected(e.id)"
                  @change="toggleSelect(e.id)"
                />
              </td>
              <td class="truncate max-w-72" :title="e.name">
                <span
                  class="inline-block w-3 text-base-content/40 transition-transform"
                  :class="expandedIds.has(e.id) ? 'rotate-90' : ''"
                  >›</span
                >
                {{ e.name }}
              </td>
              <td class="text-base-content/60">{{ e.creator ?? "—" }}</td>
              <td
                class="font-mono text-[10.5px]"
                :class="
                  e.yearmonth ? 'text-base-content/60' : 'text-base-content/40'
                "
                :title="monthTitle(e)"
              >
                {{ entryMonth(e) ? formatMonth(entryMonth(e)!) : "—" }}
              </td>
              <td>
                <span
                  v-if="e.source"
                  class="badge badge-xs badge-ghost"
                  :title="sourceTitle(e.source)"
                  >{{ sourceLabel(e.source) }}</span
                >
              </td>
              <td class="text-success text-center">
                {{ e.downloaded ? "✓" : "" }}
              </td>
            </tr>
            <tr v-if="expandedIds.has(e.id)" class="bg-base-200/40">
              <td :colspan="6" class="p-0">
                <div class="flex gap-3 p-3">
                  <!-- preview image (lazy) -->
                  <div
                    class="w-28 h-28 shrink-0 rounded-box overflow-hidden bg-base-300 border border-base-content/10 flex items-center justify-center"
                  >
                    <span
                      v-if="objectCache[e.id]?.loading"
                      class="loading loading-spinner loading-sm text-base-content/30"
                    ></span>
                    <img
                      v-else-if="objectCache[e.id]?.data?.thumbnail_url"
                      :src="objectCache[e.id]?.data?.thumbnail_url ?? undefined"
                      :alt="e.name"
                      class="w-full h-full object-cover"
                      loading="lazy"
                    />
                    <span v-else class="text-[10px] text-base-content/30"
                      >no preview</span
                    >
                  </div>

                  <div class="flex-1 min-w-0 flex flex-col gap-2">
                    <div v-if="e.tags.length" class="flex flex-wrap gap-1">
                      <span
                        v-for="tag in e.tags.slice(0, 12)"
                        :key="tag"
                        class="badge badge-xs badge-ghost"
                        >{{ tag }}</span
                      >
                    </div>
                    <span v-else class="text-[11px] text-base-content/40"
                      >no tags</span
                    >

                    <div class="flex items-center gap-2 mt-auto">
                      <button
                        type="button"
                        class="btn btn-xs"
                        :disabled="!objectCache[e.id]?.data?.url"
                        @click.stop="openObjectPage(e.id)"
                      >
                        View on MyMiniFactory ↗
                      </button>
                      <span
                        v-if="objectCache[e.id]?.error"
                        class="text-[10.5px] text-error"
                        >couldn't load details (needs minihoard 0.4.1+)</span
                      >
                      <span class="font-mono text-[10px] text-base-content/30"
                        >#{{ e.id }}</span
                      >
                    </div>
                  </div>
                </div>
              </td>
            </tr>
          </template>
        </tbody>
      </table>
      <div v-else class="p-6 text-center text-base-content/40 text-[12px]">
        {{
          listLoading
            ? "Loading your library…"
            : entries.length
              ? "No objects match these filters."
              : "Your library is empty."
        }}
      </div>
    </div>

    <div
      class="flex items-center justify-between text-[11px] text-base-content/50 shrink-0"
    >
      <span>{{ filteredEntries.length }} of {{ entries.length }} objects</span>
      <button
        v-if="visibleEntries.length < filteredEntries.length"
        type="button"
        class="btn btn-xs"
        @click="showMore"
      >
        Show
        {{
          Math.min(PAGE_SIZE, filteredEntries.length - visibleEntries.length)
        }}
        more
      </button>
    </div>

    <!-- debug log: demoted raw console, used by sync-cookie and any raw runs -->
    <details
      class="collapse collapse-arrow border border-base-content/10 bg-base-200/20 rounded-box shrink-0"
    >
      <summary
        class="collapse-title min-h-0 py-2 px-3 flex items-center gap-2 cursor-pointer"
      >
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >LOG</span
        >
      </summary>
      <div class="collapse-content px-3">
        <div
          ref="consoleEl"
          class="max-h-48 overflow-y-auto bg-base-300 border border-base-content/10 rounded-box p-3 font-mono text-[11px] leading-[1.6] whitespace-pre-wrap"
        >
          <div v-if="!lines.length" class="text-base-content/35">
            No output yet.
          </div>
          <div
            v-for="(line, index) in lines"
            :key="index"
            :class="line.isErr ? 'text-base-content/45' : ''"
          >
            {{ line.text }}
          </div>
          <div v-if="busy" class="text-primary animate-pulse">▍</div>
        </div>
      </div>
    </details>
  </div>
</template>
