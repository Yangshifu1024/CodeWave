import {
  CodeOutlined,
  FileExcelOutlined,
  FileImageOutlined,
  FileMarkdownOutlined,
  FileOutlined,
  FilePdfOutlined,
  FilePptOutlined,
  FileTextOutlined,
  FileWordOutlined,
  FileZipOutlined,
  PythonOutlined,
} from "@ant-design/icons";

const FILE_TYPES = [
  { extensions: "py pyw ipynb", icon: PythonOutlined },
  { extensions: "sh bash zsh fish bat cmd ps1", icon: CodeOutlined },
  { extensions: "js jsx ts tsx rs go java c h cpp hpp cs rb php swift kt vue svelte html css scss sql", icon: CodeOutlined },
  { extensions: "md markdown", icon: FileMarkdownOutlined },
  { extensions: "png jpg jpeg gif webp svg bmp ico avif tif tiff", icon: FileImageOutlined },
  { extensions: "xls xlsx xlsm csv tsv ods", icon: FileExcelOutlined },
  { extensions: "doc docx odt rtf", icon: FileWordOutlined },
  { extensions: "pdf", icon: FilePdfOutlined },
  { extensions: "ppt pptx odp", icon: FilePptOutlined },
  { extensions: "zip tar gz bz2 xz 7z rar", icon: FileZipOutlined },
  { extensions: "txt log rst json jsonl yaml yml toml ini conf xml", icon: FileTextOutlined },
].map(group => ({ ...group, extensions: new Set(group.extensions.split(" ")) }));

/** Shared by the session overview and Files tab; unknown extensions use a plain file icon. */
export default function SessionFileIcon({ path }: { path: string }) {
  const name = path.split(/[\\/]/).pop() ?? "";
  const dot = name.lastIndexOf(".");
  const extension = dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
  const Icon = FILE_TYPES.find(group => group.extensions.has(extension))?.icon ?? FileOutlined;
  return <Icon />;
}
