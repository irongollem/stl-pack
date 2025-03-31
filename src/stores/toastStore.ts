import {defineStore} from "pinia";
import {ref} from "vue";

export type ToastType = "success" | "error" | "warning" | "info";
export interface Toast {
    id: string;
    message: string;
    type: ToastType;
    duration: number;
}

export const useToastStore = defineStore("toast", () => {
    const toasts = ref<Toast[]>([]);
    let nextId = 0;

    const addToast = (message: string, type: ToastType = "info", duration: number = 5000) => {
        const id = `toast-${nextId++}`;
        const toast = { id, message, type, duration };
        toasts.value.push(toast);
        if (duration > 0) {
            setTimeout(() => {
                removeToast(id);
            }, duration);
        }
        return id;
    }

    const removeToast = (id: string) => {
        toasts.value = toasts.value.filter(toast => toast.id !== id);
    }

    return { toasts, addToast, removeToast };
});
