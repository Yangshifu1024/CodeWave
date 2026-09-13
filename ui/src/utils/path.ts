/** 受管数据目录名（与后端 core/config.rs 的 MANAGED_DIR_NAME 同源；改动需两端同步） */
export const MANAGED_DIR_NAME = ".codewave";

/** 取路径末端的目录/文件名：Windows 反斜杠与 POSIX 分隔符统一归一处理 */
export function baseName(p: string): string {
  return p.replace(/\\/g, "/").split("/").filter(Boolean).pop() || p;
}
