import { memo } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import { openUrl } from "@tauri-apps/plugin-opener";

// 렌더마다 새로 만들면 ReactMarkdown 이 매번 처음부터 다시 해석한다 — 모듈에 한 번만
const REMARK = [remarkGfm];
const REHYPE: NonNullable<Parameters<typeof ReactMarkdown>[0]["rehypePlugins"]> = [
  [rehypeHighlight, { detect: false, ignoreMissing: true }],
];
const COMPONENTS: Components = {
  a: ({ href, children }) => (
    <a
      href={href}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        if (href && /^https?:/.test(href)) openUrl(href);
      }}
    >
      {children}
    </a>
  ),
};

/** 글이 같으면 다시 해석하지 않는다(대화가 새로 고쳐질 때마다 말풍선 수십 개를 다시 파싱·강조하던 비용) */
export const Markdown = memo(function Markdown({ children, className }: { children: string; className?: string }) {
  return (
    <div className={`md ${className ?? ""}`}>
      <ReactMarkdown remarkPlugins={REMARK} rehypePlugins={REHYPE} components={COMPONENTS}>
        {children}
      </ReactMarkdown>
    </div>
  );
});
