<template>
  <div class="flex flex-wrap items-center gap-2">
    <label class="input input-sm flex-1 min-w-48 items-center gap-2">
      <svg
        width="13"
        height="13"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        class="opacity-40"
      >
        <circle cx="11" cy="11" r="7"></circle>
        <path d="M21 21l-4.3-4.3"></path>
      </svg>
      <input
        ref="searchInput"
        type="search"
        class="grow font-mono"
        placeholder="query models, tags…"
        v-model="query"
      />
      <span
        class="font-mono text-[9.5px] text-base-content/40 border border-base-content/15 rounded px-1 shrink-0"
      >
        /
      </span>
    </label>
    <div
      class="flex bg-base-200 border border-base-content/10 rounded-lg p-0.5"
    >
      <button
        type="button"
        class="flex items-center gap-1.5 font-semibold text-[11px] px-2.5 py-1 rounded-md cursor-pointer"
        :class="
          viewMode === 'list'
            ? 'bg-primary text-primary-content'
            : 'text-base-content/60'
        "
        @click="viewMode = 'list'"
      >
        <svg
          width="13"
          height="13"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <line x1="8" y1="6" x2="20" y2="6"></line>
          <line x1="8" y1="12" x2="20" y2="12"></line>
          <line x1="8" y1="18" x2="20" y2="18"></line>
          <line x1="3.5" y1="6" x2="3.51" y2="6"></line>
          <line x1="3.5" y1="12" x2="3.51" y2="12"></line>
          <line x1="3.5" y1="18" x2="3.51" y2="18"></line>
        </svg>
        List
      </button>
      <button
        type="button"
        class="flex items-center gap-1.5 font-semibold text-[11px] px-2.5 py-1 rounded-md cursor-pointer"
        :class="
          viewMode === 'grid'
            ? 'bg-primary text-primary-content'
            : 'text-base-content/60'
        "
        @click="viewMode = 'grid'"
      >
        <svg
          width="13"
          height="13"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <rect x="3" y="3" width="7" height="7"></rect>
          <rect x="14" y="3" width="7" height="7"></rect>
          <rect x="3" y="14" width="7" height="7"></rect>
          <rect x="14" y="14" width="7" height="7"></rect>
        </svg>
        Grid
      </button>
    </div>
    <select
      v-model="groupMode"
      class="select select-sm w-40 font-medium text-[11px]"
      title="How the catalog is ordered"
    >
      <option value="none">Sort: A–Z</option>
      <option value="designer">Group: designer › release</option>
      <option value="designer-date">Group: designer › newest</option>
      <option value="height">Sort: tallest first</option>
      <option value="volume">Sort: largest first</option>
    </select>
    <select
      v-model="designerFilter"
      class="select select-sm w-44 font-medium text-[11px]"
      title="Show only this designer's models"
    >
      <option value="">All designers</option>
      <option v-for="d in designers" :key="d.designer" :value="d.designer">
        {{ d.designer }} ({{ d.model_count }})
      </option>
    </select>
    <div class="dropdown">
      <button
        type="button"
        tabindex="0"
        class="btn btn-sm gap-1.5 font-medium text-[11px]"
        title="Filter by physical size — mined by the geometry scan"
      >
        Size
        <span
          v-if="hasGeometryFilter"
          class="w-1.5 h-1.5 rounded-full bg-primary"
          aria-hidden="true"
        ></span>
      </button>
      <div
        tabindex="0"
        class="dropdown-content menu z-30 mt-1 w-64 rounded-box bg-base-200 p-3 gap-1 shadow-lg"
      >
        <div
          class="font-mono text-[10px] tracking-wide text-base-content/40 uppercase px-1"
        >
          Height (mm)
        </div>
        <div class="flex items-center gap-1.5 px-1 mb-2">
          <NumberInput
            id="geo-height-min"
            v-model="heightMinMm"
            placeholder="min"
            :min="0"
            class="flex-1 min-w-0"
          />
          <span class="opacity-40 shrink-0">–</span>
          <NumberInput
            id="geo-height-max"
            v-model="heightMaxMm"
            placeholder="max"
            :min="0"
            class="flex-1 min-w-0"
          />
        </div>
        <div
          class="font-mono text-[10px] tracking-wide text-base-content/40 uppercase px-1"
        >
          Volume (ml)
        </div>
        <div class="flex items-center gap-1.5 px-1 mb-1.5">
          <NumberInput
            id="geo-volume-min"
            v-model="volumeMinMl"
            placeholder="min"
            :min="0"
            class="flex-1 min-w-0"
          />
          <span class="opacity-40 shrink-0">–</span>
          <NumberInput
            id="geo-volume-max"
            v-model="volumeMaxMl"
            placeholder="max"
            :min="0"
            class="flex-1 min-w-0"
          />
        </div>
        <button
          v-if="hasGeometryFilter"
          type="button"
          class="btn btn-ghost btn-xs self-start"
          @click="clearGeometryFilter"
        >
          Clear
        </button>
      </div>
    </div>
    <span class="flex-1"></span>
    <div class="join">
      <div class="dropdown">
        <button
          type="button"
          tabindex="0"
          class="btn btn-sm join-item gap-1.5 font-mono font-normal"
          :title="
            roots.length
              ? roots.map((r) => r.root).join('\n')
              : 'No catalog folders yet'
          "
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.7"
          >
            <path
              d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"
            ></path>
          </svg>
          {{ roots.length }}
          <svg
            width="9"
            height="9"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2.5"
            class="opacity-55"
          >
            <path d="M6 9l6 6 6-6"></path>
          </svg>
        </button>
        <div
          tabindex="0"
          class="dropdown-content menu z-30 mt-1 w-110 rounded-box bg-base-200 p-2 shadow-lg"
        >
          <div
            v-if="!roots.length"
            class="px-2 py-1.5 text-[11px] text-base-content/50"
          >
            No catalog folders yet — add one designer folder at a time; each
            scans on its own.
          </div>
          <div
            v-for="r in roots"
            :key="r.root"
            class="flex items-center gap-2 px-2 py-1.5"
          >
            <button
              type="button"
              class="btn btn-ghost btn-xs px-1"
              :class="r.primary ? 'text-warning' : 'text-base-content/30'"
              :title="
                r.primary
                  ? 'Primary folder — Clean up moves every folder\'s models into this one. Click to unset.'
                  : 'Make this the primary folder: Clean up will move models from every folder into it'
              "
              @click="togglePrimary(r)"
            >
              {{ r.primary ? "★" : "☆" }}
            </button>
            <div class="min-w-0 flex-1">
              <div class="truncate font-mono text-[11px]" :title="r.root">
                {{ r.root }}
              </div>
              <div class="text-[10px] text-base-content/40">
                {{ r.model_count }} models ·
                {{ formatFileSize(r.total_size_bytes) }} ·
                {{
                  r.last_scan_epoch
                    ? `scanned ${new Date(r.last_scan_epoch * 1000).toLocaleDateString()}`
                    : "never scanned"
                }}
              </div>
            </div>
            <button
              type="button"
              class="btn btn-ghost btn-xs"
              :disabled="isScanning"
              @click="scanRoot(r.root)"
            >
              Scan
            </button>
            <button
              type="button"
              class="btn btn-ghost btn-xs text-error"
              :disabled="isScanning"
              title="Remove from the catalog — files on disk are untouched"
              @click="removeRoot(r.root)"
            >
              ✕
            </button>
          </div>
          <div
            class="mt-1.5 flex items-center gap-2 border-t border-base-content/10 px-2 pt-2 pb-1"
          >
            <button
              type="button"
              class="btn btn-ghost btn-xs text-primary gap-1"
              :disabled="isScanning"
              @click="addFolder"
            >
              <svg
                width="13"
                height="13"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
              >
                <path d="M12 5v14M5 12h14"></path>
              </svg>
              Add folder…
            </button>
            <span class="flex-1"></span>
            <span class="font-mono text-[10px] text-base-content/40">
              indexed to catalog.db
            </span>
          </div>
        </div>
      </div>
      <button
        v-if="!isScanning"
        type="button"
        class="btn btn-sm btn-primary join-item gap-1.5"
        :disabled="!hasRoots"
        title="Rescan every catalog folder, one at a time"
        @click="scanAll"
      >
        <svg
          width="13"
          height="13"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <path d="M21 12a9 9 0 1 1-3-6.7L21 8"></path>
          <path d="M21 3v5h-5"></path>
        </svg>
        Scan
      </button>
      <button
        v-else
        type="button"
        class="btn btn-sm btn-error join-item"
        @click="cancelAllScans"
      >
        Cancel
      </button>
    </div>
    <div class="dropdown dropdown-end">
      <button
        type="button"
        tabindex="0"
        class="btn btn-sm gap-1.5"
        title="Bulk & maintenance actions — Clean up, Pack, Render previews"
      >
        <svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor">
          <circle cx="5" cy="12" r="1.7"></circle>
          <circle cx="12" cy="12" r="1.7"></circle>
          <circle cx="19" cy="12" r="1.7"></circle>
        </svg>
        Actions
      </button>
      <div
        tabindex="0"
        class="dropdown-content menu z-30 mt-1 w-80 rounded-box bg-base-200 p-2 shadow-lg gap-0.5"
      >
        <div
          class="px-2 pt-1 pb-1.5 font-mono text-[10px] tracking-wide text-base-content/40 uppercase"
        >
          Library actions<span v-if="designerFilter">
            · {{ designerFilter }}</span
          >
        </div>
        <button
          type="button"
          class="flex flex-col items-start gap-0.5 rounded-lg px-2 py-2 text-left hover:bg-base-300 disabled:opacity-40 disabled:hover:bg-transparent"
          :disabled="!hasRoots || isScanning"
          @click="openNormalize()"
        >
          <span class="text-[12px] font-semibold">Clean up…</span>
          <span class="text-[10.5px] leading-snug text-base-content/50">
            Restructure folders on disk to match the curated catalog — you
            review every move first
          </span>
        </button>
        <button
          type="button"
          class="flex flex-col items-start gap-0.5 rounded-lg px-2 py-2 text-left hover:bg-base-300 disabled:opacity-40 disabled:hover:bg-transparent"
          :disabled="!hasRoots || isScanning || isPacking"
          @click="bulkPack()"
        >
          <span class="text-[12px] font-semibold">Pack…</span>
          <span class="text-[10.5px] leading-snug text-base-content/50">
            Compress models into pack archives to save disk space — scoped to
            the designer filter when one is set. Safe to cancel; re-running
            resumes.
          </span>
        </button>
        <button
          type="button"
          class="flex flex-col items-start gap-0.5 rounded-lg px-2 py-2 text-left hover:bg-base-300 disabled:opacity-40 disabled:hover:bg-transparent"
          :disabled="!hasRoots || isScanning || isPacking || isBatchRendering"
          @click="openBatchRender()"
        >
          <span class="text-[12px] font-semibold">Render previews…</span>
          <span class="text-[10.5px] leading-snug text-base-content/50">
            Render missing catalog previews in ONE Blender launch — scoped to
            the designer filter when one is set. Finished previews survive a
            cancel.
          </span>
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { storeToRefs } from "pinia";
import NumberInput from "../NumberInput.vue";
import { useCatalogStore } from "../../stores/catalogStore";
import { formatFileSize } from "../../utils/format";

