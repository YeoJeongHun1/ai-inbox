import { useLayoutEffect, useRef, useState } from "react";

/**
 * 설정의 긴 설명 문단 — 기본 2줄만 보이고 둘째 줄 끝의 "자세히"로 펼친다(내용은 그대로, 접기만).
 * 동의·권한·경고 문구에는 쓰지 않는다(접힌 채 동의하게 두지 않는다) — 그런 곳은 <p className="set-note"> 그대로.
 */
export function Note({ small, children }: { small?: boolean; children: React.ReactNode }) {
  const ref = useRef<HTMLParagraphElement>(null);
  const [over, setOver] = useState(false);
  const [open, setOpen] = useState(false);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || open) return;
    const check = () => setOver(el.scrollHeight > el.clientHeight + 2);
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
  }, [children, open]);
  return (
    <div className={`note-wrap ${small ? "small" : ""}`}>
      <p ref={ref} className={`set-note ${small ? "small" : ""} ${open ? "" : "clamp"}`}>
        {children}
        {open && (
          <button className="note-more" aria-expanded onClick={() => setOpen(false)}>
            접기
          </button>
        )}
      </p>
      {over && !open && (
        <button className="note-more over" aria-expanded={false} onClick={() => setOpen(true)}>
          … 자세히
        </button>
      )}
    </div>
  );
}
