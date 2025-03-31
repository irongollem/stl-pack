<template>
  <h2 class="text-xl font-bold">Images</h2>

  <div class="border border-gray-500 rounded-box bg-base-100 p-2 w-full mb-2">
    <div class="flex items-center gap-2 mb-3">
      <label class="btn btn-primary flex-grow">
        <input
            class="hidden"
            type="file"
            :accept="accept || 'image/*'"
            multiple
            @change="handleFileChange"
        />
        <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 mr-2" viewBox="0 0 20 20" fill="currentColor">
          <path fill-rule="evenodd" d="M3 17a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1zM6.293 6.707a1 1 0 010-1.414l3-3a1 1 0 011.414 0l3 3a1 1 0 01-1.414 1.414L11 5.414V13a1 1 0 11-2 0V5.414L7.707 6.707a1 1 0 01-1.414 0z" clip-rule="evenodd" />
        </svg>
        Add Images
      </label>
      <span class="text-sm opacity-70" v-if="imageData.length">
        {{ imageData.length }} image{{ imageData.length !== 1 ? 's' : '' }} selected
      </span>
    </div>


  <!-- Main Preview Image -->
  <div v-if="imageData.length > 0" class="w-full aspect-square mb-4">
    <img :src="imageData[selectedImageIndex].objUrl"
         alt="Primary preview"
         class="w-full h-full object-cover rounded-lg border border-base-300 cursor-pointer hover:border-2 hover:border-primary-500"
         @click="imageDetailViewOpen = true"
    />
  </div>

  <!-- Image Thumbnails -->
  <div v-if="imageData.length > 0" class="flex overflow-x-auto gap-4 pb-2 max-w-full flex-shrink-0 overflow-y-hidden">
    <div
        v-for="(img, index) in imageData"
        :key="index"
        class="relative flex-shrink-0 w-32"
    >
      <img
          :src="img.objUrl"
          :alt="`Image ${index + 1}`"
          class="w-32 h-32 object-cover rounded cursor-pointer"
          :class="{
            'border-2 border-primary-500': index === selectedImageIndex,
            'border-2 border-success': index === 0
          }"
          @click="selectedImageIndex = index"
      />
      <div class="absolute top-1 right-1 flex gap-1">
        <button v-if="index !== 0"
                @click.stop="makePrimary(index)"
                title="Make primary image"
                class="bg-success text-white rounded-full w-6 h-6 flex items-center justify-center hover:bg-success-dark text-sm font-bold"
        >
          ★
        </button>
        <button
            @click.stop="removeImage(index)"
            title="Remove image"
            class="bg-red-500 text-white rounded-full w-6 h-6 flex items-center justify-center hover:bg-red-700 text-sm font-bold"
        >
          ×
        </button>
      </div>
    </div>
  </div>

  <div v-else class="text-center p-4 text-base-content/50">
    No images selected
  </div>
  </div>

  <ModalView :isOpen="imageDetailViewOpen" @close="imageDetailViewOpen = false">
    <img
        v-if="imageData.length > 0"
        :src="imageData[selectedImageIndex].objUrl"
        alt="Full size preview"
        class="max-w-full max-h-[90vh] object-contain"
    />
  </ModalView>
</template>

<script setup lang="ts">
import ModalView from "./ModalView.vue";
import { ref, watch } from "vue";

interface ImageItem {
  file: File;
  objUrl: string;
}

const imageDetailViewOpen = ref(false);

const props = defineProps<{
  modelValue?: File[];
  accept?: string;
}>();

const emit = defineEmits<{
  (e: "update:modelValue", files: File[]): void;
}>();

const imageData = ref<ImageItem[]>([]);
const selectedImageIndex = ref(0);

// Initialize from modelValue if provided
watch(() => props.modelValue, (newFiles) => {
  // Clean up old object URLs
  imageData.value.forEach(img => {
    if (img.objUrl?.startsWith("blob:")) {
      URL.revokeObjectURL(img.objUrl);
    }
  });

  // Create new image data from files
  if (newFiles && newFiles.length > 0) {
    imageData.value = newFiles.map(file => ({
      file,
      objUrl: URL.createObjectURL(file)
    }));
    selectedImageIndex.value = 0;
  } else {
    imageData.value = [];
    selectedImageIndex.value = 0;
  }
}, { immediate: true });

const handleFileChange = (e: Event) => {
  const files = (e.target as HTMLInputElement).files;
  if (!files) return;

  const validFiles = Array.from(files).filter(file =>
      validateFileType(file, props.accept || "image/*")
  );

  if (validFiles.length < files.length) {
    console.warn("Some files are not valid images and were removed");
  }

  // Check for duplicates by file name and add only new files
  const existingFileNames = new Set(imageData.value.map(item => item.file.name));

  const newFiles = validFiles.filter(file => !existingFileNames.has(file.name))
      .map(file => ({
        file,
        objUrl: URL.createObjectURL(file)
      }));

  if (newFiles.length > 0) {
    imageData.value = [...imageData.value, ...newFiles];
    // Reset the input to allow selecting the same files again
    (e.target as HTMLInputElement).value = '';
    emitUpdate();
  }
};

const makePrimary = (index: number) => {
  if (index > 0 && index < imageData.value.length) {
    // Move the selected image to the front (index 0)
    const primaryImage = imageData.value.splice(index, 1)[0];
    imageData.value.unshift(primaryImage);

    // Update selected index to track the same image
    selectedImageIndex.value = 0;
    emitUpdate();
  }
};

const removeImage = (index: number) => {
  if (index >= 0 && index < imageData.value.length) {
    const img = imageData.value[index];
    if (img.objUrl?.startsWith("blob:")) {
      URL.revokeObjectURL(img.objUrl);
    }

    imageData.value.splice(index, 1);

    // Adjust selected index if necessary
    if (index <= selectedImageIndex.value) {
      selectedImageIndex.value = Math.max(0, selectedImageIndex.value - 1);
    }

    // Handle empty state
    if (imageData.value.length === 0) {
      selectedImageIndex.value = 0;
    }

    emitUpdate();
  }
};

const emitUpdate = () => {
  // Extract just the File objects for the v-model
  const files = imageData.value.map(item => item.file);
  emit("update:modelValue", files);
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
</script>