const store = useCatalogStore();
const {
  query,
  viewMode,
  groupMode,
  designerFilter,
  designers,
  heightMinMm,
  heightMaxMm,
  volumeMinMm3,
  volumeMaxMm3,
  hasGeometryFilter,
  roots,
  isScanning,
  hasRoots,
  isPacking,
  isBatchRendering,
} = storeToRefs(store);
const {
  togglePrimary,
  scanRoot,
  removeRoot,
  addFolder,
  scanAll,
  cancelAllScans,
  clearGeometryFilter,
  openNormalize,
  bulkPack,
  openBatchRender,
} = store;

// The store keeps volume in mm³ (the backend's unit); ml is what a resin
// printer's slicer reports, so the inputs convert at the edge.
const volumeMinMl = computed<number | null>({
  get: () => (volumeMinMm3.value === null ? null : volumeMinMm3.value / 1000),
  set: (v) => {
    volumeMinMm3.value = v === null ? null : v * 1000;
  },
});
const volumeMaxMl = computed<number | null>({
  get: () => (volumeMaxMm3.value === null ? null : volumeMaxMm3.value / 1000),
  set: (v) => {
    volumeMaxMm3.value = v === null ? null : v * 1000;
  },
});

const searchInput = ref<HTMLInputElement | null>(null);
// "/" focuses search from anywhere, like GitHub/Slack — except while the
// user is already typing somewhere, where it must stay a literal character
const onGlobalKeydown = (event: KeyboardEvent) => {
  if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey) {
    return;
  }
  const target = event.target as HTMLElement | null;
  const tag = target?.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
  event.preventDefault();
  searchInput.value?.focus();
};
onMounted(() => window.addEventListener("keydown", onGlobalKeydown));
onUnmounted(() => window.removeEventListener("keydown", onGlobalKeydown));
</script>
