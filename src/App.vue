<script setup lang="ts">
import { computed, defineAsyncComponent } from "vue";
import BlenderSetupDialog from "./components/BlenderSetupDialog.vue";
import ImportPackageModal from "./components/ImportPackageModal.vue";
import Sidebar from "./components/Sidebar.vue";
import ToastContainer from "./components/ToastContainer.vue";
import { use3DPackageHandler } from "./composables/use3DPackageHandler";
import { useReleasesStore } from "./stores/releasesStore";
// Catalog is the boot tab, so it loads eagerly. Every other view is lazy:
// static imports here made first paint haul the whole app — including
// three.js and both 3D viewports — before the window showed anything (the
// white flash on a fresh boot), and each new tool made it worse. Lazy
// views also let the bundler code-split instead of shipping one chunk.
import Catalog from "./views/Catalog.vue";

const BaseCutter = defineAsyncComponent(() => import("./views/BaseCutter.vue"));
const Minihoard = defineAsyncComponent(() => import("./views/Minihoard.vue"));
const Releases = defineAsyncComponent(() => import("./views/Releases.vue"));
const Render = defineAsyncComponent(() => import("./views/Render.vue"));
const Settings = defineAsyncComponent(() => import("./views/Settings.vue"));

const {
  pendingImport,
  importing,
  confirmImport,
  confirmRecompile,
  cancelImport,
} = use3DPackageHandler();
const releasesStore = useReleasesStore();

const currentTabComponent = computed(() => {
  switch (releasesStore.activeTab) {
    case "catalog":
      return Catalog;
    case "releases":
      return Releases;
    case "render":
      return Render;
    case "basecutter":
      return BaseCutter;
    case "minihoard":
      return Minihoard;
    case "settings":
      return Settings;
    default:
      return Catalog;
  }
});
</script>

<template>
  <div class="h-screen flex bg-base-100 text-base-content">
    <Sidebar />
    <div class="flex-1 min-w-0 overflow-hidden">
      <KeepAlive>
        <component :is="currentTabComponent" class="h-full" />
      </KeepAlive>
    </div>
  </div>

  <ToastContainer />
  <!-- Outside the KeepAlive so first-run setup overlays every tab -->
  <BlenderSetupDialog />
  <ImportPackageModal
    :inspection="pendingImport?.inspection ?? null"
    :importing="importing"
    @confirm="confirmImport"
    @recompile="confirmRecompile"
    @cancel="cancelImport"
  />
</template>
