import type { PendingImage } from "./run.types";

export const IMAGE_MAX_BYTES = 5 * 1024 * 1024;
export const IMAGE_MAX_COUNT = 4;
export const IMAGE_MAX_TOTAL_BASE64 = 20 * 1024 * 1024;
export type AppendResult = { status: "stale" } | {
  status: "appended"; accepted: number; rejected: number; reason?: "count" | "total";
};

/** Preserve current images; count truncates the new batch, total budget rejects it. */
export function mergeDraftImages(current: PendingImage[], incoming: PendingImage[]) {
  const seen = new Set(current.map((image) => image.id));
  const unique = incoming.filter((image) => {
    if (seen.has(image.id)) return false;
    seen.add(image.id);
    return true;
  });
  const accepted = unique.slice(0, Math.max(0, IMAGE_MAX_COUNT - current.length));
  if ([...current, ...accepted].reduce((total, image) => total + image.data.length, 0) > IMAGE_MAX_TOTAL_BASE64) {
    return { images: current, result: { status: "appended", accepted: 0, rejected: unique.length, reason: "total" } as AppendResult };
  }
  return {
    images: [...current, ...accepted],
    result: { status: "appended", accepted: accepted.length, rejected: unique.length - accepted.length,
      ...(accepted.length < unique.length ? { reason: "count" as const } : {}) } as AppendResult,
  };
}
