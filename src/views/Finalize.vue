<template>
<button
    class="btn btn-success"
    @click="finalizeRelease"
    :disabled="!releasesStore.modelCount"
>
    Finalize & Export Release
</button>
</template>

<script setup lang="ts">
import { useToastStore } from "../stores/toastStore.ts";
import { commands } from "../bindings.ts";
import { useReleasesStore } from "../stores/releasesStore";

const toastStore = useToastStore();
const releasesStore = useReleasesStore();

const finalizeRelease = async () => {
  if (releasesStore.release) {
    try {
      const result = await commands.finalizeRelease(
        releasesStore.release.release_dir,
      );
      if (result.status === "ok") {
        toastStore.addToast(
          "Release finalized and exported successfully",
          "success",
        );
      } else {
        console.error("Finalize release failed:", result.error);
        toastStore.addToast(
          `Failed to finalize release: ${
            typeof result.error === "string"
              ? result.error
              : JSON.stringify(result.error)
          }`,
          "error",
          0,
        );
      }
    } catch (error) {
      console.error("Finalize exception:", error);
      toastStore.addToast(
        `Exception during finalization: ${error}`,
        "error",
        0,
      );
    }
  }
};
</script>
