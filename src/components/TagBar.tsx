import { useEffect, useState } from "react";
import { MessageSquareQuote, Settings2, Tag as TagIcon, X } from "lucide-react";
import { api, filterActive, NO_FILTER, type SessionTags, type TagFilter } from "../api";
import { filterLabel, tagColor, toggleTag, toggleUntagged, useTags } from "../tags";

interface Props {
  sessionId: string;
  /** 대화가 바뀔 때마다 오른다 — 개수를 다시 잰다 */
  refreshKey: number;
  filter: TagFilter;
  onFilter: (f: TagFilter) => void;
  onManage: () => void;
  /** 고른 태그 요청들의 요지를 입력창 앞에 붙인다 */
  onContext: (text: string) => void;
  /** 칩 줄 숨기기(기본 숨김 — 더보기에서 켠다) */
  onClose?: () => void;
  toast: (m: string) => void;
}

/** 대화 위의 태그 칩 줄 — 이 세션에서 쓰인 태그만, 요청 수와 함께. 누르면 그 태그의 요청만 보이고 여럿 고를 수 있다. */
export function TagBar({ sessionId, refreshKey, filter, onFilter, onManage, onContext, onClose, toast }: Props) {
  const { ov, byId } = useTags();
  const [st, setSt] = useState<SessionTags | null>(null);
  useEffect(() => {
    api.tagSession(sessionId).then(setSt).catch(() => setSt(null));
  }, [sessionId, refreshKey, ov]);

  const active = filterActive(filter);
  const used = (st?.tags ?? []).filter(([id]) => byId.has(id));
  if (!st) return null;

  const attach = async () => {
    try {
      const text = await api.tagContext(sessionId, filter, filterLabel(filter, byId));
      if (!text) {
        toast("붙일 앞선 요청이 없습니다");
        return;
      }
      onContext(text);
      toast("입력창 앞에 앞선 요청들을 붙였습니다 — 이어서 지시를 쓰세요");
    } catch (e) {
      toast(String(e));
    }
  };

  return (
    <div className="tagbar" role="group" aria-label="태그로 거르기">
      <TagIcon size={14} className="tb-icon" />
      <button className={`tag-chip all ${active ? "" : "on"}`} onClick={() => onFilter(NO_FILTER)} title="태그로 거르지 않고 전체 보기">
        전체 <span className="n">{st.total}</span>
      </button>
      {used.map(([id, n]) => {
        const t = byId.get(id)!;
        const on = filter.tags.includes(id);
        return (
          <button key={id} className={`tag-chip ${on ? "on" : ""}`} onClick={() => onFilter(toggleTag(filter, id))} aria-pressed={on} title={`「${t.name}」 요청만 보기 — 여러 개를 고를 수 있습니다`}>
            <span className="tag-dot" style={{ background: tagColor(t) }} />
            {t.name} <span className="n">{n}</span>
          </button>
        );
      })}
      {st.untagged > 0 && (
        <button className={`tag-chip untagged ${filter.untagged ? "on" : ""}`} onClick={() => onFilter(toggleUntagged(filter))} aria-pressed={filter.untagged} title="태그가 하나도 없는 요청">
          미분류 <span className="n">{st.untagged}</span>
        </button>
      )}
      {filter.tags.length >= 2 && !filter.untagged && (
        <span className="seg tb-seg" role="group" aria-label="고른 태그의 결합">
          <button className={filter.all ? "" : "on"} onClick={() => onFilter({ ...filter, all: false })} title="고른 태그 중 하나라도 가진 요청">
            하나라도
          </button>
          <button className={filter.all ? "on" : ""} onClick={() => onFilter({ ...filter, all: true })} title="고른 태그를 모두 가진 요청">
            모두
          </button>
        </span>
      )}
      <span className="tb-spacer" />
      {active && (
        <button className="tb-btn" onClick={attach} title="고른 태그의 앞선 요청 첫 줄들을 새 지시 앞에 붙여, 세션이 어느 맥락을 두고 하는 말인지 알게 합니다">
          <MessageSquareQuote size={13} /> 이 맥락으로 이어 말하기
        </button>
      )}
      <button className="tb-btn" onClick={onManage} title="태그·자동 규칙 관리">
        <Settings2 size={13} /> 태그 관리
      </button>
      {onClose && (
        <button className="icon-btn tb-close" onClick={onClose} title="태그 줄 숨기기 — 거르기도 풉니다(더보기 › 태그로 거르기로 다시 켭니다)" aria-label="태그 줄 숨기기">
          <X size={14} />
        </button>
      )}
    </div>
  );
}
