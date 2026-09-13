// Lightweight code block (replaces naive-ui NCode): highlight.js highlighting, constant dark background
import { useEffect, useRef } from "react";
import hljs from "highlight.js";

export default function CodeBlock({ code, language }: { code: string; language?: string }) {
  const ref = useRef<HTMLPreElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (language && hljs.getLanguage(language)) {
      try {
        el.innerHTML = hljs.highlight(code, { language, ignoreIllegals: true }).value;
        return;
      } catch {
        /* fallthrough */
      }
    }
    el.textContent = code;
  }, [code, language]);

  return <pre ref={ref} className="code-block" />;
}
