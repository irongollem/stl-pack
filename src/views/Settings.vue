
<template>
  <View>
    <template #left>
      <h1 class="text-xl font-bold">Settings</h1>
      <form class="mt-4 space-y-6">
        <!-- Scratch Directory Selection -->
        <DirInput id="scratch_dir" label="Temporary Files Directory" v-model="settings.scratch_dir" tooltip="Your files will be temporarily stored here before being compressed." />

        <!-- Target Directory Selection -->
        <DirInput id="target_dir" label="Target Directory" v-model="settings.target_dir" tooltip="Your compressed files will be saved here." />

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
                :aria-label="type"
                name="compression_type"
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
import { ref, onMounted } from "vue";
import { commands, Settings, CompressionType } from "../bindings.ts";
import DirInput from "../components/DirInput.vue";

// Settings state
const settings = ref<Settings>({
  scratch_dir: null,
  target_dir: null,
  compression_type: "Zip"
});

// Load settings on component mount
onMounted(async () => {
  try {
    const savedSettings = await commands.getSettings();
    if (savedSettings.status === "ok") {
      settings.value = savedSettings.data;
    }
  } catch (error) {
    console.error("Failed to load settings:", error);
  }
});

// Save settings to backend
const saveSettings = async () => {
  try {
    const result = await commands.setSettings(settings.value);
    if (result.status === "error") {
      console.error("Failed to save settings:", result.error);
    }
  } catch (error) {
    console.error("Error saving settings:", error);
  }
};

// Available compression types
const compressionTypes: CompressionType[] = ["Zip", "Tar", "TarGz", "TarBz2"];
</script>
