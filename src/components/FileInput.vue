<template>
  <div class="form-control w-full mb-2">
    <label class="floating-label" :for="id">
      <span class="label">{{ label }}</span>
    </label>

    <div class="border border-gray-500 rounded-box bg-base-100 p-2 w-full">
      <div class="flex items-center gap-2 mb-3">
        <label class="btn btn-primary flex-grow">
          <input
              :id="id"
              class="hidden"
              type="file"
              :accept="accept"
              :multiple="multiple"
              @change="handleFileChange"
              :disabled="disabled"
          />
          <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 mr-2" viewBox="0 0 20 20" fill="currentColor">
            <path fill-rule="evenodd" d="M3 17a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1zM6.293 6.707a1 1 0 010-1.414l3-3a1 1 0 011.414 0l3 3a1 1 0 01-1.414 1.414L11 5.414V13a1 1 0 11-2 0V5.414L7.707 6.707a1 1 0 01-1.414 0z" clip-rule="evenodd" />
          </svg>
          Select Files
        </label>
        <span class="text-sm opacity-70" v-if="filesStore.size">
          {{ filesStore.size }} file{{ filesStore.size !== 1 ? 's' : '' }} selected
        </span>
      </div>

      <div class="overflow-x-auto" v-if="filesStore.size">
        <table class="table table-xs w-full">
          <thead>
          <tr>
            <th @click="changeSorting('name')" class="cursor-pointer hover:bg-base-200">
              File Name
              <span v-if="sortColumn === 'name'">{{ sortDirection === 'asc' ? '↑' : '↓' }}</span>
            </th>
            <th @click="changeSorting('size')" class="cursor-pointer hover:bg-base-200">
              File Size
              <span v-if="sortColumn === 'size'">{{ sortDirection === 'asc' ? '↑' : '↓' }}</span>
            </th>
            <th @click="changeSorting('type')" class="cursor-pointer hover:bg-base-200">
              File Type
              <span v-if="sortColumn === 'type'">{{ sortDirection === 'asc' ? '↑' : '↓' }}</span>
            </th>
            <th>Actions</th>
          </tr>
          </thead>
          <tbody>
          <tr v-for="file in sortedFiles" :key="file.name + file.size">
            <td class="max-w-[200px] truncate" :title="file.name">{{ file.name }}</td>
            <td>{{ formatFileSize(file.size) }}</td>
            <td>{{ getFileType(file) }}</td>
            <td>
              <button class="btn btn-error btn-xs" @click="removeFile(file)">
                Remove
              </button>
            </td>
          </tr>
          </tbody>
        </table>
      </div>

      <div v-else class="text-center p-4 text-base-content/50">
        No files selected
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onBeforeUnmount, computed } from "vue";

const props = defineProps<{
  id: string;
  label?: string;
  accept?: string;
  multiple?: boolean;
  disabled?: boolean;
  modelValue?: File[];
}>();

const filesStore = ref<Set<File>>(new Set<File>());
const sortColumn = ref<"name" | "size" | "type">("type");
const sortDirection = ref<"asc" | "desc">("asc");

const emit = defineEmits<{
  (e: "update:modelValue", value: File[]): void;
}>();

// Initialize files from modelValue if provided
if (props.modelValue) {
  props.modelValue.forEach((file) => filesStore.value.add(file));
}

const objectUrls = ref<string[]>([]);

const sortedFiles = computed(() => {
  const files = Array.from(filesStore.value);
  return files.sort((a, b) => {
    let compareResult = 0;

    if (sortColumn.value === "name") {
      compareResult = a.name.localeCompare(b.name);
    } else if (sortColumn.value === "size") {
      compareResult = a.size - b.size;
    } else if (sortColumn.value === "type") {
      const typeA = getFileType(a);
      const typeB = getFileType(b);
      compareResult = typeA.localeCompare(typeB);
    }

    return sortDirection.value === "asc" ? compareResult : -compareResult;
  });
});

const getFileType = (file: File): string => {
  if (file.type) return file.type;
  const ext = file.name.split(".").pop()?.toLowerCase() || "";
  return ext ? `.${ext}` : "Unknown";
};

const formatFileSize = (size: number): string => {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
};

const changeSorting = (column: "name" | "size" | "type") => {
  if (sortColumn.value === column) {
    sortDirection.value = sortDirection.value === "asc" ? "desc" : "asc";
  } else {
    sortColumn.value = column;
    sortDirection.value = "asc";
  }
};

const handleFileChange = (e: Event) => {
  const files = (e.target as HTMLInputElement).files;
  if (!files) return;

  const validFiles = Array.from(files).filter((file) =>
    validateFileType(file, props.accept || ""),
  );

  if (validFiles.length < files.length) {
    console.warn(
      "Some files are not valid, these were removed from the selection.",
    );
  }

  validFiles.forEach((file) => filesStore.value.add(file));

  // Reset the input value to allow selecting the same file again
  (e.target as HTMLInputElement).value = "";

  emitUpdate();
};

const removeFile = (file: File) => {
  filesStore.value.delete(file);
  emitUpdate();
};

const emitUpdate = () => {
  emit("update:modelValue", Array.from(filesStore.value));
};

const validateFileType = (file: File, acceptTypes: string): boolean => {
  if (!acceptTypes) return true;

  const acceptedTypes = acceptTypes
    .split(",")
    .map((type) => type.trim().toLowerCase());
  const fileType = file.type.toLowerCase();
  const fileExt = `.${file.name.split(".").pop()?.toLowerCase() || ""}`;

  return acceptedTypes.some((type) => {
    if (type.startsWith(".")) {
      return fileExt === type;
    } else if (type.includes("/*")) {
      const [category] = type.split("/");
      return fileType.startsWith(`${category}/`);
    }
    return fileType === type;
  });
};

onBeforeUnmount(() => {
  objectUrls.value.forEach((url) => URL.revokeObjectURL(url));
});
</script>