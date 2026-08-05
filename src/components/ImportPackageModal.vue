<template>
  <div
    v-if="inspection"
    class="fixed inset-0 bg-black/60 backdrop-blur-sm flex items-center justify-center z-50"
  >
    <div
      class="bg-base-100 border border-base-content/10 rounded-xl shadow-xl w-130 max-w-[92vw] max-h-[85vh] p-5 flex flex-col gap-4"
    >
      <div class="flex flex-col gap-1">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >{{
            inspection.is_update ? "RELEASE UPDATE" : "RELEASE IMPORT"
          }}</span
        >
        <span class="font-bold text-[15px]">{{ inspection.release_name }}</span>
        <span class="text-[12px] text-base-content/60">
          by {{ inspection.designer }} · {{ inspection.date
          }}<template v-if="inspection.version">
            · v{{ inspection.version }}</template
          >
        </span>
      </div>

      <p
        v-if="inspection.blocked"
        class="text-[12.5px] text-error leading-relaxed"
      >
        {{ inspection.blocked }}
      </p>

      <template v-else>
        <p class="text-[12.5px] text-base-content/70 leading-relaxed">
          {{ summaryLine }}
        </p>
        <p
          v-if="ownership.missingFiles > 0"
          class="text-[12.5px] text-warning/90 leading-relaxed"
        >
          You own {{ ownership.owned }} of {{ ownership.total }} files across
          this release · {{ formatFileSize(ownership.missingBytes) }} missing.
        </p>

        <div class="flex flex-col gap-1 overflow-y-auto -mx-1 px-1">
          <label
            v-for="component in inspection.components"
            :key="component.name"
            class="flex items-center gap-3 bg-base-200 border border-base-content/10 rounded-lg px-3 py-2"
            :class="
              selectable(component)
                ? 'cursor-pointer'
                : 'opacity-60 cursor-not-allowed'
            "
            :title="component.model_names.join(', ')"
          >
            <input
              type="checkbox"
              class="checkbox checkbox-sm"
              :disabled="!selectable(component) || importing"
              v-model="selected[component.name]"
            />
            <div class="flex-1 min-w-0">
              <div class="flex items-center gap-2">
                <span class="text-[13px] font-medium truncate">{{
                  component.name
                }}</span>
                <span
                  class="font-mono text-[9px] tracking-wider shrink-0"
                  :class="badge(component.state).tone"
                  >{{ badge(component.state).label }}</span
                >
              </div>
              <div class="text-[11px] text-base-content/50 truncate">
                {{ component.file_count }} file{{
                  component.file_count === 1 ? "" : "s"
                }}
                · {{ formatFileSize(component.size_bytes)
                }}<template v-if="component.detail">
                  — {{ component.detail }}</template
                >
              </div>
              <div
                v-if="component.files_owned < component.file_count"
                class="text-[11px] text-warning/70 truncate"
              >
                You own {{ component.files_owned }}/{{
                  component.file_count
                }}
                files · {{ formatFileSize(component.missing_bytes) }} missing
              </div>
            </div>
          </label>
        </div>
      </template>

      <div class="flex items-center justify-between gap-2">
        <span class="text-[11px] text-base-content/50">
          <template v-if="!inspection.blocked && selectedNames.length"
            >{{ selectedNames.length }} selected ·
            {{ formatFileSize(selectedSize) }}</template
          >
        </span>
        <div class="flex gap-2">
          <button
            type="button"
            class="btn btn-sm"
            :disabled="importing"
            @click="emit('cancel')"
          >
            {{ inspection.blocked ? "Close" : "Cancel" }}
          </button>
          <button
            v-if="!inspection.blocked && recompileEligible"
            type="button"
            class="btn btn-sm btn-secondary"
            :disabled="importing"
            @click="emit('recompile', null)"
          >
            <span
              v-if="importing"
              class="loading loading-spinner loading-xs"
            ></span>
            Import what you own
          </button>
          <button
            v-if="!inspection.blocked"
            type="button"
            class="btn btn-sm btn-primary"
            :disabled="importing || selectedNames.length === 0"
            @click="emit('confirm', selectedNames)"
          >
            <span
              v-if="importing"
              class="loading loading-spinner loading-xs"
            ></span>
            {{ inspection.is_update ? "Update" : "Import" }}
          </button>
        </div>
      </div>

      <p
        v-if="!inspection.blocked"
        class="text-[10px] text-base-content/35 leading-relaxed"
      >
        Every file is verified against the release's checksums before it lands
        in your library. Files you edited locally are kept aside as "(edited)",
        never overwritten.
      </p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, watch } from "vue";
