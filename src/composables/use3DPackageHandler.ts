import { listen } from "@tauri-apps/api/event";
import { onMounted, onUnmounted, ref } from "vue";
import { commands, type PackageInspection } from "../bindings";
import { useToastStore } from "../stores/toastStore";
import { selectDirectory } from "./useFileSelect";

export interface PendingImport {
  filePath: string;
  library: string;
  /** The catalog root owning the destination, when there is one. */
  ownerRoot: string | null;
  inspection: PackageInspection;
}

/**
 * The receiving end of the 3pk format: a `release.3pk` arriving via OS file
 * association or drag-drop is first INSPECTED — every component diffed by
 * checksum against what the library already holds — and the result drives
 * the selective-import dialog (new release: everything pre-checked; update:
 * only the changed components). The confirmed import verifies each
 * component against the manifest checksums, rematerializes dedup-elided
 * files, and a catalog scan restores the packed curation. confirmRecompile
 * is the fallback for what the archive path can't cover: it materializes
 * owned files from anywhere in the library by checksum, even when a
 * component's sibling archive is missing entirely.
 */
export function use3DPackageHandler() {
  const toastStore = useToastStore();
  const pendingImport = ref<PendingImport | null>(null);
  const importing = ref(false);
  let unlistenFn: (() => void) | null = null;

  const handle3DPackage = async (filePath: string) => {
    try {
      // Default destination: the first catalog folder, so the release is
      // scanned like everything else; ask only when none is configured
      const settings = await commands.getSettings();
      const catalogRoots =
        (settings.status === "ok" &&
          (settings.data.catalog_roots ??
            (settings.data.catalog_root
              ? [settings.data.catalog_root]
              : []))) ||
        [];
      const library =
        catalogRoots[0] ||
        (await selectDirectory({ title: "Import into which folder?" }));
      if (!library) return;
      const ownerRoot =
        catalogRoots.find(
          (root) =>
            library === root ||
            library.startsWith(`${root}/`) ||
            library.startsWith(`${root}\\`),
        ) ?? null;

      const result = await commands.inspectReleasePackage(filePath, library);
      if (result.status !== "ok") {
        toastStore.reportError("Could not read the package", result.error);
        return;
      }
      pendingImport.value = {
        filePath,
        library,
        ownerRoot,
        inspection: result.data,
      };
    } catch (error) {
      toastStore.reportError("Failed to import 3D package", error);
    }
  };

  const confirmImport = async (components: string[]) => {
    const pending = pendingImport.value;
    if (!pending || importing.value) return;
    importing.value = true;
    try {
      const result = await commands.importRelease(
        pending.filePath,
        pending.library,
        components,
      );
      if (result.status !== "ok") {
        toastStore.reportError("Import failed", result.error);
        return;
      }
      const outcome = result.data;
      for (const error of outcome.errors) toastStore.addToast(error, "error");
      for (const warning of outcome.warnings)
        toastStore.addToast(warning, "warning");
      toastStore.addToast(
        `${outcome.updated ? "Updated" : "Imported"} "${outcome.release_name}" by ${outcome.designer} — ${outcome.components} component${outcome.components === 1 ? "" : "s"}, ${outcome.files} files, verified`,
        "success",
      );
      pendingImport.value = null;

      // Index it right away when it landed inside a catalog folder; the
      // scan also restores the packed curation from the model.json
      // sidecars. Only the OWNING folder rescans — not the whole catalog.
      if (pending.ownerRoot) {
        await commands.startCatalogScan(pending.ownerRoot);
      }
    } catch (error) {
      toastStore.reportError("Failed to import 3D package", error);
    } finally {
      importing.value = false;
    }
  };

  const confirmRecompile = async (components: string[] | null) => {
    const pending = pendingImport.value;
    if (!pending || importing.value) return;
    importing.value = true;
    try {
      const result = await commands.recompileReleaseFromLibrary(
        pending.filePath,
        pending.library,
        components,
      );
      if (result.status !== "ok") {
        toastStore.reportError(
          "Recompile from your library failed",
          result.error,
        );
        return;
      }
      const outcome = result.data;
      for (const error of outcome.errors) toastStore.addToast(error, "error");
      for (const warning of outcome.warnings)
        toastStore.addToast(warning, "warning");
      const landedFiles = outcome.components.reduce(
        (sum, c) => sum + c.files_landed,
        0,
      );
      const completeCount = outcome.components.filter((c) => c.complete).length;
      const allComplete = completeCount === outcome.components.length;
      toastStore.addToast(
        `Recompiled "${outcome.release_name}" from your library — ${completeCount} of ${outcome.components.length} component${outcome.components.length === 1 ? "" : "s"} complete, ${landedFiles} files landed`,
        allComplete ? "success" : "warning",
      );
      pendingImport.value = null;

      if (pending.ownerRoot) {
        await commands.startCatalogScan(pending.ownerRoot);
      }
    } catch (error) {
      toastStore.reportError("Failed to recompile from your library", error);
    } finally {
      importing.value = false;
    }
  };

  const cancelImport = () => {
    if (!importing.value) pendingImport.value = null;
  };

  onMounted(async () => {
    // Drag-drop on a running app
    unlistenFn = await listen("3dpak-open", (event) => {
      handle3DPackage(event.payload as string);
    });

    // A file opened via OS file association arrives before this listener
    // exists (Tauri events don't queue), so the backend parks it for us
    try {
      const pending = await commands.getPending3dpak();
      if (pending) await handle3DPackage(pending);
    } catch (error) {
      console.error("Failed to check for a pending 3D package:", error);
    }
  });

  onUnmounted(() => {
    unlistenFn?.();
  });

  return {
    handle3DPackage,
    pendingImport,
    importing,
    confirmImport,
    confirmRecompile,
    cancelImport,
  };
}
