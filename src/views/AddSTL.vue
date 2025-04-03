<template>
  <form @submit.prevent="saveModelData" @keydown.enter.prevent">
  <View>
    <template #left>
      <h1 class="text-xl font-bold">Model info</h1>
        <TextInput
            id="model-name"
            v-model="model.model_name"
            label="Model Name"
            placeholder="Enter model name..."
        />

        <TextArea id="description" placeholder="Enter the description (Optional)..." label="Description" v-model="model.description" />

      <TextInput id="group" label="Group" placeholder="Enter group name (Optional)..." v-model="model.group" />

        <TagInput id="tags" v-model="model.tags" label="Tags" placeholder="Write tags here..." />

        <FileInput
            id="model-files"
            label="Model Files"
            multiple
            accept=".stl,.obj,.chitubox,.lys,.3mf,.blend,.gcode,.png"
            v-model="modelFiles"
            :enabled="model.model_name.length > 0"
        />

        <ul v-if="model.model_files.length > 0" class="list">
          <li v-for="modelFile in modelFiles" :key="modelFile.name" class="list-row">
            <div>
              <img class="size-8 rounded-box" :src="getLogo(modelFile.name)" alt="File icon" />
            </div>
            <div>
              {{modelFile.name}}
            </div>
            <div>
              <button class="btn btn-xs btn-error" @click="model.model_files.splice(modelFiles.indexOf(modelFile), 1)">Remove</button>
            </div>
          </li>
        </ul>

        <div class="flex justify-between w-full mb-4">
          <button class="btn btn-primary" type="submit" :disabled="!formComplete || isStoring">
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
    </template>

    <template  #right>
      <ImagePreview v-model="images" />
    </template>
  </View>
  </form>
</template>

<script setup lang="ts">
import TextArea from "../components/TextArea.vue";
import TextInput from "../components/TextInput.vue";
import TagInput from "../components/TagInput.vue";
import { computed, onMounted, Ref, ref } from "vue";
import { fileLogos } from "../types.ts";
import FileInput from "../components/FileInput.vue";
import { listen } from "@tauri-apps/api/event";
import View from "../components/View.vue";
import { commands, StlModel } from "../bindings.ts";
import ImagePreview from "../components/ImagePreview.vue";
import { useToastStore } from "../stores/toastStore.ts";
import { useReleasesStore } from "../stores/releasesStore.ts";

const toastStore = useToastStore();
const releasesStore = useReleasesStore();
const model: Ref<StlModel> = ref({
  model_name: "",
  description: "",
  tags: [],
  images: [],
  model_files: [],
  group: null,
});
const images = ref<File[]>([]);
const modelFiles = ref<File[]>([]);

const isStoring = ref(false);
const storageProgress = ref(0);
const totalFiles = ref(0);
const processedFiles = ref(0);
const storeFiles = async (images: File[], files: File[], modelName: string) => {
  if (!model.value.model_name) {
    throw new Error("Model name is required for file upload");
  }
  totalFiles.value = images.length + files.length;
  processedFiles.value = 0;
  storageProgress.value = 0;

  const processedImages = [];
  for (const [imageIndex, image] of images.entries()) {
    if (image) {
      const fileData = new Uint8Array(await image.arrayBuffer());
      const fileBlob = await commands.storeImage(
        Array.from(fileData),
        image.name,
        modelName,
        imageIndex,
      );
      if (fileBlob.status === "ok") {
        const filePath = fileBlob.data;
        processedImages.push(filePath);
        processedFiles.value++;
        storageProgress.value = processedFiles.value / totalFiles.value;
      }
    }
  }

  const processedModelFiles = [];
  for (const file of files) {
    const fileData = new Uint8Array(await file.arrayBuffer());
    const fileBlob = await commands.storeModelFile(
      Array.from(fileData),
      file.name,
      modelName,
    );
    if (fileBlob.status === "ok") {
      const filePath = fileBlob.data;
      processedModelFiles.push(filePath);
      processedFiles.value++;
      storageProgress.value = processedFiles.value / totalFiles.value;
    }
  }

  return { processedImages, processedModelFiles };
};

const formComplete = computed(
  () =>
    model.value.model_name &&
    modelFiles.value.length > 0 &&
    images.value.length > 0,
);

const saveModelData = async () => {
  if (!formComplete.value) {
    toastStore.addToast("Please make sure the form is complete", "error", 0);
    return;
  }
  try {
    isStoring.value = true;
    const storedFiles = await storeFiles(
      images.value,
      modelFiles.value,
      model.value.model_name,
    );

    const modelData: StlModel = {
      ...model.value,
      images: storedFiles.processedImages,
      model_files: storedFiles.processedModelFiles,
    };

    console.log("Saving model with release directory:", releasesStore.releaseDir);
    // Make sure releaseDirectoryName is available
    if (!releasesStore.releaseDir) {
      throw new Error("Release directory name is missing");
    }

    await commands.saveModel(
      modelData,
      releasesStore.releaseDir
    );

    console.log("Model saved successfully");
    toastStore.addToast("Model saved successfully", "success");
    releasesStore.addModel(modelData);
    clearModel();
    // Also clear file lists
    modelFiles.value = [];
    images.value = [];
  } catch (error) {
    toastStore.addToast("Failed to save model: " + error, "error", 0);
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
    model_name: "",
    description: "",
    tags: [],
    images: [],
    model_files: [],
  };
  images.value = [];
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
