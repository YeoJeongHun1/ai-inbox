import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { CircleHelp, CircleSlash, ListOrdered, Loader2, PauseCircle, Search, X } from "lucide-react";
import { phoneReply, type OutlineRow, type TagFilter } from "../api";
import { matchFilter } from "../tags";
import { TagDots } from "./TagUi";
import { clock, plainLine } from "../format";

/** 오늘은 시:분, 그 전은 월/일 시:분 — 한 세션 안에서는 날짜보다 시각이 요청을 가른다 */
function rowTime(iso: string): string {
  const t = new Date(iso);
  const now = new Date();
  const today = t.getFullYear() === now.getFullYear() && t.getMonth() === now.getMonth() && t.getDate() === now.getDate();
  return today ? clock(iso) : `${t.getMonth() + 1}/${t.getDate()} ${clock(iso)}`;
}

/** 한 줄 높이 — 고정이라 수백 개도 보이는 줄만 그린다 */
const ROW = 56;
const OVERSCAN = 6;

/** 목차에 보일 요청 제목: 폰 머리말·빈 줄을 걷어 낸 첫 줄 */
export function outlineTitle(r: OutlineRow): string {
  const phone = phoneReply(r.head);
  const text = phone ? phone.body || phone.title || "" : r.head;
  const line = text
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  if (line) return line;
  if (r.slash_command) return r.slash_command;
  if (r.atts > 0) return `이미지 ${r.atts}장`;
  return "(빈 요청)";
}

/** 요약이 없을 때 둘째 줄 — 첫 줄만으로 구분이 안 되는 요청이 많다 */
function secondLine(r: OutlineRow): string {
  const phone = phoneReply(r.head);
  const lines = (phone ? phone.body : r.head)
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  return lines[1] ?? "";
}

function originTag(r: OutlineRow): string | null {
  if (phoneReply(r.head)) return "폰";
  if (r.origin === "peer") return r.peer_name ?? "다른 세션";
  if (r.origin === "channel") return "채널";
  return null;
}

function Mark({ r, unread }: { r: OutlineRow; unread: boolean }) {
  if (r.status === "running" || r.status === "background") return <Loader2 size={13} className="spin" aria-label="작업 중" />;
  if (r.status === "waiting") return <PauseCircle size={13} aria-label="승인·답변 대기" />;
  if (r.needs_input) return <CircleHelp size={13} aria-label="답이 필요해요" />;
  if (unread) return <span className="toc-dot" aria-label="안 읽음" />;
  if (r.status === "interrupted" || r.status === "stopped") return <CircleSlash size={13} aria-label="중단됨" />;
  return null;
}

interface Props {
  rows: OutlineRow[] | null;
  /** 화면에 보이는 요청(하나씩 보기면 고른 요청) */
  currentId: number | null;
  /** 대화에 이미 받아 둔 요청의 최신 읽음 상태 — 목차가 대화와 어긋나지 않게 */
  unreadOf: (r: OutlineRow) => boolean;
  /** 태그로 거르는 중이면 그 태그의 요청만 목록에 둔다(번호는 세션 안 순서 그대로) */
  tagFilter: TagFilter | null;
  single: boolean;
  onSingle: (on: boolean) => void;
  onPick: (id: number) => void;
  onClose: () => void;
}

