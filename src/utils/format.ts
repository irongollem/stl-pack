/** Human-readable size from a byte count, e.g. 2048 -> "2.0 KB". */
export const formatFileSize = (size?: number) => {
  if (!size) return "Unknown";
  let fileSize = size;
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (fileSize >= 1024 && i < units.length - 1) {
    fileSize /= 1024;
    i++;
  }
  return `${fileSize.toFixed(1)} ${units[i]}`;
};

/** Human-readable size from a KB count (the backend reports sizes in KB). */
export const formatBytes = (kb: number) => {
  if (kb === 0) return "0 KB";
  return formatFileSize(kb * 1024);
};

/** Seconds -> "3m 25s". */
export const formatTime = (seconds: number) => {
  const mins = Math.floor(seconds / 60);
  const secs = Math.floor(seconds % 60);
  return `${mins}m ${secs}s`;
};

/** Group a hex fingerprint into fours for humans, e.g. "ab12cd34" -> "ab12 cd34". */
export const formatFingerprint = (fingerprint: string) =>
  fingerprint.match(/.{1,4}/g)?.join(" ") ?? fingerprint;

/**
 * Normalize anything a Result/exception can carry into a readable string —
 * AppError values arrive as one-key objects like { IoError: "..." } and
 * would otherwise render as "[object Object]".
 */
export const describeError = (error: unknown): string => {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object") {
    const [kind, detail] = Object.entries(error)[0] ?? [];
    if (kind && typeof detail === "string") return `${kind}: ${detail}`;
    return JSON.stringify(error);
  }
  return String(error);
};
