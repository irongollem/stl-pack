<template>
  <form @submit.prevent="saveModelData" @keydown.enter.prevent">
  <View>
    <template #left>
      <h1 class="text-xl font-bold">Model info</h1>
        <TextInput
            id="model-name"
            v-model="model.name"
            label="Model Name"
            placeholder="Enter model name..."
        />

        <TextArea id="description" placeholder="Enter the description (Optional)..." label="Description" v-model="model.description" />

      <TextInput id="group" label="Group" placeholder="Enter group name (Optional)..." v-model="model.group" />

        <TagInput id="tags" v-model="model.tags" label="Tags" placeholder="Write tags here..." />

        <FileSelect
            id="model-files"
            label="Model Files"
            multiple
            accept=".stl,.obj,.chitubox,.lys,.3mf,.blend,.gcode"
            v-model="modelFiles"
            :enabled="model.name.length > 0"
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
          <button class="btn btn-error" type="button" @click="clearModel">Clear Model</button>
        </div>
    </template>

    <template #right>
      <ImageSelect v-model="images" />
    </template>
  </View>
  </form>
</template>

<script setup lang="ts">
import TextArea from "../components/TextArea.vue";
import TextInput from "../components/TextInput.vue";
import TagInput from "../components/TagInput.vue";
import { computed, ref } from "vue";
import type { Ref } from "vue";
import { fileLogos } from "../types.ts";
import View from "../components/View.vue";
import { commands, type StlModel } from "../bindings.ts";
import ImageSelect from "../components/ImageSelect.vue";
import { useToastStore } from "../stores/toastStore.ts";
import { useReleasesStore } from "../stores/releasesStore.ts";
import type { SelectedFile } from "../composables/useFileSelect";
import FileSelect from "../components/FileSelect.vue";

const toastStore = useToastStore();
const releasesStore = useReleasesStore();
const model: Ref<StlModel> = ref({
  id: "",
  name: "",
  description: "",
  tags: [],
  images: [],
  model_files: [],
  group: null,
});
const images = ref<SelectedFile[]>([]);
const modelFiles = ref<SelectedFile[]>([]);

const isStoring = ref(false);

const formComplete = computed(
  () =>
    model.value.name && modelFiles.value.length > 0 && images.value.length > 0,
);

const saveModelData = async () => {
  if (!formComplete.value) {
    toastStore.addToast("Please make sure the form is complete", "error", 0);
    return;
  }

  try {
    if (!releasesStore.releaseDir) {
      throw new Error("Release directory name is missing");
    }

    const savedModelTupleResult = await commands.addModel(
      model.value,
      releasesStore.releaseDir,
      modelFiles.value.map((f) => f.path),
      images.value.map((f) => f.path),
    );
    if (savedModelTupleResult.status === "ok") {
      toastStore.addToast("Model saved successfully", "success");
      releasesStore.addModel(...savedModelTupleResult.data);
      clearModel();
    } else {
      toastStore.addToast(
        `Failed to save model: ${savedModelTupleResult.error}`,
        "error",
        0,
      );
    }
  } catch (error) {
    toastStore.addToast(`Failed to save model: ${error}`, "error", 0);
  } finally {
    isStoring.value = false;
  }
};

const clearModel = () => {
  model.value = {
    id: "",
    name: "",
    description: "",
    tags: [],
    images: [],
    model_files: [],
    group: "",
  };
  images.value = [];
  modelFiles.value = [];
};

const getLogo = (fileName: string) => {
  const ext = fileName.split(".").pop();
  if (ext === "stl") {
    return fileLogos.stl;
  }
  if (ext === "chitu" || ext === "chitubox") {
    return fileLogos.chitubox;
  }
  if (ext === "lyt" || ext === "lys" || ext === "lychee") {
    return fileLogos.lychee;
  }
  return "tauri.svg";
};
</script>