export const Outline = memo(function Outline({ rows, currentId, unreadOf, tagFilter, single, onSingle, onPick, onClose }: Props) {
  const [query, setQuery] = useState("");
  const [unreadOnly, setUnreadOnly] = useState(false);
  const list = useRef<HTMLDivElement>(null);
  const [top, setTop] = useState(0);
  const [height, setHeight] = useState(400);

  const all = useMemo(() => rows ?? [], [rows]);
  const indexOf = useMemo(() => new Map(all.map((r, i) => [r.id, i + 1])), [all]);
  const unreadN = useMemo(() => all.filter(unreadOf).length, [all, unreadOf]);
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    return all.filter((r) => {
      if (unreadOnly && !unreadOf(r)) return false;
      if (tagFilter && !matchFilter(r.tags, tagFilter)) return false;
      if (!q) return true;
      return r.head.toLowerCase().includes(q) || (r.summary ?? "").toLowerCase().includes(q);
    });
  }, [all, query, unreadOnly, unreadOf, tagFilter]);
  useEffect(() => {
    if (unreadOnly && unreadN === 0) setUnreadOnly(false);
  }, [unreadOnly, unreadN]);

  useLayoutEffect(() => {
    const el = list.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    setHeight(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  // 지금 보는 요청이 목차 밖에 있으면 가운데로 — 목차를 처음 열 때도
  useEffect(() => {
    const el = list.current;
    if (!el || currentId == null) return;
    const i = shown.findIndex((r) => r.id === currentId);
    if (i < 0) return;
    const y = i * ROW;
    if (y < el.scrollTop || y + ROW > el.scrollTop + el.clientHeight) {
      el.scrollTop = Math.max(0, y - el.clientHeight / 2 + ROW / 2);
    }
  }, [currentId, shown]);

  const move = (d: number) => {
    if (!shown.length) return;
    const i = shown.findIndex((r) => r.id === currentId);
    const next = shown[Math.min(shown.length - 1, Math.max(0, (i < 0 ? (d > 0 ? -1 : shown.length) : i) + d))];
    if (next) onPick(next.id);
  };

  const first = Math.max(0, Math.floor(top / ROW) - OVERSCAN);
  const last = Math.min(shown.length, Math.ceil((top + height) / ROW) + OVERSCAN);

  return (
    <aside className="toc" aria-label="이 세션의 요청 목록">
      <header className="toc-head">
        <ListOrdered size={15} />
        <strong>요청 {all.length}</strong>
        <div className="seg" role="group" aria-label="보는 방식">
          <button className={single ? "" : "on"} onClick={() => onSingle(false)} title="대화를 이어서 보고, 고른 요청으로 이동">
            이어 보기
          </button>
          <button className={single ? "on" : ""} onClick={() => onSingle(true)} title="고른 요청 하나의 대화만 보기">
            하나씩
          </button>
        </div>
        <button className="icon-btn" title="닫기 (Esc)" onClick={onClose}>
          <X size={15} />
        </button>
      </header>
      <div className="toc-tools">
        <label className="toc-search">
          <Search size={13} />
          <input
            value={query}
            placeholder="요청 찾기"
            spellCheck={false}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.stopPropagation();
                if (query) setQuery("");
                else onClose();
              } else if (e.key === "Enter" && shown[0]) {
                onPick(shown[0].id);
              }
            }}
          />
        </label>
        <button
          className={`toc-chip ${unreadOnly ? "on" : ""}`}
          disabled={!unreadN && !unreadOnly}
          onClick={() => setUnreadOnly(!unreadOnly)}
          title="안 읽은 결과가 있는 요청만"
        >
          안 읽음 {unreadN}
        </button>
      </div>
      <div
        ref={list}
        className="toc-list"
        tabIndex={0}
        onScroll={(e) => setTop(e.currentTarget.scrollTop)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" || e.key === "j") {
            e.preventDefault();
            move(1);
          } else if (e.key === "ArrowUp" || e.key === "k") {
            e.preventDefault();
            move(-1);
          } else if (e.key === "Escape") {
            onClose();
          }
        }}
      >
        {rows == null ? (
          <p className="toc-empty">불러오는 중…</p>
        ) : shown.length === 0 ? (
          <p className="toc-empty">{query ? "찾는 요청이 없습니다." : unreadOnly ? "안 읽은 결과가 없습니다." : "요청이 없습니다."}</p>
        ) : (
          <div style={{ height: shown.length * ROW, position: "relative" }}>
            {shown.slice(first, last).map((r, k) => {
              const unread = unreadOf(r);
              const tag = originTag(r);
              const sub = r.summary ? plainLine(r.summary, 120) : secondLine(r);
              return (
                <button
                  key={r.id}
                  className={`toc-row ${r.id === currentId ? "cur" : ""} ${unread ? "unread" : ""}`}
                  style={{ top: (first + k) * ROW }}
                  onClick={() => onPick(r.id)}
                  title={outlineTitle(r)}
                >
                  <span className="toc-n">{indexOf.get(r.id)}</span>
                  <span className="toc-main">
                    <span className="toc-title">
                      {tag && <span className="toc-tag">{tag}</span>}
                      {outlineTitle(r)}
                      <TagDots tags={r.tags ?? []} />
                    </span>
                    {sub && <span className="toc-sub">{sub}</span>}
                  </span>
                  <span className="toc-side">
                    <time>{rowTime(r.prompt_at)}</time>
                    <Mark r={r} unread={unread} />
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </div>
    </aside>
  );
});
