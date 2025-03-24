<script setup lang="ts">
import MonthYearInput from "../components/MonthYearInput.vue";
import TextArea from "../components/TextArea.vue";
import TextInput from "../components/TextInput.vue";
import TagInput from "../components/TagInput.vue";
import { Ref, ref } from "vue";
import {fileLogos, FileMeta, StlModel} from "../types.ts";
import { invoke } from "@tauri-apps/api/core";
import FileInput from "../components/FileInput.vue";
import ModalView from "../components/ModalView.vue";

const model: Ref<StlModel> = ref({
  modelName: "",
  releaseDate: "",
  designer: "",
  release: "",
  description: "",
  tags: [],
  pictures: [],
  modelFiles: [],
});

const imagePreviews = ref<string[]>([]);
const selectedImageIndex = ref(0);

const saveModelData = async () => {
  await invoke("save_model", model.value);
  alert("Model saved successfully!"); // FIXME: Show a toast message instead

  clearModel();
};

const clearModel = () => {
  model.value = {
    modelName: "",
    releaseDate: "",
    designer: "",
    release: "",
    description: "",
    tags: [],
    pictures: [],
    modelFiles: [],
  };
  imagePreviews.value = [];
  selectedImageIndex.value = 0;
};

const imageDetailViewOpen = ref(false);

const updateImagePreviews = (files: FileMeta[]) => {
  console.log("I was called!", files);
  imagePreviews.value = files.map((file) => file.path);
};

const removeImage = (index: number) => {
  // Remove from model.pictures
  model.value.pictures.splice(index, 1);
  model.value.pictures = [...model.value.pictures];

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
  const ext = fileName.split('.').pop();
  if (ext === 'stl') {
    return fileLogos.stl;
  } else if (ext === 'chitu' || ext === 'chitubox') {
    return fileLogos.chitubox;
  } else if (ext === 'lyt' || ext === 'lys' || ext === 'lychee') {
    return fileLogos.lychee;
  } else {
    return 'tauri.svg';
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
              v-model="model.pictures"
              @update:modelValue="updateImagePreviews" />

          <FileInput
              id="model-files"
              label="Model Files"
              multiple
              accept=".stl,.obj,.chitubox,.lys,.3mf,.blend,.gcode,.png"
              v-model="model.modelFiles" />

          <ul v-if="model.modelFiles.length > 0" class="list">
            <li v-for="modelFile in model.modelFiles" :key="modelFile.name" class="list-row">
              <div>
                <img class="size-8 rounded-box" :src="getLogo(modelFile.name)" alt="File icon" />
              </div>
              <div>
              {{modelFile.name}}
              </div>
              <div>
                <button class="btn btn-xs btn-error" @click="model.modelFiles.splice(model.modelFiles.indexOf(modelFile), 1)">Remove</button>
              </div>
            </li>
          </ul>

          <div class="flex justify-between w-full">
            <button class="btn" type="submit">Save Model</button>
            <button class="btn btn-error" @click="clearModel">Clear Model</button>
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
