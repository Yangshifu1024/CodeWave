// Font preferences ([docs/chat-and-interface](../../../docs/chat-and-interface.md)).
// Config owns persistence; localStorage supplies the pre-mount cache. Save receipts
// update saved state without rewinding a newer local preview.
import { useMemo, useSyncExternalStore } from "react";

/** sans = interface, content = prose, mono = code and logs. */
export type FontSlot = "sans" | "content" | "mono";
export const FONT_SIZE_SLOTS = ["sans", "content", "mono"] as const;
export type FontSizeLevel = "12" | "13" | "14" | "15" | "16" | "17" | "18" | "19" | "20";
export const FONT_SIZE_LEVELS: readonly FontSizeLevel[] =
  ["12", "13", "14", "15", "16", "17", "18", "19", "20"] as const;
export const levelToPx = (level: FontSizeLevel): number => Number(level);
export const DEFAULT_FONT_SIZE_PX: Record<FontSlot, number> = { sans: 14, content: 14, mono: 12 };
export const DEFAULT_FONT_SIZE_LEVEL: Record<FontSlot, FontSizeLevel> = { sans: "14", content: "14", mono: "12" };

export function pxToLevel(px: number): FontSizeLevel | null {
  const level = String(px) as FontSizeLevel;
  return FONT_SIZE_LEVELS.includes(level) ? level : null;
}

export interface FontPreferences {
  sans: string;
  content: string;
  mono: string;
  fontSizeSans: FontSizeLevel;
  fontSizeContent: FontSizeLevel;
  fontSizeMono: FontSizeLevel;
}

export const DEFAULT_FONT_LEADS: Record<FontSlot, string> = { sans: "System", content: "System", mono: "SF Mono" };
const STORAGE_KEYS: Record<FontSlot, string> = { sans: "ws_font_sans", content: "ws_font_content", mono: "ws_font_mono" };
const SIZE_STORAGE_KEYS: Record<FontSlot, string> = {
  sans: "ws_font_size_sans", content: "ws_font_size_content", mono: "ws_font_size_mono",
};
const SIZE_PREF_KEYS = { sans: "fontSizeSans", content: "fontSizeContent", mono: "fontSizeMono" } as const;
const BACKEND_SIZE_KEYS = { sans: "font_size_sans", content: "font_size_content", mono: "font_size_mono" } as const;

export function fontSizeLevel(prefs: FontPreferences, slot: FontSlot): FontSizeLevel {
  return prefs[SIZE_PREF_KEYS[slot]];
}

const fontListeners = new Set<() => void>();
function subscribeFonts(listener: () => void): () => void {
  fontListeners.add(listener);
  return () => { fontListeners.delete(listener); };
}

export function useFontPreferences(): FontPreferences {
  const snapshot = useSyncExternalStore(subscribeFonts, () => JSON.stringify(readStoredFonts()));
  return useMemo(() => {
    const prefs = JSON.parse(snapshot) as FontPreferences;
    for (const slot of FONT_SIZE_SLOTS) prefs[slot] = sanitizeFontList(prefs[slot]);
    return prefs;
  }, [snapshot]);
}

export function useFontSize(slot: FontSlot): number {
  return useSyncExternalStore(subscribeFonts, () => levelToPx(fontSizeLevel(readStoredFonts(), slot)));
}

/** Preserve title and metadata hierarchy relative to each slot's default. */
export function fontSizeCss(defaultPx: number, slot: FontSlot = "sans"): string {
  return `calc(${defaultPx}px + var(--ws-font-size-${slot}-offset))`;
}

