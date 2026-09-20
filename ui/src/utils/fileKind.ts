// 按扩展名判断文件类型（预览分派与附件入口共用）。
// 放在 utils 而不是某个组件里：附件 hook 也需要判断「这是不是图片」，
// 若从 FileViewerModal 里取，会把 markdown / mermaid / antd 那一大串一起拖进输入区的依赖里。

const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico"]);
const MD_EXTS = new Set(["md", "markdown"]);

/** 取路径扩展名（小写；无扩展名返回空串）。 */
export function extOf(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? path;
  const i = name.lastIndexOf(".");
  return i >= 0 ? name.slice(i + 1).toLowerCase() : "";
}

/** 是否图片扩展名。 */
export function isImagePath(path: string): boolean {
  return IMAGE_EXTS.has(extOf(path));
}

/** 是否 markdown 扩展名。 */
export function isMarkdownPath(path: string): boolean {
  return MD_EXTS.has(extOf(path));
}
