import { ref } from "vue";
import { open } from "@tauri-apps/plugin-dialog";
import { stat } from "@tauri-apps/plugin-fs";
import type { FileInfo } from "@tauri-apps/plugin-fs";
import { convertFileSrc } from "@tauri-apps/api/core";

export interface SelectedFile {
  path: string; // The full file path
  name: string; // The file name
  info: FileInfo; // The standard Tauri FileInfo
  type?: string; // Our custom file type/extension

  getPreviewUrl: () => string;
}

export interface FileFilter {
  name: string;
  extensions: string[];
}

export function useFileSelect() {
  const selectedFiles = ref<SelectedFile[]>([]);

  // Create filters based on accepted file types
  const createFiltersFromAccept = (
    accept: string,
  ): FileFilter[] | undefined => {
    if (!accept) return undefined;

    const filters: FileFilter[] = [];
    const allExtensions: string[] = [];

    const extensions = accept
      .split(",")
      .map((type) => type.trim())
      .map((type) => (type.startsWith(".") ? type.substring(1) : type));

    if (extensions.length) {
      allExtensions.push(...extensions);
    }

    const images = extensions.filter((ext) =>
      ["jpg", "jpeg", "png", "gif", "svg", "webp", "avif", "bmp"].includes(ext),
    );
    const models = extensions.filter((ext) =>
      ["stl", "obj", "3mf", "lys", "chitubox"].includes(ext),
    );
    const documents = extensions.filter((ext) =>
      ["txt", "md", "json", "csv", "xml", "pdf"].includes(ext),
    );

    if (images.length) {
      filters.push({
        name: "Images",
        extensions: images,
      });
    }
    if (models.length) {
      filters.push({
        name: "Models",
        extensions: models,
      });
    }
    if (documents.length) {
      filters.push({
        name: "Documents",
        extensions: documents,
      });
    }

    if (allExtensions.length) {
      filters.push({
        name: "All Supported",
        extensions: allExtensions,
      });
    }

    return filters.length ? filters : undefined;
  };

  const selectDirectory = async (options: {
    title?: string;
  }): Promise<string | null> => {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: options.title || "Select Directory",
      });

      return selected ? (selected as string) : null;
    } catch (error) {
      console.error("Directory selection failed:", error);
      return null;
    }
  };

  const selectFiles = async (options: {
    accept?: string;
    multiple?: boolean;
    title?: string;
  }): Promise<SelectedFile[] | null> => {
    try {
      const filters = options.accept
        ? createFiltersFromAccept(options.accept)
        : undefined;

      const selected = await open({
        multiple: options.multiple,
        filters,
        title: options.title || "Select Files",
      });

      if (!selected) return null;

      const paths = Array.isArray(selected) ? selected : [selected];
      const files: SelectedFile[] = [];

      for (const path of paths) {
        try {
          const fileInfo = await stat(path);
          const fileName = path.split(/[/\\]/).pop() || "";
          const extension = fileName.split(".").pop()?.toLowerCase() || "";

          files.push({
            path,
            name: fileName,
            info: fileInfo,
            type: extension ? `.${extension}` : "Unknown",
            getPreviewUrl() {
              return convertFileSrc(this.path);
            },
          });
        } catch (error) {
          console.error(`Failed to read file metadata for ${path}:`, error);
        }
      }

      return files.length > 0 ? files : null;
    } catch (error) {
      console.error("Selection failed:", error);
      return null;
    }
  };

  // Format file size for display
  const formatFileSize = (size?: number) => {
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

  return {
    selectedFiles,
    selectFiles,
    selectDirectory,
    formatFileSize,
    createFiltersFromAccept,
  };
}
