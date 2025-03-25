<script setup lang="ts">
import MonthYearInput from "../components/MonthYearInput.vue";
import TextArea from "../components/TextArea.vue";
import TextInput from "../components/TextInput.vue";
import TagInput from "../components/TagInput.vue";
import {onMounted, Ref, ref} from "vue";
import { FileContext, fileLogos, StlModel } from "../types.ts";
import { invoke } from "@tauri-apps/api/core";
import FileInput from "../components/FileInput.vue";
import ModalView from "../components/ModalView.vue";
import {listen} from "@tauri-apps/api/event";

const model: Ref<StlModel> = ref({
  modelName: "",
  releaseDate: "",
  designer: "",
  release: "",
  description: "",
  tags: [],
  images: [],
  modelFiles: [],
});
const images = ref<FileContext[]>([]);
const modelFiles = ref<FileContext[]>([]);

const imagePreviews = ref<string[]>([]);
const selectedImageIndex = ref(0);
const scratchDir = ref<string | null>(null); // TODO: Implement scratchDir option for user
const storageProgress = ref(0);
const totalFiles = ref(0);
const processedFiles = ref(0);

const storeFiles = async (
  images: FileContext[],
  files: FileContext[],
  modelName: string,
) => {
  if (!model.value.modelName) {
    throw new Error("Model name is required for file upload");
  }
  totalFiles.value = images.length + files.length;
  processedFiles.value = 0;
  storageProgress.value = 0;

  const processedImages = [];
  for (const [imageIndex, image] of images.entries()) {
    if (image.file) {
      const fileData = new Uint8Array(await image.file.arrayBuffer());
      const filePath = await invoke<string>("store_image", {
        imageData: Array.from(fileData),
        imageName: image.name,
        modelName,
        imageIndex,
        scratchDir,
      });

      processedImages.push(filePath);
      processedFiles.value++;
      storageProgress.value = processedFiles.value / totalFiles.value;
    }
  }

  const processedModelFiles = [];
  for (const modelFile of files) {
    if (modelFile.file) {
      const fileData = new Uint8Array(await modelFile.file.arrayBuffer());
      const filePath = await invoke("store_model_file", {
        fileData: Array.from(fileData),
        fileName: modelFile.name,
        modelName,
        scratchDir,
      });
      processedModelFiles.push(filePath);
      processedFiles.value++;
      storageProgress.value = processedFiles.value / totalFiles.value;
    }
  }

  return { processedImages, processedModelFiles };
};

const isStoring = ref(false);
const saveModelData = async () => {
  try {
    isStoring.value = true;
    const storedFiles = await storeFiles(
      images.value,
      modelFiles.value,
      model.value.modelName,
    );

    const modelData = {
      ...model.value,
      pictures: storedFiles.processedImages,
      modelFiles: storedFiles.processedModelFiles,
    };

    await invoke("save_model", { model: modelData });
    alert("Model saved successfully!"); // FIXME: Show a toast message instead
    clearModel();
  } catch (error) {
    alert("Failed to save model"); // FIXME: Show a toast message instead
  } finally {
    isStoring.value = false;
  }
};

const compressionProgress = ref(0);
const compressedFiles = ref(0);
const totalCompressedFiles = ref(0);
const isCompressing = ref(false);
interface CompressionProgressPayload {
  percent: number;
  processed: number;
  total: number;
}

onMounted(async () => {
  await listen<CompressionProgressPayload>("compression_progress", (event) => {
    const data = event.payload;
    compressionProgress.value = data.percent;
    compressedFiles.value = data.processed;
    totalCompressedFiles.value = data.total;
    isCompressing.value = data.processed < data.total;
  });
});

const clearModel = () => {
  model.value = {
    modelName: "",
    releaseDate: "",
    designer: "",
    release: "",
    description: "",
    tags: [],
    images: [],
    modelFiles: [],
  };
  imagePreviews.value = [];
  selectedImageIndex.value = 0;
};

const imageDetailViewOpen = ref(false);

const updateImagePreviews = (files: FileContext[]) => {
  imagePreviews.value = files.map((file) => file.path);
};

const removeImage = (index: number) => {
  // Remove from model.pictures
  model.value.images.splice(index, 1);
  model.value.images = [...model.value.images];

  // Remove from imagePreviews
  imagePreviews.value.splice(index, 1);

  // Adjust selected index if necessary
  if (index <= selectedImageIndex.value) {
    selectedImageIndex.value = Math.max(0, selectedImageIndex.value - 1);
  }

  // Handle empty state
  if (imagePreviews.value.length === 0) {
    selectedImageIndex.value = 0;
  }
};

const getLogo = (fileName: string) => {
  const ext = fileName.split(".").pop();
  if (ext === "stl") {
    return fileLogos.stl;
  } else if (ext === "chitu" || ext === "chitubox") {
    return fileLogos.chitubox;
  } else if (ext === "lyt" || ext === "lys" || ext === "lychee") {
    return fileLogos.lychee;
  } else {
    return "tauri.svg";
  }
};
</script>

