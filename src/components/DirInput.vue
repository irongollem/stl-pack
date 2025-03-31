<script setup lang="ts">
import { open } from "@tauri-apps/plugin-dialog";

const props = defineProps<{
  id: string;
  label: string;
  modelValue?: string | null;
  tooltip?: string;
}>();

const emit = defineEmits<{
  (e: "update:modelValue", value: string): void;
}>();

const selectDirectory = async () => {
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      title: `Select ${props.label}`,
    });

    if (selected) {
      emit("update:modelValue", selected as string);
    }
  } catch (error) {
    console.error("Directory selection failed:", error);
  }
};
</script>

<template>
  <label class="floating-label mb-2" :for="id">
    <span class="label">{{  label  }}</span>
    <div class="flex">
      <input type="text" readonly
             :value="modelValue"
             class="input flex-1"
             placeholder="Select target directory..." />
      <div class="tooltip" :data-tip="tooltip">
        <button type="button"
                @click="selectDirectory()"
                class="btn">
          Browse
        </button>
      </div>
    </div>
  </label>
</template>

<style scoped>

</style>