<template>
  <!-- Footer: stats + duplicates -->
  <div
    class="flex flex-wrap items-center gap-4 font-mono text-[10.5px] text-base-content/40 border-t border-base-content/10 pt-2"
  >
    <template v-if="stats">
      <span
        @click="toggleDups"
        :class="reclaimableGroups.length ? 'text-primary cursor-pointer' : ''"
      >
        <template v-if="reclaimableGroups.length"
          >{{ reclaimableGroups.length }} duplicate groups ·
          {{ formatFileSize(wastedBytes) }} reclaimable</template
        >
        <template v-else-if="dupGroups.length"
          >{{ dupGroups.length }} groups shared · stored once</template
        >
        <template v-else
          >{{ stats.total_models }} models · {{ stats.total_files }} files ·
          {{ formatFileSize(stats.total_size_bytes) }}</template
        >
      </span>
      <span
        v-if="stats.packed_models"
        title="Models compressed at rest: what their files would occupy loose vs what the archives take"
      >
        📦 {{ stats.packed_models }} packed ·
        {{
          formatFileSize(
            (stats.packed_logical_bytes ?? 0) -
              (stats.packed_archive_bytes ?? 0),
          )
        }}
        saved
      </span>
    </template>
    <span class="flex-1"></span>
    <span v-if="lastScanLabel">scanned {{ lastScanLabel }}</span>
    <button
      v-if="!isFindingDuplicates"
      type="button"
      class="border border-base-content/15 rounded-full px-2.5 py-0.5 text-base-content/60 cursor-pointer disabled:opacity-40"
      :disabled="!stats?.total_files || isMiningGeometry"
      @click="startDuplicateScan"
    >
      rescan duplicates
    </button>
    <span v-else class="flex items-center gap-2">
      <span class="loading loading-spinner loading-xs"></span>
      hashing {{ dupProgress?.processed ?? 0 }}/{{ dupProgress?.total ?? "?" }}
      <button type="button" class="link" @click="cancelDuplicateScan">
        cancel
      </button>
    </span>
    <button
      v-if="!isMiningGeometry"
      type="button"
      class="border border-base-content/15 rounded-full px-2.5 py-0.5 text-base-content/60 cursor-pointer disabled:opacity-40"
      :disabled="!stats?.total_files || isFindingDuplicates"
      @click="startGeometryScan"
    >
      mine geometry
    </button>
    <span v-else class="flex items-center gap-2">
      <span class="loading loading-spinner loading-xs"></span>
      mining {{ geoProgress?.processed ?? 0 }}/{{ geoProgress?.total ?? "?" }}
      <button type="button" class="link" @click="cancelGeometryScan">
        cancel
      </button>
    </span>
  </div>
</template>

<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCatalogStore } from "../../stores/catalogStore";
import { formatFileSize } from "../../utils/format";

const store = useCatalogStore();
const {
  stats,
  reclaimableGroups,
  wastedBytes,
  dupGroups,
  lastScanLabel,
  isFindingDuplicates,
  dupProgress,
  isMiningGeometry,
  geoProgress,
} = storeToRefs(store);
const {
  toggleDups,
  startDuplicateScan,
  cancelDuplicateScan,
  startGeometryScan,
  cancelGeometryScan,
} = store;
</script>
