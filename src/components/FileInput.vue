<template>
<label class="floating-label mb-2" :for="id">
  <span class="label"> {{ label }}</span>
  <input
      :id="id"
      class="file-input w-full"
      type="file"
      :accept="accept"
      :multiple="multiple"
      @change="handleFileChange" />
</label>
</template>

<script setup lang="ts">
import { ref, onBeforeUnmount } from "vue";
import { FileMeta } from "../types.ts";

const props = defineProps<{
  id: string;
  label?: string;
  modelValue?: Array<FileMeta>;
  accept?: string;
  multiple?: boolean;
}>();

const emit = defineEmits<{
  (
    e: "update:modelValue",
    value: Array<FileMeta>,
  ): void;
}>();

const objectUrls = ref<string[]>([]);

const handleFileChange = (e: Event) => {
  const files = (e.target as HTMLInputElement).files;
  if (!files)return;

  const validFiles = Array.from(files).filter((file) => validateFileType(file, props.accept || ""));

  if (validFiles.length < files.length) {
    console.warn("Some files are not valid");
  }

  const newFiles = validFiles.map((file) => {
    const objectUrl = URL.createObjectURL(file);
    objectUrls.value.push(objectUrl);
    return { name: file.name, path: objectUrl, file };
  });
  emit("update:modelValue", [...(props.modelValue || []), ...newFiles]);
};

const validateFileType = (file: File, acceptTypes: string): boolean => {
  if (!acceptTypes) return true;

  const acceptedTypes = acceptTypes.split(",").map(type => type.trim().toLowerCase());
  const fileType = file.type.toLowerCase();
  const fileExt = `.${file.name.split(".").pop().toLowerCase() || ''}`;

  return acceptedTypes.some((type) => {
    if (type.startsWith(".")) {
      return fileExt === type;
    } else if (type.includes("/*")) {
      const [category] = type.split('/');
      return fileType.startsWith(`${category}/`);
    }
    return fileType === type;
  });
}

onBeforeUnmount(() => {
  objectUrls.value.forEach((url) => URL.revokeObjectURL(url));
});
</script>


<style scoped>

</style>