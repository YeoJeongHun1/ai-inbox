import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Check, Plus, Tag as TagIcon, X } from "lucide-react";
import { api, filterActive, NO_FILTER, type TagFilter, type TagInfo, type TurnTag } from "../api";
import { tagColor, toggleTag, toggleUntagged, useTags } from "../tags";

export function TagDot({ tag }: { tag: TagInfo | undefined }) {
  return <span className="tag-dot" style={{ background: tagColor(tag) }} aria-hidden />;
}

/** 요청 옆 태그 배지. 모델 제안(ai)은 점선 — ✓ 로 받아들이고 ✕ 로 물리친다 */
export function TagBadges({ tags, onChanged, turnId }: { tags: TurnTag[]; turnId: number; onChanged: () => void }) {
  const { byId } = useTags();
  const shown = tags.filter((t) => byId.has(t.id));
  if (!shown.length) return null;
  const decide = async (tagId: number, accept: boolean) => {
    await api.tagAiDecide(turnId, tagId, accept).catch(() => {});
    onChanged();
  };
  return (
    <span className="tag-badges">
      {shown.map((t) => {
        const info = byId.get(t.id);
        return (
          <span key={t.id} className={`tag-badge ${t.state}`} title={t.state === "ai" ? "모델이 제안한 태그 — 받아들이면 표식이 됩니다" : t.state === "manual" ? "직접 붙인 태그" : "규칙으로 붙은 태그"}>
            <TagDot tag={info} />
            {info?.name}
            {t.state === "ai" && (
              <>
                <button className="tag-x" title="받아들이기" onClick={() => decide(t.id, true)}>
                  <Check size={11} />
                </button>
                <button className="tag-x" title="물리치기" onClick={() => decide(t.id, false)}>
                  <X size={11} />
                </button>
              </>
            )}
          </span>
        );
      })}
    </span>
  );
}

/** 검색 결과 등 읽기 전용 태그 이름들 */
export function TagLabels({ tags }: { tags: TurnTag[] }) {
  const { byId } = useTags();
  const on = tags.filter((t) => t.state !== "ai" && byId.has(t.id));
  if (!on.length) return null;
  return (
    <span className="tag-badges">
      {on.map((t) => (
        <span key={t.id} className="tag-badge">
          <TagDot tag={byId.get(t.id)} />
          {byId.get(t.id)?.name}
        </span>
      ))}
    </span>
  );
}

/** 검색·이력 화면용 태그 거르기 칩 — 전체 태그 목록에서 고른다(여럿 가능, 검색어와 함께 적용) */
export function TagFilterChips({ filter, onFilter, className }: { filter: TagFilter; onFilter: (f: TagFilter) => void; className?: string }) {
  const { ov } = useTags();
  const tags = ov?.tags ?? [];
  if (!tags.length) return null;
  return (
    <div className={`tagbar tf-chips ${className ?? ""}`} role="group" aria-label="태그로 거르기">
      <TagIcon size={14} className="tb-icon" />
      {tags.map((t) => {
        const on = filter.tags.includes(t.id);
        return (
          <button key={t.id} className={`tag-chip ${on ? "on" : ""}`} aria-pressed={on} onClick={() => onFilter(toggleTag(filter, t.id))} title={`「${t.name}」 태그가 붙은 요청만`}>
            <TagDot tag={t} />
            {t.name} <span className="n">{t.turns}</span>
          </button>
        );
      })}
      {ov && ov.untagged > 0 && (
        <button className={`tag-chip untagged ${filter.untagged ? "on" : ""}`} aria-pressed={filter.untagged} onClick={() => onFilter(toggleUntagged(filter))} title="태그가 하나도 없는 요청">
          미분류 <span className="n">{ov.untagged}</span>
        </button>
      )}
      {filterActive(filter) && (
        <button className="tb-btn" onClick={() => onFilter(NO_FILTER)}>
          거르기 풀기
        </button>
      )}
    </div>
  );
}