<template>
  <main class="bg-gray-800 h-screen p-16 text-white">
    <header class="mb-6">
      <h1 class="text-2xl font-bold">Add STL Model</h1>
    </header>

    <div class="flex flex-col lg:flex-row gap-8">
      <section class="lg:w-1/2">
        <h2 class="text-xl font-bold">Metadata</h2>
        <form @submit.prevent="saveModelData">
          <TextInput
              id="model-name"
              v-model="model.modelName"
              label="Model Name"
              placeholder="Enter model name..."
          />

          <TextArea id="description" placeholder="Enter the description..." label="Description" v-model="model.description" />

          <TextInput id="designer" label="Designer" placeholder="Name of the designer..." v-model="model.designer" />

          <TextInput id="release" label="Release" placeholder="Name of the release..." v-model="model.release" />

          <MonthYearInput id="releaseDate" label="Release date" v-model="model.releaseDate" />

          <TagInput id="tags" v-model="model.tags" label="Tags" placeholder="Write tags here..." />

          <FileInput
              id="pictures"
              label="Pictures"
              multiple
              accept=".jpg,.jpeg,.png,.gif,.webp,image/*"
              v-model="images"
              :enabled="model.modelName.length > 0"
              @update:modelValue="updateImagePreviews"
          />

          <FileInput
              id="model-files"
              label="Model Files"
              multiple
              accept=".stl,.obj,.chitubox,.lys,.3mf,.blend,.gcode,.png"
              v-model="modelFiles"
              :enabled="model.modelName.length > 0"
          />

          <ul v-if="model.modelFiles.length > 0" class="list">
            <li v-for="modelFile in modelFiles" :key="modelFile.name" class="list-row">
              <div>
                <img class="size-8 rounded-box" :src="getLogo(modelFile.name)" alt="File icon" />
              </div>
              <div>
              {{modelFile.name}}
              </div>
              <div>
                <button class="btn btn-xs btn-error" @click="model.modelFiles.splice(modelFiles.indexOf(modelFile), 1)">Remove</button>
              </div>
            </li>
          </ul>

          <div class="flex justify-between w-full mb-4">
            <button class="btn" type="submit" :disabled="isStoring">
              <template v-if="isStoring">
                <span class="loading loading-spinner"></span>
                <span>Storing...</span>
              </template>
              <span v-else>Save Model</span>
            </button>
            <button class="btn btn-error" @click="clearModel">Clear Model</button>
          </div>
          <div v-if="isStoring" class="w-full">
            <h3>Moving files to temporary directory</h3>
            <progress class="progress w-full" :value="storageProgress" max="100" />
            <p class="text-sm text-center">Processing files: {{processedFiles}}/{{totalFiles}}</p>
          </div>
          <div v-if="isCompressing" class="w-full">
            <h3>Compressing files</h3>
            <progress class="progress w-full" :value="compressionProgress" max="100" />
            <p class="text-sm text-center">Compressing files: {{compressedFiles}}/{{totalCompressedFiles}}</p>
          </div>
        </form>
      </section>

      <aside v-if="imagePreviews.length" class="lg:w-1/2">
        <h2 class="text-xl font-bold">Image Previews</h2>
        <div class="w-full aspect-square mb-4">
          <img :src="imagePreviews[selectedImageIndex]"
               alt="Primary preview"
               class="w-full h-full object-cover rounded-lg border border-base-300 cursor-pointer hover:border-2 hover:border-primary-500"
               @click="imageDetailViewOpen = true"
          />
        </div>
        <div class="flex overflow-x-auto gap-4 pb-2 max-w-full flex-shrink-0 overflow-y-hidden">
          <div
              v-for="(img, index) in imagePreviews"
              :key="index"
              class="relative flex-shrink-0 w-32"
          >
            <img
                :src="img"
                :alt="`Image ${index + 1}`"
                class="w-32 h-32 object-cover rounded cursor-pointer"
                :class="{ 'border-2 border-primary-500': index === selectedImageIndex }"
                @click="selectedImageIndex = index"
            />
            <button
                @click.stop="removeImage(index)"
                class="absolute top-1 right-1 bg-red-500 text-white rounded-full w-6 h-6 flex items-center justify-center hover:bg-red-700 text-sm font-bold"
            >
              ×
            </button>
          </div>
        </div>
      </aside>
    </div>


  </main>
  <ModalView :isOpen="imageDetailViewOpen" @close="imageDetailViewOpen = false">
    <img
        :src="imagePreviews[selectedImageIndex]"
        alt="Full size preview"
        class="max-w-full max-h-[90vh] object-contain"
    />
  </ModalView>
</template>
