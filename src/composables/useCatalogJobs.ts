import type { UnlistenFn } from "@tauri-apps/api/event";
import { computed, onMounted, onUnmounted, ref } from "vue";
import {
  commands,
  events,
  type DuplicateStatus,
  type GeometryStatus,
  type ScanStatus,
} from "../bindings";

/**
 * Tracks the catalog's background jobs (disk scan + duplicate analysis +
 * geometry mining) driven by the Rust scan-status / duplicate-status /
 * geometry-status event streams.
 */
export function useCatalogJobs() {
  const scanStatus = ref<ScanStatus | null>(null);
  const scanJobId = ref("");
  const dupStatus = ref<DuplicateStatus | null>(null);
  const dupJobId = ref("");
  const geoStatus = ref<GeometryStatus | null>(null);
  const geoJobId = ref("");
  /** Bumped when a scan finishes so views can refresh their queries. */
  const scanCompletedCount = ref(0);
  const dupCompletedCount = ref(0);
  const geoCompletedCount = ref(0);

  let unlistenScan: UnlistenFn | null = null;
  let unlistenDup: UnlistenFn | null = null;
  let unlistenGeo: UnlistenFn | null = null;

  onMounted(async () => {
    unlistenScan = await events.scanStatus.listen((event) => {
      scanStatus.value = event.payload;
      if (
        "Completed" in event.payload ||
        "Failed" in event.payload ||
        "Cancelled" in event.payload
      ) {
        scanJobId.value = "";
        if ("Completed" in event.payload) scanCompletedCount.value++;
      }
    });
    unlistenDup = await events.duplicateStatus.listen((event) => {
      dupStatus.value = event.payload;
      if (
        "Completed" in event.payload ||
        "Failed" in event.payload ||
        "Cancelled" in event.payload
      ) {
        dupJobId.value = "";
        if ("Completed" in event.payload) dupCompletedCount.value++;
      }
    });
    unlistenGeo = await events.geometryStatus.listen((event) => {
      geoStatus.value = event.payload;
      if (
        "Completed" in event.payload ||
        "Failed" in event.payload ||
        "Cancelled" in event.payload
      ) {
        geoJobId.value = "";
        if ("Completed" in event.payload) geoCompletedCount.value++;
      }
    });
  });

  onUnmounted(() => {
    unlistenScan?.();
    unlistenDup?.();
    unlistenGeo?.();
  });

  const isScanning = computed(
    () =>
      !!scanJobId.value ||
      (scanStatus.value !== null &&
        ("Started" in scanStatus.value || "Progress" in scanStatus.value)),
  );

  const isFindingDuplicates = computed(
    () =>
      !!dupJobId.value ||
      (dupStatus.value !== null &&
        ("Started" in dupStatus.value || "Progress" in dupStatus.value)),
  );

  const isMiningGeometry = computed(
    () =>
      !!geoJobId.value ||
      (geoStatus.value !== null &&
        ("Started" in geoStatus.value || "Progress" in geoStatus.value)),
  );

  const scanProgress = computed(() =>
    scanStatus.value && "Progress" in scanStatus.value
      ? scanStatus.value.Progress
      : null,
  );

  const scanError = computed(() =>
    scanStatus.value && "Failed" in scanStatus.value
      ? scanStatus.value.Failed.error
      : null,
  );

  const dupProgress = computed(() =>
    dupStatus.value && "Progress" in dupStatus.value
      ? dupStatus.value.Progress
      : null,
  );

  const dupSummary = computed(() =>
    dupStatus.value && "Completed" in dupStatus.value
      ? dupStatus.value.Completed
      : null,
  );

  const geoProgress = computed(() =>
    geoStatus.value && "Progress" in geoStatus.value
      ? geoStatus.value.Progress
      : null,
  );

  const geoSummary = computed(() =>
    geoStatus.value && "Completed" in geoStatus.value
      ? geoStatus.value.Completed
      : null,
  );

  const startScan = async (root: string) => {
    scanStatus.value = null;
    const result = await commands.startCatalogScan(root);
    if (result.status === "ok") scanJobId.value = result.data;
    return result;
  };

  const startDuplicateScan = async () => {
    dupStatus.value = null;
    const result = await commands.startDuplicateScan();
    if (result.status === "ok") dupJobId.value = result.data;
    return result;
  };

  const startGeometryScan = async () => {
    geoStatus.value = null;
    const result = await commands.startGeometryScan();
    if (result.status === "ok") geoJobId.value = result.data;
    return result;
  };

  const cancelScan = async () => {
    if (scanJobId.value) await commands.cancelCatalogJob(scanJobId.value);
  };

  const cancelDuplicateScan = async () => {
    if (dupJobId.value) await commands.cancelCatalogJob(dupJobId.value);
  };

  const cancelGeometryScan = async () => {
    if (geoJobId.value) await commands.cancelCatalogJob(geoJobId.value);
  };

  return {
    isScanning,
    scanProgress,
    scanError,
    scanCompletedCount,
    startScan,
    cancelScan,
    isFindingDuplicates,
    dupProgress,
    dupSummary,
    dupCompletedCount,
    startDuplicateScan,
    cancelDuplicateScan,
    isMiningGeometry,
    geoProgress,
    geoSummary,
    geoCompletedCount,
    startGeometryScan,
    cancelGeometryScan,
  };
}
