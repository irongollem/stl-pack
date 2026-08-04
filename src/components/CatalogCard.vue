<script setup lang="ts">
import { convertFileSrc } from "@tauri-apps/api/core";
import { computed } from "vue";
import type { CatalogGroup } from "../bindings";
import { formatFileSize } from "../utils/format";

const props = defineProps<{
  group: CatalogGroup;
  selected?: boolean;
  checked?: boolean;
}>();

defineEmits<{
  select: [group: CatalogGroup];
  toggleCheck: [group: CatalogGroup];
}>();

const previewUrl = computed(() =>
  props.group.preview_path ? convertFileSrc(props.group.preview_path) : null,
);

const measuredSize = computed(() => {
  const { height_mm, volume_mm3 } = props.group;
  const parts: string[] = [];
  if (height_mm != null) parts.push(`${height_mm.toFixed(1)} mm`);
  if (volume_mm3 != null) parts.push(`${(volume_mm3 / 1000).toFixed(1)} ml`);
  return parts.join(" · ");
});
</script>

<template>
  <!-- div, not button: the card hosts a nested checkbox and interactive
       elements can't nest -->
  <div
    role="button"
    class="card bg-base-100 border border-base-content/15 text-left hover:border-primary transition-colors overflow-hidden cursor-pointer"
    :class="{ 'border-primary ring-1 ring-primary': selected }"
    @click="$emit('select', group)"
  >
    <figure class="aspect-square bg-black/40 relative">
      <img
        v-if="previewUrl"
        :src="previewUrl"
        :alt="group.group_name"
        loading="lazy"
        class="object-cover w-full h-full"
      />
      <div
        v-else
        class="w-full h-full flex items-center justify-center text-4xl opacity-30"
      >
        🗿
      </div>
      <!-- batch-selection for the move tool; quiet until checked or hovered -->
      <input
        type="checkbox"
        class="checkbox checkbox-sm absolute top-1.5 left-1.5 bg-base-100/70"
        :class="checked ? 'opacity-100' : 'opacity-40 hover:opacity-100'"
        :checked="checked"
        @click.stop
        @change="$emit('toggleCheck', group)"
      />
      <!-- compressed at rest: the whole group lives in pack archives -->
      <span
        v-if="group.packed"
        class="badge badge-xs font-mono bg-base-100/80 absolute top-1.5 right-1.5"
        title="Packed — compressed at rest"
      >
        📦
      </span>
      <!-- any effectively-flagged member marks the whole card -->
      <span
        v-if="group.nsfw"
        class="badge badge-xs badge-error badge-outline font-mono absolute top-1.5"
        :class="group.packed ? 'right-9' : 'right-1.5'"
        title="18+ — hidden from browsing when Show 18+ is off in Settings"
      >
        18+
      </span>
      <!-- support variants at a glance -->
      <div
        v-if="group.support_statuses.length"
        class="absolute bottom-1.5 right-1.5 flex gap-1"
      >
        <span
          v-for="status in group.support_statuses"
          :key="status"
          class="badge badge-xs font-mono bg-base-100/80"
        >
          {{ status }}
        </span>
      </div>
    </figure>
    <div class="card-body p-3 gap-1">
      <h3 class="font-semibold text-sm truncate" :title="group.group_name">
        {{ group.group_name }}
      </h3>
      <p v-if="group.designer" class="text-xs opacity-60 truncate">
        {{ group.designer }}
      </p>
      <p class="text-xs opacity-40">
        <template v-if="group.pose_count > 1"
          >{{ group.pose_count }} poses ·
        </template>
        {{ group.file_count }} file{{ group.file_count === 1 ? "" : "s" }} ·
        {{ formatFileSize(group.total_size_bytes) }}
      </p>
      <!-- Mined physical size — only known once the geometry scan has run;
           silently omitted rather than shown as 0, matching the drawer's
           un-mined marker. -->
      <p v-if="measuredSize" class="text-xs opacity-40 font-mono">
        {{ measuredSize }}
      </p>
    </div>
  </div>
</template>
