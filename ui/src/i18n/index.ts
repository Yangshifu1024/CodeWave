import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import zhCN from "./zh-CN";
import enUS from "./en-US";

// The language persistence key matches the Vue version (ws_lang); the dictionary structure is identical to the original vue-i18n one
void i18n.use(initReactI18next).init({
  lng: localStorage.getItem("ws_lang") || "zh-CN",
  fallbackLng: "zh-CN",
  resources: {
    "zh-CN": { translation: zhCN },
    "en-US": { translation: enUS },
  },
  interpolation: { escapeValue: false },
  react: { useSuspense: false },
});

export { i18n };
