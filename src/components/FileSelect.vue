<script setup lang="ts">
import { computed } from "vue";
import { ref } from "vue";
import { useFileSelect, type SelectedFile } from "../composables/useFileSelect";

const props = defineProps<{
  id: string;
  label: string;
  modelValue?: string | SelectedFile[] | null;
  tooltip?: string;
  dirMode?: boolean;
  multiple?: boolean;
  accept?: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string | SelectedFile[]];
}>();

const { selectFiles, selectDirectory, formatFileSize } = useFileSelect();
const selectedFiles = ref<SelectedFile[]>([]);

if (!props.dirMode && Array.isArray(props.modelValue)) {
  selectedFiles.value = props.modelValue;
}

const fileCountText = computed(() => {
  if (props.dirMode || !selectedFiles.value.length) return "";
  return `${selectedFiles.value.length} file${selectedFiles.value.length > 1 ? "s" : ""} selected`;
});

const selectPath = async () => {
  try {
    if (props.dirMode) {
      const selected = await selectDirectory({
        title: `Select ${props.label}`,
      });

      if (selected) {
        emit("update:modelValue", selected);
      }
    } else {
      const files = await selectFiles({
        multiple: props.multiple,
        accept: props.accept,
        title: `Select ${props.label}`,
      });

      if (files) {
        selectedFiles.value = files as SelectedFile[];
        emit("update:modelValue", selectedFiles.value);
      }
    }
  } catch (error) {
    console.error("Selection failed:", error);
  }
};

const removeFile = (file: SelectedFile) => {
  selectedFiles.value = selectedFiles.value.filter((f) => f.path !== file.path);
  emit("update:modelValue", selectedFiles.value);
};

const clearFiles = () => {
  selectedFiles.value = [];
  emit("update:modelValue", selectedFiles.value);
};
</script>

<template>
<div class="form-control mb-2">
  <label class="floating-label" :for="id">
    <span class="label">{{  label  }}</span>
  </label>

    <div v-if="dirMode" class="flex">
      <input
        type="text"
        readonly
        :value="modelValue as string"
        class="input flex-1"
        placeholder="Select target directory..."
      />
      <div class="tooltip" :data-tip="tooltip">
        <button
          type="button"
          @click="selectPath()"
          class="btn"
        > Browse </button>
      </div>
    </div>

    <div v-else class="border border-gray-500 rounded-box bg-base-100 p-2 w-full">
      <div class="flex items-center gap-2 mb-3">
        <button
          type="button"
          class="btn btn-primary flex-grow"
          @click="selectPath"
        >
          <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 mr-2" viewBox="0 0 20 20" fill="currentColor">
            <path fill-rule="evenodd" d="M3 17a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1zM6.293 6.707a1 1 0 010-1.414l3-3a1 1 0 011.414 0l3 3a1 1 0 01-1.414 1.414L11 5.414V13a1 1 0 11-2 0V5.414L7.707 6.707a1 1 0 01-1.414 0z" clip-rule="evenodd" />
          </svg>
          Select Files
        </button>
        <span class="text-sm opacity-70" v-if="selectedFiles.length">
          {{ fileCountText }}
        </span>
      </div>

      <div class="overflow-x-auto" v-if="selectedFiles.length">
        <table class="table table-xs w-full">
          <thead>
            <tr>
              <th>File Name</th>
              <th>Size</th>
              <th>Type</th>
              <th>Actions</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="file in selectedFiles" :key="file.path">
              <td class="max-w-[200px] truncate" :title="file.name">{{ file.name }}</td>
              <td>{{ formatFileSize(file.info.size) }}</td>
              <td>{{ file.type }}</td>
              <td>
                <button class="btn btn-error btn-xs" @click="removeFile(file)">
                  Remove
                </button>
              </td>
            </tr>
          </tbody>
        </table>

        <div class="mt-2 flex justify-end">
          <button class="btn btn-sm btn-outline" @click="clearFiles">
            Clear All
          </button>
        </div>
      </div>

      <div v-else class="text-center p-4 text-base-content/50">
        No files selected
      </div>
  </div>
</div>
</template>
