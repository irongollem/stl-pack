import { createApp } from "vue";
import App from "./App.vue";
import "./assets/styles.css";
import { createPinia } from "pinia";

createApp(App).use(createPinia()).mount("#app");