const FORBIDDEN_CHARS = /["'\\;{}<>]|\p{C}/gu;
export function sanitizeFontList(input: string): string {
  return input.split(",")
    .map((segment) => segment.replace(FORBIDDEN_CHARS, "").replace(/\s+/g, " ").trim())
    .filter((segment) => segment !== "").join(", ");
}

export function buildFontOverride(sanitized: string, slot: FontSlot): string {
  if (sanitized === "") return "";
  const leading = sanitized.split(",").map((name) => `"${name.trim()}"`).join(", ");
  return `${leading}, var(--ws-font-${slot}-fallback)`;
}

export function previewFontFamily(sanitized: string, slot: FontSlot): string {
  return buildFontOverride(sanitized, slot) || `var(--ws-font-${slot}-fallback)`;
}

// Only the old localStorage cache used these names. Config sizes are numeric.
const LEGACY_LEVEL_PX = new Map<string, FontSizeLevel>([
  ["small", "13"], ["default", "15"], ["large", "17"], ["xlarge", "19"],
]);
export function sanitizeFontLevel(input: string, slot: FontSlot = "sans"): FontSizeLevel {
  const raw = input.trim();
  return LEGACY_LEVEL_PX.get(raw) ??
    (FONT_SIZE_LEVELS.includes(raw as FontSizeLevel) ? raw as FontSizeLevel : DEFAULT_FONT_SIZE_LEVEL[slot]);
}
export function sanitizeFontLevelStr(input: string, slot: FontSlot = "sans"): FontSizeLevel {
  return sanitizeFontLevel(input, slot);
}

function applyFont(slot: FontSlot, sanitized: string): void {
  const property = `--ws-font-${slot}`;
  const value = buildFontOverride(sanitized, slot);
  if (value === "") document.documentElement.style.removeProperty(property);
  else document.documentElement.style.setProperty(property, value);
}
function applyFontSize(slot: FontSlot, level: FontSizeLevel): void {
  const property = `--ws-font-size-${slot}`;
  if (level === DEFAULT_FONT_SIZE_LEVEL[slot]) document.documentElement.style.removeProperty(property);
  else document.documentElement.style.setProperty(property, `${levelToPx(level)}px`);
}

export function readStoredFonts(): FontPreferences {
  const get = (key: string) => typeof localStorage === "undefined" ? "" : localStorage.getItem(key) ?? "";
  return {
    sans: get(STORAGE_KEYS.sans), content: get(STORAGE_KEYS.content), mono: get(STORAGE_KEYS.mono),
    fontSizeSans: sanitizeFontLevel(get(SIZE_STORAGE_KEYS.sans), "sans"),
    fontSizeContent: sanitizeFontLevel(get(SIZE_STORAGE_KEYS.content), "content"),
    fontSizeMono: sanitizeFontLevel(get(SIZE_STORAGE_KEYS.mono), "mono"),
  };
}

/** Called by main.tsx before React mounts, preventing a default-font flash. */
export function applyInitialFonts(): void {
  const prefs = readStoredFonts();
  for (const slot of FONT_SIZE_SLOTS) {
    applyFont(slot, sanitizeFontList(prefs[slot]));
    applyFontSize(slot, fontSizeLevel(prefs, slot));
  }
}

export function storeFonts(prefs: FontPreferences): FontPreferences {
  const saved = { ...prefs };
  for (const slot of FONT_SIZE_SLOTS) {
    saved[slot] = sanitizeFontList(prefs[slot]);
    saved[SIZE_PREF_KEYS[slot]] = sanitizeFontLevel(fontSizeLevel(prefs, slot), slot);
    const level = fontSizeLevel(saved, slot);
    if (typeof localStorage !== "undefined") {
      if (saved[slot] === "") localStorage.removeItem(STORAGE_KEYS[slot]);
      else localStorage.setItem(STORAGE_KEYS[slot], saved[slot]);
      if (level === DEFAULT_FONT_SIZE_LEVEL[slot]) localStorage.removeItem(SIZE_STORAGE_KEYS[slot]);
      else localStorage.setItem(SIZE_STORAGE_KEYS[slot], level);
    }
    applyFont(slot, saved[slot]);
    applyFontSize(slot, level);
  }
  for (const listener of fontListeners) listener();
  return saved;
}

export function commitFontSlot(slot: FontSlot, candidate: string): FontPreferences {
  return storeFonts({ ...readStoredFonts(), [slot]: candidate });
}
export function commitFontSizeSlot(slot: FontSlot, level: FontSizeLevel): FontPreferences {
  return storeFonts({ ...readStoredFonts(), [SIZE_PREF_KEYS[slot]]: level });
}

export interface BackendFontPrefs {
  sans?: string;
  content?: string;
  mono?: string;
  font_size_sans?: number;
  font_size_content?: number;
  font_size_mono?: number;
}

/** Reconcile only on config load. Receipts must not overwrite optimistic edits. */
export function reconcileFontsFromConfig(backend: BackendFontPrefs): { prefs: FontPreferences; migrate: boolean } {
  const prefs = readStoredFonts();
  const families = { sans: sanitizeFontList(backend.sans ?? ""),
    content: sanitizeFontList(backend.content ?? ""), mono: sanitizeFontList(backend.mono ?? "") };
  // Preserve the established cache-only family import when all saved families
  // are empty. New serde size defaults must not suppress that import.
  const hasBackendFamilies = FONT_SIZE_SLOTS.some((slot) => families[slot] !== "");
  let migrate = false;
  for (const slot of FONT_SIZE_SLOTS) {
    prefs[slot] = hasBackendFamilies ? families[slot] : sanitizeFontList(prefs[slot]);
    const size = backend[BACKEND_SIZE_KEYS[slot]];
    const cachedSize = fontSizeLevel(prefs, slot);
    prefs[SIZE_PREF_KEYS[slot]] = size == null ? cachedSize : pxToLevel(size) ?? DEFAULT_FONT_SIZE_LEVEL[slot];
    migrate ||= (!hasBackendFamilies && prefs[slot] !== "") ||
      (size == null && cachedSize !== DEFAULT_FONT_SIZE_LEVEL[slot]);
  }
  storeFonts(prefs);
  return { prefs, migrate };
}
