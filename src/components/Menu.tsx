import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Check, MoreHorizontal } from "lucide-react";

/** 더보기 메뉴의 한 줄 — 누르는 항목 · 켜고 끄는 항목(checked) · 구분선 · 읽기만 하는 정보 줄 */
export type MenuEntry =
  | { label: string; icon?: React.ReactNode; hint?: string; title?: string; onClick: () => void; disabled?: boolean; checked?: boolean; danger?: boolean }
  | "sep"
  | { info: React.ReactNode };

/**
 * "⋯ 더보기" — 머리줄에 늘 보이지 않아도 되는 동작을 한 곳에 접는다.
 * 키보드: 열면 첫 항목에 포커스 · ↑↓ 이동 · Esc 로 닫고 단추로 돌아간다.
 */
export function MoreMenu({ items, title = "더보기", label, className = "", align = "right" }: { items: MenuEntry[]; title?: string; label?: string; className?: string; align?: "left" | "right" }) {
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  return (
    <>
      <button
        ref={btn}
        className={`icon-btn more-btn ${anchor ? "on" : ""} ${className}`}
        title={title}
        aria-label={label ?? title}
        aria-haspopup="menu"
        aria-expanded={!!anchor}
        onClick={(e) => setAnchor(anchor ? null : e.currentTarget.getBoundingClientRect())}
      >
        <MoreHorizontal size={18} />
      </button>
      {anchor && (
        <PopMenu
          items={items}
          anchor={anchor}
          align={align}
          onClose={(refocus) => {
            setAnchor(null);
            if (refocus) btn.current?.focus();
          }}
        />
      )}
    </>
  );
}

function PopMenu({ items, anchor, align, onClose }: { items: MenuEntry[]; anchor: DOMRect; align: "left" | "right"; onClose: (refocus: boolean) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const want = align === "right" ? anchor.right - r.width : anchor.left;
    const left = Math.max(8, Math.min(want, window.innerWidth - r.width - 8));
    const below = anchor.bottom + 6;
    const top = below + r.height > window.innerHeight - 8 ? Math.max(8, anchor.top - r.height - 6) : below;
    setPos({ left, top });
    el.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
  }, [anchor, align]);
  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose(false);
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose(true);
        return;
      }
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const list = [...(ref.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
      if (!list.length) return;
      const i = list.indexOf(document.activeElement as HTMLButtonElement);
      const next = e.key === "ArrowDown" ? (i + 1) % list.length : (i - 1 + list.length) % list.length;
      list[next].focus();
    };
    const away = () => onClose(false);
    window.addEventListener("mousedown", down);
    window.addEventListener("keydown", key, true);
    window.addEventListener("blur", away);
    window.addEventListener("resize", away);
    return () => {
      window.removeEventListener("mousedown", down);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("blur", away);
      window.removeEventListener("resize", away);
    };
  }, [onClose]);
  return (
    <div
      ref={ref}
      className="ctx-menu pop-menu"
      role="menu"
      style={pos ? { left: pos.left, top: pos.top } : { left: -9999, top: 0 }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) =>
        it === "sep" ? (
          <hr key={i} />
        ) : "info" in it ? (
          <div key={i} className="menu-info">
            {it.info}
          </div>
        ) : (
          <button
            key={i}
            role={it.checked === undefined ? "menuitem" : "menuitemcheckbox"}
            aria-checked={it.checked === undefined ? undefined : it.checked}
            className={it.danger ? "danger" : ""}
            disabled={it.disabled}
            title={it.title}
            onClick={() => {
              onClose(true);
              it.onClick();
            }}
          >
            <span className="menu-ic">{it.checked ? <Check size={14} /> : it.icon}</span>
            <span className="menu-label">{it.label}</span>
            {it.hint && <kbd className="menu-hint">{it.hint}</kbd>}
          </button>
        ),
      )}
    </div>
  );
}