/** 목차·목록의 아주 작은 표식 — 이름은 마우스를 올리면 */
export function TagDots({ tags }: { tags: TurnTag[] }) {
  const { byId } = useTags();
  const on = tags.filter((t) => t.state !== "ai" && byId.has(t.id));
  if (!on.length) return null;
  return (
    <span className="tag-dots" title={on.map((t) => byId.get(t.id)?.name).join(" · ")}>
      {on.map((t) => (
        <TagDot key={t.id} tag={byId.get(t.id)} />
      ))}
    </span>
  );
}

/** 요청 하나의 태그를 고치는 작은 창 — 있는 태그를 켜고 끄고, 새 태그를 바로 만든다 */
export function TagPicker({
  turnId,
  sessionId,
  current,
  anchor,
  onClose,
  onChanged,
  toast,
}: {
  turnId: number;
  sessionId: string;
  current: TurnTag[];
  anchor: DOMRect;
  onClose: () => void;
  onChanged: () => void;
  toast: (m: string, a?: { label: string; run: () => void | Promise<void> }) => void;
}) {
  const { ov } = useTags();
  const [name, setName] = useState("");
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ top: number; left: number }>({ top: anchor.bottom + 4, left: Math.max(8, anchor.left - 140) });
  void sessionId;

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      top: Math.min(window.innerHeight - r.height - 8, anchor.bottom + 4),
      left: Math.max(8, Math.min(window.innerWidth - r.width - 8, anchor.left - 140)),
    });
  }, [anchor, ov]);
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  const stateOf = (id: number) => current.find((t) => t.id === id)?.state;
  const suggestRule = useCallback(
    async (tag: TagInfo) => {
      try {
        const c = await api.tagSuggestForTurn(turnId, tag.id);
        if (!c.length) return;
        toast(`「${tag.name}」 태그를 붙였습니다`, {
          label: `${c[0].pattern} 를 다룬 요청도 자동으로`,
          run: async () => {
            await api.tagRuleAdd(tag.id, "path", c[0].pattern, true);
            toast(`규칙을 추가했습니다 — 이 폴더(${c[0].pattern})를 다룬 요청에 「${tag.name}」 이 자동으로 붙습니다`);
          },
        });
      } catch {
        /* 제안은 덤 */
      }
    },
    [turnId, toast],
  );
  const toggle = async (tag: TagInfo) => {
    const on = !(stateOf(tag.id) && stateOf(tag.id) !== "ai");
    try {
      await api.turnTagSet([turnId], tag.id, on);
      onChanged();
      if (on) suggestRule(tag);
    } catch (e) {
      toast(String(e));
    }
  };
  const create = async () => {
    const n = name.trim();
    if (!n) return;
    try {
      const existing = ov?.tags.find((t) => t.name.toLowerCase() === n.toLowerCase());
      const id = existing ? existing.id : await api.tagCreate(n);
      await api.turnTagSet([turnId], id, true);
      setName("");
      onChanged();
    } catch (e) {
      toast(String(e));
    }
  };

  return (
    <div ref={ref} className="tag-picker" style={{ top: pos.top, left: pos.left }} role="dialog" aria-label="이 요청의 태그">
      <div className="tp-head">
        <TagIcon size={13} /> 이 요청의 태그
      </div>
      <div className="tp-list">
        {(ov?.tags ?? []).length === 0 && <p className="tp-empty">아직 태그가 없습니다. 아래에서 만드세요.</p>}
        {(ov?.tags ?? []).map((t) => {
          const st = stateOf(t.id);
          const on = !!st && st !== "ai";
          return (
            <button key={t.id} className={`tp-row ${on ? "on" : ""}`} onClick={() => toggle(t)} aria-pressed={on}>
              <span className="tp-check">{on && <Check size={12} />}</span>
              <TagDot tag={t} />
              <span className="tp-name">{t.name}</span>
              <span className="tp-src">{st === "auto" ? "자동" : st === "ai" ? "제안" : ""}</span>
            </button>
          );
        })}
      </div>
      <form
        className="tp-new"
        onSubmit={(e) => {
          e.preventDefault();
          create();
        }}
      >
        <input value={name} maxLength={24} placeholder="새 태그 이름" spellCheck={false} onChange={(e) => setName(e.target.value)} autoFocus />
        <button className="icon-btn" type="submit" title="만들고 붙이기" disabled={!name.trim()}>
          <Plus size={15} />
        </button>
      </form>
    </div>
  );
}
