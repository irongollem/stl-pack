<template>
  <div class="flex flex-wrap gap-1.5 items-center">
    <span class="font-mono text-[11px] text-base-content/40 shrink-0">
      {{ total.toLocaleString() }} result{{ total === 1 ? "" : "s" }}
    </span>
    <!-- Honesty note: a height/volume filter can't place un-mined models,
         so it hides them rather than guessing — this says how many, so
         "fewer results" doesn't get mistaken for "nothing that tall". -->
    <span
      v-if="hasGeometryFilter && notMinedCount > 0"
      class="font-mono text-[11px] text-warning/80 shrink-0"
      title="These models have no mined geometry yet, so the size filter can't place them. Run a geometry scan to include them."
    >
      · not mined yet: {{ notMinedCount.toLocaleString() }}
    </span>
    <span
      v-if="visibleTags.length"
      class="w-px h-3.5 bg-base-content/15 shrink-0"
    ></span>
    <button
      v-for="tag in visibleTags"
      :key="tag.tag"
      type="button"
      class="font-mono text-[11px] rounded-full px-2.5 py-1 border cursor-pointer"
      :class="
        selectedTags.includes(tag.tag)
          ? 'bg-primary text-primary-content border-primary'
          : 'text-base-content/60 border-base-content/15'
      "
      @click="toggleTag(tag.tag)"
    >
      {{ tag.tag }} {{ tag.count }}
    </button>
  </div>
</template>

<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCatalogStore } from "../../stores/catalogStore";

const store = useCatalogStore();
const { total, visibleTags, selectedTags, hasGeometryFilter, notMinedCount } =
  storeToRefs(store);
const { toggleTag } = store;
</script>