import type {
  ComponentState,
  ComponentStatus,
  PackageInspection,
} from "../bindings";
import { formatFileSize } from "../utils/format";

const props = defineProps<{
  inspection: PackageInspection | null;
  importing: boolean;
}>();

const emit = defineEmits<{
  confirm: [components: string[]];
  recompile: [components: string[] | null];
  cancel: [];
}>();

// Pre-check what an import should touch: new and changed components. An
// unchanged one stays selectable (re-importing it repairs deleted files)
// but unchecked; packed/missing ones can't run at all.
const selected = reactive<Record<string, boolean>>({});
watch(
  () => props.inspection,
  (inspection) => {
    for (const key of Object.keys(selected)) delete selected[key];
    for (const component of inspection?.components ?? []) {
      selected[component.name] =
        component.state === "new" || component.state === "changed";
    }
  },
  { immediate: true },
);

const selectable = (component: ComponentStatus) =>
  component.state !== "packed" && component.state !== "missing_archive";

const selectedNames = computed(() =>
  (props.inspection?.components ?? [])
    .filter((c) => selectable(c) && selected[c.name])
    .map((c) => c.name),
);

const selectedSize = computed(() =>
  (props.inspection?.components ?? [])
    .filter((c) => selectable(c) && selected[c.name])
    .reduce((sum, c) => sum + c.size_bytes, 0),
);

const badge = (state: ComponentState) => {
  switch (state) {
    case "new":
      return { label: "NEW", tone: "text-success" };
    case "changed":
      return { label: "UPDATE", tone: "text-warning" };
    case "packed":
      return { label: "PACKED", tone: "text-warning" };
    case "missing_archive":
      return { label: "MISSING", tone: "text-error" };
    default:
      return { label: "UNCHANGED", tone: "text-base-content/40" };
  }
};

// Release-wide rollup of the per-component completeness diff — how much of
// the WHOLE release this library already owns by checksum, regardless of
// which components are selected or importable today.
const ownership = computed(() => {
  const components = props.inspection?.components ?? [];
  return {
    total: components.reduce((sum, c) => sum + c.file_count, 0),
    owned: components.reduce((sum, c) => sum + c.files_owned, 0),
    missingFiles: components.reduce((sum, c) => sum + c.missing.length, 0),
    missingBytes: components.reduce((sum, c) => sum + c.missing_bytes, 0),
  };
});

// Worth offering "Import what you own" when at least one non-packed
// component is short a file — packed components can't be helped by a
// donor (they're already complete, just compressed) so recompiling one
// would only ever refuse it.
const recompileEligible = computed(() =>
  (props.inspection?.components ?? []).some(
    (c) => c.state !== "packed" && c.files_owned < c.file_count,
  ),
);

const summaryLine = computed(() => {
  const components = props.inspection?.components ?? [];
  if (!props.inspection?.is_update) {
    return `${components.length} component${components.length === 1 ? "" : "s"} ready to import.`;
  }
  const changed = components.filter(
    (c) => c.state === "changed" || c.state === "new",
  ).length;
  if (changed === 0) {
    return "Your library already matches this release — nothing needs updating.";
  }
  return `${changed} of ${components.length} component${components.length === 1 ? "" : "s"} changed since your last import — only what you select is rewritten.`;
});
</script>
