
<template>
  <View>
    <template #left>
      <h1 class="text-xl font-bold">Settings</h1>
      <form class="mt-4 space-y-6">
        <FileSelect
          id="scratch_dir"
          label="Temporary Files Directory"
          dir-mode
          v-model="settings.scratch_dir"
          tooltip="Your files will be temporarily stored here before being compressed."
        />

        <FileSelect
          id="target_dir"
          label="Target Directory"
          dir-mode
          v-model="settings.target_dir"
          tooltip="Your compressed files will be saved here."
        />

        <!-- Compression Type -->
        <div class="form-control">
          <label class="floating-label">
            <span class="label">Compression Type</span>
          </label>
          <div class="join">
            <input
                type="radio"
                v-for="type in compressionTypes"
                :key="type"
                class="btn join-item"
                :class="{'btn-active btn-primary': settings.compression_type === type }"
                :aria-label="type === 'Zip' ? 'ZIP' : type === 'SevenZip' ? '7-Zip' : type === 'TarGz' ? 'TAR.GZ' : 'TAR.XZ'"
                name="compression_type"
                :value="type"
                v-model="settings.compression_type"
            />
          </div>
        </div>
      </form>
    </template>
  </View>
</template>

<script setup lang="ts">
import View from "../components/View.vue";
import FileSelect from "../components/FileSelect.vue";
import { ref, onMounted, watch } from "vue";
import { commands, type Settings, type CompressionType } from "../bindings.ts";
// Settings state
const settings = ref<Settings>({
  scratch_dir: null,
  target_dir: null,
  compression_type: "Zip",
  chunk_size: null,
});

watch(
  settings,
  async () => {
    console.log("triggered once again!");
    await saveSettings();
  },
  { deep: true },
);

// Load settings on component mount
onMounted(async () => {
  try {
    console.log("before: ", settings.value);
    const savedSettings = await commands.getSettings();
    if (savedSettings.status === "ok") {
      savedSettings.data.compression_type =
        savedSettings.data.compression_type || "Zip";
      settings.value = savedSettings.data;
      console.log("saved value", settings.value);
    }
  } catch (error) {
    console.error("Failed to load settings:", error);
  }
});

// Save settings to backend
const saveSettings = async () => {
  try {
    console.log("saving settings: ", settings.value);
    const result = await commands.setSettings(settings.value);
    if (result.status === "error") {
      console.error("Failed to save settings:", result.error);
    }
  } catch (error) {
    console.error("Error saving settings:", error);
  }
};

// Available compression types
const compressionTypes: CompressionType[] = [
  "Zip",
  "SevenZip",
  "TarGz",
  "TarXz",
];
</script>
