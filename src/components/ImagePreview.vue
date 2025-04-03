<template>
  <h2 class="text-xl font-bold">Images</h2>

  <div class="border border-gray-500 rounded-box bg-base-100 p-2 w-full mb-2">
    <div class="flex items-center gap-2 mb-3">
      <button @click="selectImages" class="btn btn-primary flex-grow">
        <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 mr-2" viewBox="0 0 20 20" fill="currentColor">
          <path fill-rule="evenodd" d="M3 17a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1zM6.293 6.707a1 1 0 010-1.414l3-3a1 1 0 011.414 0l3 3a1 1 0 01-1.414 1.414L11 5.414V13a1 1 0 11-2 0V5.414L7.707 6.707a1 1 0 01-1.414 0z" clip-rule="evenodd" />
        </svg>
        Add Images
      </button>
      <span class="text-sm opacity-70" v-if="images.length">
        {{ images.length }} image{{ images.length !== 1 ? 's' : '' }} selected
      </span>
    </div>


  <!-- Main Preview Image -->
  <div v-if="images.length > 0" class="w-full aspect-square mb-4">
    <img :src="images[selectedImageIndex].getPreviewUrl()"
         alt="Primary preview"
         class="w-full h-full object-cover rounded-lg border border-base-300 cursor-pointer hover:border-2 hover:border-primary-500"
         @click="imageDetailViewOpen = true"
    />
  </div>

  <!-- Image Thumbnails -->
  <div v-if="images.length > 0" class="flex overflow-x-auto gap-4 pb-2 max-w-full flex-shrink-0 overflow-y-hidden">
    <div
        v-for="(img, index) in images"
        :key="index"
        class="relative flex-shrink-0 w-32"
    >
      <img
          :src="img.getPreviewUrl()"
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
        v-if="images.length > 0"
        :src="images[selectedImageIndex].getPreviewUrl()"
        alt="Full size preview"
        class="max-w-full max-h-[90vh] object-contain"
    />
  </ModalView>
</template>

<script setup lang="ts">
import ModalView from "./ModalView.vue";
import { ref, watch } from "vue";
import { useFileSelect, type SelectedFile } from "../composables/useFileSelect";

const imageDetailViewOpen = ref(false);

const props = defineProps<{
  modelValue?: SelectedFile[];
  accept?: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [files: SelectedFile[]];
}>();

const { selectFiles } = useFileSelect();
const images = ref<SelectedFile[]>([]);
const selectedImageIndex = ref(0);

watch(
  () => props.modelValue,
  (newFiles) => {
    if (newFiles && newFiles.length > 0) {
      images.value = newFiles;
    } else {
      images.value = [];
    }
    selectedImageIndex.value = 0;
  },
  { immediate: true },
);

const selectImages = async () => {
  try {
    const imageAccept = props.accept || "jpg,jpeg,png,gif,webp,svg,bmp";
    const files = await selectFiles({
      multiple: true,
      accept: imageAccept,
      title: "Select Images",
    });

    if (!files) return;

    // Check for duplicates by file path
    const existingPaths = new Set(images.value.map((file) => file.path));

    const newFiles = files.filter((file) => !existingPaths.has(file.path));

    if (newFiles.length > 0) {
      // Add new images to existing ones
      images.value = [...images.value, ...newFiles];
      emitUpdate();
    }
  } catch (error) {
    console.error("Error selecting images:", error);
  }
};

const makePrimary = (index: number) => {
  if (index > 0 && index < images.value.length) {
    // Move the selected image to the front (index 0)
    const primaryImage = images.value.splice(index, 1)[0];
    images.value.unshift(primaryImage);

    // Update selected index to track the same image
    selectedImageIndex.value = 0;
    emitUpdate();
  }
};

const removeImage = (index: number) => {
  if (index >= 0 && index < images.value.length) {
    images.value.splice(index, 1);

    // Adjust selected index if necessary
    if (index <= selectedImageIndex.value) {
      selectedImageIndex.value = Math.max(0, selectedImageIndex.value - 1);
    }

    // Handle empty state
    if (images.value.length === 0) {
      selectedImageIndex.value = 0;
    }

    emitUpdate();
  }
};

const emitUpdate = () => {
  emit("update:modelValue", images.value);
};
</script>
